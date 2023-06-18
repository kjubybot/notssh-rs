use std::{
    fs::{self, File},
    net::SocketAddr,
    time::Duration,
};

use clap::Parser;
use log::LevelFilter;
use notssh::not_ssh_server::NotSshServer;
use notssh_cli::not_ssh_cli_server::NotSshCliServer;
use sqlx::{postgres::PgConnectOptions, PgPool};
use tokio::{
    net::UnixListener,
    signal::unix,
    sync::watch::{self, Receiver},
};
use tokio_stream::wrappers::UnixListenerStream;

use model::{ActionCommand, ActionState, ListOptions, PingCommand, ShellCommand};

mod api;
mod cli;
mod model;

// TTL do delete clients after 24 hours of inactivity
const CLIENT_TTL: Duration = Duration::from_secs(86400);

pub mod notssh {
    include!("../../gen/notssh.rs");

    impl action::Command {
        pub fn ping(data: String) -> Self {
            Self::Ping(action::Ping { ping: data })
        }

        pub fn purge() -> Self {
            Self::Purge(action::Purge {})
        }

        pub fn shell(cmd: String, args: Vec<String>, stdin: Vec<u8>) -> Self {
            Self::Shell(action::Shell { cmd, args, stdin })
        }
    }
}

pub mod notssh_cli {
    include!("../../gen/notssh_cli.rs");
}

#[derive(serde::Deserialize)]
struct DatabaseConfig {
    host: String,
    #[serde(default = "DatabaseConfig::default_port")]
    port: u16,
    username: String,
    password: String,
    database: String,
    #[serde(default = "DatabaseConfig::default_ssl")]
    use_ssl: bool,
}

impl DatabaseConfig {
    fn default_port() -> u16 {
        5432
    }

    fn default_ssl() -> bool {
        true
    }
}

#[derive(serde::Deserialize)]
struct Config {
    #[serde(default = "Config::default_address")]
    address: String,
    #[serde(default = "Config::default_port")]
    port: u16,
    #[serde(default = "Config::default_socket")]
    socket: String,
    db: DatabaseConfig,
}

impl Config {
    fn default_address() -> String {
        "0.0.0.0".into()
    }

    fn default_port() -> u16 {
        3144 // TODO choose a beautiful port
    }

    fn default_socket() -> String {
        "/run/notssh/cli.sock".into()
    }
}

#[derive(clap::Parser)]
struct Args {
    /// Config path
    #[arg(
        short = 'c',
        long = "config",
        default_value = "/etc/notssh/config.yaml"
    )]
    config: String,

    /// Log level
    #[arg(short = 'l', long = "log-level", default_value_t = LevelFilter::Warn)]
    log_level: LevelFilter,

    /// Perform database migration
    #[arg(short, long)]
    migrate: bool,
}

