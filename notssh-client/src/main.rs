use std::{
    str::FromStr,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use clap::Parser;
use log::LevelFilter;
use notssh::{not_ssh_client::NotSshClient, res, RegisterRequest, Res};
use notssh_util::error;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::transport::Endpoint;

mod shell;

pub mod notssh {
    include!("../../gen/notssh.rs");

    impl res::Result {
        pub fn pong(data: String) -> Self {
            Self::Pong(res::Pong { pong: data })
        }

        pub fn purge() -> Self {
            Self::Purge(res::Purge {})
        }

        pub fn shell(code: i32, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
            Self::Shell(res::Shell {
                code,
                stdout,
                stderr,
            })
        }
    }
}

struct AuthSource {
    id: Option<String>,
}

impl AuthSource {
    fn new() -> Self {
        Self { id: None }
    }

    fn with_id(id: String) -> Self {
        Self { id: Some(id) }
    }
}

#[derive(clap::Parser)]
struct Args {
    /// Server endpoint (example: http://192.168.1.2:3144)
    #[arg(short, long)]
    endpoint: String,

    /// Path to id file
    #[arg(short = 'c', long, default_value = "~/.notssh_id")]
    client_id: String,

    /// Log level
    #[arg(short = 'l', long = "log-level", default_value_t = LevelFilter::Warn)]
    log_level: LevelFilter,
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let args = Args::parse();
    env_logger::builder().filter_level(args.log_level).init();

    let mut id_path = args.client_id;
    if id_path.starts_with("~") {
        id_path = match std::env::var("HOME") {
            Ok(path) => path,
            Err(_) => ".".to_owned(),
        } + "/.notssh";
    }

    let auth_source = match std::fs::read_to_string(&id_path) {
        Ok(id) => AuthSource::with_id(id),
        Err(_) => AuthSource::new(),
    };
    let auth_source = Arc::new(Mutex::new(auth_source));
    let chan = Endpoint::from_str(&args.endpoint)
        .unwrap()
        .connect()
        .await
        .with_context(|| "cannot connect")?;

    let a = auth_source.clone();
    let mut client = NotSshClient::with_interceptor(chan, move |mut req: tonic::Request<()>| {
        let lock = a.lock().unwrap();
        if let Some(id) = lock.id.as_ref() {
            let id = match FromStr::from_str(id) {
                Ok(v) => Ok(v),
                Err(_) => Err(tonic::Status::invalid_argument("client id is not valid")),
            }?;
            req.metadata_mut().insert("x-client-id", id);
        }
        Ok(req)
    });

    if auth_source.lock().unwrap().id.is_none() {
        let req = tonic::Request::new(RegisterRequest {});
        let res = client
            .register(req)
            .await
            .map_err(|e| error::Error::from(e))
            .with_context(|| "cannot register client")?
            .into_inner();
        let _ = std::fs::write(id_path, &res.id)?;
        auth_source.lock().unwrap().id = Some(res.id);
    }

    let (tx, rx) = unbounded_channel();
    let req = UnboundedReceiverStream::new(rx);
    let mut res = client
        .poll(req)
        .await
        .map_err(|e| error::Error::from(e))?
        .into_inner();

    while let Some(act) = res.message().await? {
        let cmd = act
            .command
            .ok_or(error::Error::bad_request("action contains no command"))?;
        let res = match cmd {
            notssh::action::Command::Ping(ping) => Res {
                id: act.id,
                result: Some(res::Result::pong(ping.ping)),
            },
            notssh::action::Command::Purge(_) => Res {
                id: act.id,
                result: Some(res::Result::purge()),
            },
            notssh::action::Command::Shell(shell) => {
                let runner = shell::Runner::new(shell.cmd, shell.args, shell.stdin);
                let out = runner.run().await?;
                let code = match out.status.code() {
                    Some(c) => c,
                    None => -1,
                };
                let res = res::Result::shell(code, out.stdout, out.stderr);
                Res {
                    id: act.id,
                    result: Some(res),
                }
            }
        };
        tx.send(res)?;
    }
    Ok(())
}
