use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use notssh_cli::{
    not_ssh_cli_client::NotSshCliClient, ListRequest, PingRequest, PingResponse, PurgeRequest,
    PurgeResponse, ShellRequest, ShellResponse,
};
use notssh_util::error;
use tokio::{
    io::AsyncWriteExt,
    net::UnixStream,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
    task::JoinSet,
};
use tonic::transport::{Channel, Uri};
use tower::service_fn;

pub mod notssh_cli {
    include!("../../gen/notssh_cli.rs");
}

#[derive(clap::Parser)]
struct Cli {
    /// Client ids separated by commas (id1,id2,...)
    #[arg(long)]
    ids: Option<String>,

    /// Number of parallel executors
    #[arg(short, long, default_value_t = 4)]
    parallel: u8,

    #[command(subcommand)]
    command: Command,

    /// Unix socket to connect to
    #[arg(short = 'u', long, default_value = "/run/notssh/cli.sock")]
    socket: String,
}

#[derive(clap::Subcommand)]
enum Command {
    /// List connected clients
    List,
    /// Send ping to client
    Ping,
    /// Purge all traces from client WARNING! This action is irreversable!
    Purge,
    /// Execute shell command on client
    Shell(ShellArgs),
}

#[derive(clap::Args)]
struct ShellArgs {
    /// Command to execute
    cmd: String,
    /// Command arguments
    args: Vec<String>,
    /// Path to file to use as input for the command
    #[arg(long)]
    stdin: Option<String>,
    /// Annotate command output with clients' ids
    #[arg(short, long, default_value_t = false)]
    annotate: bool,
}

#[derive(Debug)]
enum ExecReq {
    Ping(PingRequest),
    Purge(PurgeRequest),
    Shell(ShellRequest),
}

type TonicResult<T> = Result<tonic::Response<T>, tonic::Status>;

#[derive(Debug)]
enum ExecRes {
    Ping(TonicResult<PingResponse>, String),
    Purge(TonicResult<PurgeResponse>, String),
    Shell(TonicResult<ShellResponse>, String),
}

async fn executor(
    mut client: NotSshCliClient<tonic::transport::Channel>,
    rx: Arc<Mutex<UnboundedReceiver<ExecReq>>>,
    tx: UnboundedSender<ExecRes>,
) {
    while let Some(exec) = rx.lock().await.recv().await {
        match exec {
            ExecReq::Ping(req) => {
                let id = req.id.clone();
                let res = client.ping(req).await;
                tx.send(ExecRes::Ping(res, id)).unwrap();
            }
            ExecReq::Purge(req) => {
                let id = req.id.clone();
                let res = client.purge(req).await;
                tx.send(ExecRes::Purge(res, id)).unwrap();
            }
            ExecReq::Shell(req) => {
                let id = req.id.clone();
                let res = client.shell(req).await;
                tx.send(ExecRes::Shell(res, id)).unwrap();
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();
    let sock_path = cli.socket.clone();
    let uri = Uri::builder()
        .scheme("unix")
        .authority(".")
        .path_and_query(&sock_path)
        .build()?;
    let chan = Channel::builder(uri)
        .connect_with_connector(service_fn(|uri: Uri| {
            let path = uri.path().to_owned();
            UnixStream::connect(path)
        }))
        .await
        .with_context(|| format!("cannot connect to {}", sock_path))?;

    let mut client = NotSshCliClient::new(chan);

    if let Command::List = cli.command {
        let req = tonic::Request::new(ListRequest {});
        let res = client
            .list(req)
            .await
            .map_err(|e| error::Error::from(e))?
            .into_inner();
        println!("{:<36} CONNECTED", "CLIENT ID");
        for client in res.clients {
            println!("{:<36} {}", client.id, client.connected);
        }
        return Ok(());
    }

    let ids: Vec<String> = cli
        .ids
        .ok_or(error::Error::arg("ids required for this command"))
        .map(|ids| {
            ids.split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
                .collect()
        })?;

    let (req_tx, req_rx) = mpsc::unbounded_channel();
    let (res_tx, mut res_rx) = mpsc::unbounded_channel();
    let req_rx = Arc::new(Mutex::new(req_rx));

    let mut set = JoinSet::new();
    for _ in 0..cli.parallel {
        set.spawn(executor(client.clone(), req_rx.clone(), res_tx.clone()));
    }
    drop(req_rx);
    drop(res_tx);

    match cli.command {
        Command::List => unreachable!(),
        Command::Ping => {
            for id in ids {
                let req = PingRequest { id };
                req_tx.send(ExecReq::Ping(req)).unwrap();
            }
            drop(req_tx);
            while let Some(ExecRes::Ping(res, id)) = res_rx.recv().await {
                match res {
                    Ok(_) => println!("{} Ping OK", id),
                    Err(e) => println!("{} Ping failed ({})", id, e),
                }
            }
        }
        Command::Purge => {
            for id in ids {
                let req = PurgeRequest { id };
                req_tx.send(ExecReq::Purge(req)).unwrap();
            }
            drop(req_tx);
            while let Some(ExecRes::Purge(res, id)) = res_rx.recv().await {
                match res {
                    Ok(_) => println!("{} Purged", id),
                    Err(e) => println!("{} Purge failed ({})", id, e),
                }
            }
        }
        Command::Shell(args) => {
            let stdin = args.stdin.map_or(Ok(Vec::new()), |path| {
                std::fs::read(&path).with_context(|| format!("cannot read input file {}", path))
            })?;
            for id in ids {
                let req = ShellRequest {
                    id,
                    cmd: args.cmd.clone(),
                    args: args.args.clone(),
                    stdin: stdin.clone(),
                };
                req_tx.send(ExecReq::Shell(req)).unwrap();
            }
            drop(req_tx);
            while let Some(ExecRes::Shell(res, id)) = res_rx.recv().await {
                let mut stdout = tokio::io::stdout();
                let mut stderr = tokio::io::stderr();
                if args.annotate {
                    let a = format!("\n{}\n{:-<36}\n", id, "");
                    stdout
                        .write_all(a.as_bytes())
                        .await
                        .with_context(|| "cannot write to stdout")?;
                }
                match res {
                    Ok(res) => {
                        let res = res.into_inner();
                        stdout
                            .write_all(&res.stdout)
                            .await
                            .with_context(|| "cannot write response to stdout")?;
                        stderr
                            .write_all(&res.stderr)
                            .await
                            .with_context(|| "cannot write response to stderr")?;
                    }
                    Err(e) => println!("Shell failed ({})", e),
                }
                if args.annotate {
                    let a = format!("\n{:-<36}\n", "");
                    stdout
                        .write_all(a.as_bytes())
                        .await
                        .with_context(|| "cannot write to stdout")?;
                }
            }
        }
    }

    while let Some(_) = set.join_next().await {}
    Ok(())
}
