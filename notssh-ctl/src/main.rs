use anyhow::Context;
use clap::Parser;
use notssh_cli::{
    not_ssh_cli_client::NotSshCliClient, ListRequest, PingRequest, PurgeRequest, ShellRequest,
};
use notssh_util::error;
use tokio::{io::AsyncWriteExt, net::UnixStream};
use tonic::transport::{Channel, Uri};
use tower::service_fn;

pub mod notssh_cli {
    include!("../../gen/notssh_cli.rs");
}

#[derive(clap::Parser)]
struct Cli {
    /// ID of client
    #[arg(long)]
    id: Option<String>,

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

    match cli.command {
        Command::List => {
            let req = tonic::Request::new(ListRequest {});
            let res = client
                .list(req)
                .await
                .map_err(|e| error::Error::from(e))?
                .into_inner();
            println!("{:<36} ADDRESS", "CLIENT ID");
            for client in res.clients {
                println!("{:<36} {} {}", client.id, client.address, client.connected);
            }
        }
        Command::Ping => {
            let id = cli.id.ok_or(error::Error::arg("id is required"))?;
            let req = tonic::Request::new(PingRequest { id });
            client.ping(req).await.map_err(|e| error::Error::from(e))?;
            println!("Ping OK");
        }
        Command::Purge => {
            let id = cli.id.ok_or(error::Error::arg("id is required"))?;
            let req = tonic::Request::new(PurgeRequest { id });
            client.purge(req).await.map_err(|e| error::Error::from(e))?;
            println!("Purged");
        }
        Command::Shell(args) => {
            let id = cli.id.ok_or(error::Error::arg("id is required"))?;
            let stdin = args.stdin.map_or(Ok(Vec::new()), |path| {
                std::fs::read(&path).with_context(|| format!("cannot read input file {}", path))
            })?;
            let req = tonic::Request::new(ShellRequest {
                id,
                cmd: args.cmd,
                args: args.args,
                stdin,
            });
            let res = client
                .shell(req)
                .await
                .map_err(|e| error::Error::from(e))?
                .into_inner();
            tokio::io::stdout()
                .write_all(&res.stdout)
                .await
                .with_context(|| "cannot write response to stdout")?;
            tokio::io::stderr()
                .write_all(&res.stderr)
                .await
                .with_context(|| "cannot write response to stderr")?;
        }
    }

    Ok(())
}