async fn gc(pool: PgPool, mut rx: Receiver<()>) {
    log::info!(target: "GC", "Starting GC");
    let mut i = tokio::time::interval(Duration::from_secs(3600)); // Sweep stale records once in an
                                                                  // hour
    loop {
        tokio::select! {
            _ = rx.changed() => break,
            _ = i.tick() => {
                let mut tx = match pool.begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        log::error!(target: "GC", "cannot begin transaction: {}", e);
                        continue;
                    }
                };
                let actions = match model::Action::list_by_state(ActionState::Finished, ListOptions::new(), &mut tx).await {
                    Ok(act) => act,
                    Err(e) => {
                        log::error!(target: "GC", "cannot list finished actions: {}", e);
                        continue;
                    },
                };
                log::debug!(target: "GC", "removing finished actions: {:?}", actions);
                for act in actions {
                    if let Err(e) = match act.command {
                        ActionCommand::Ping => PingCommand::delete(&act.id, &mut tx).await,
                        ActionCommand::Purge => Ok(()),
                        ActionCommand::Shell => ShellCommand::delete(&act.id, &mut tx).await,
                    } {
                        log::error!(target: "GC", "cannot delete command from database: {}", e);
                        continue;
                    }
                    if let Err(e) = act.delete(&mut tx).await {
                        log::error!(target: "GC", "cannot delete command from database: {}", e);
                        continue;
                    }
                }

                let clients = match model::Client::list_stale(CLIENT_TTL, &mut tx).await {
                    Ok(clients) => clients,
                    Err(e) => {
                        log::error!(target: "GC", "cannot list disconnected clients: {}", e);
                        continue;
                    }
                };
                log::debug!(target: "GC", "removing stale clients: {:?}", clients);
                for client in clients {
                        if let Err(e) = model::Client::delete(&client.id, &mut tx).await {
                            log::error!(target: "GC", "cannot delete stale client '{}' from database: {}", client.id, e);
                        }
                }

                if let Err(e) = tx.commit().await {
                    log::error!(target: "GC","cannot commit transaction: {}", e);
                    continue;
                }
            }
        }
    }

    log::info!(target: "GC", "Stopping GC");
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            log::error!(target: "GC", "cannot begin transaction: {}", e);
            return;
        }
    };

    let clients = match model::Client::list(ListOptions::new(), &mut tx).await {
        Ok(clients) => clients,
        Err(e) => {
            log::error!(target: "GC", "cannot list clients: {}", e);
            return;
        }
    };

    for mut client in clients.into_iter().filter(|c| c.connected) {
        client.connected = false;
        client.address = None;
        if let Err(e) = client.update(&mut tx).await {
            log::error!(target: "GC", "cannot update client in database: {}", e);
        }
    }

    if let Err(e) = tx.commit().await {
        log::error!(target: "GC", "cannot commit transaction: {}", e);
    }
}

// Helper func for graceful shutdown
async fn waiter(mut rx: Receiver<()>) {
    let _ = rx.changed().await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    env_logger::builder().filter_level(args.log_level).init();

    let fd = File::open(args.config)?;
    let cfg: Config = serde_yaml::from_reader(fd)?;

    let (tx, rx) = watch::channel(());

    let mut opts = PgConnectOptions::new()
        .host(&cfg.db.host)
        .port(cfg.db.port)
        .username(&cfg.db.username)
        .password(&cfg.db.password)
        .database(&cfg.db.database);
    opts = if cfg.db.use_ssl {
        opts.ssl_mode(sqlx::postgres::PgSslMode::Require)
    } else {
        opts.ssl_mode(sqlx::postgres::PgSslMode::Disable)
    };

    let pool = PgPool::connect_with(opts).await?;

    if args.migrate {
        log::info!("applying migrations");
        sqlx::migrate!("../migrations").run(&pool).await?;
        return Ok(());
    }

    log::info!("Starting GC");
    let gc_handle = tokio::spawn(gc(pool.clone(), rx.clone()));

    log::info!("Starting server");
    let service = api::Server::new(pool.clone());
    let addr = SocketAddr::new(cfg.address.parse()?, cfg.port);
    let server = tonic::transport::Server::builder().add_service(NotSshServer::new(service));

    let server_handle = tokio::spawn(server.serve_with_shutdown(addr, waiter(rx.clone())));

    log::info!("Starting control server");
    match fs::remove_file(&cfg.socket) {
        Ok(_) => Ok(()),
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(e),
        },
    }?;
    let cli_listener = UnixListener::bind(cfg.socket).unwrap();
    let cli_listener = UnixListenerStream::new(cli_listener);
    let cli_service = cli::CliServer::new(pool);
    let cli_server =
        tonic::transport::Server::builder().add_service(NotSshCliServer::new(cli_service));

    let cli_server_handle =
        tokio::spawn(cli_server.serve_with_incoming_shutdown(cli_listener, waiter(rx.clone())));

    let mut sigterm = unix::signal(unix::SignalKind::terminate())?;
    let mut sigint = unix::signal(unix::SignalKind::interrupt())?;

    log::info!("Ready");
    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
    };
    log::info!("Shutting down");
    tx.send(())?;
    let _ = tokio::join!(gc_handle, server_handle, cli_server_handle);

    Ok(())
}
