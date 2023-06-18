use std::{pin::Pin, time::Duration};

use crate::{
    model::{self, ShellCommand},
    notssh::{
        action::Command, not_ssh_server::NotSsh, res, Action, RegisterRequest, RegisterResponse,
        Res,
    },
};
use chrono::Utc;
use model::{ActionCommand, ActionState, Client, PingCommand};
use sqlx::PgPool;

const PING_INTERVAL: Duration = Duration::from_secs(60);

pub struct Server {
    db: PgPool,
}

impl Server {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    async fn poll_results(db: PgPool, client_id: String, mut stream: tonic::Streaming<Res>) {
        log::debug!("Begin polling results for {}", client_id);
        loop {
            let res = match stream.message().await {
                Ok(res) => match res {
                    Some(res) => res,
                    None => {
                        log::info!("no more results from {}", client_id);
                        break;
                    }
                },
                Err(e) => {
                    log::error!("cannot receive result from {}: {}", client_id, e);
                    break;
                }
            };
            let mut tx = match db.begin().await {
                Ok(tx) => tx,
                Err(e) => {
                    log::error!("cannot begin transaction: {}", e);
                    continue;
                }
            };

            let mut act = match model::Action::get(&res.id, &mut tx).await {
                Ok(act) => act,
                Err(e) => {
                    log::error!("cannot get action from database: {}", e);
                    continue;
                }
            };

            if let Some(r) = res.result.clone() {
                match r {
                    res::Result::Pong(pong) => {
                        if let Err(e) = PingCommand::delete(&res.id, &mut tx).await {
                            log::error!("cannot delete ping command from database: {}", e);
                            continue;
                        }
                        act.result = Some(pong.pong.into());
                    }
                    res::Result::Purge(_) => act.result = Some("purged".into()),
                    res::Result::Shell(shell) => {
                        if let Err(e) = ShellCommand::delete(&res.id, &mut tx).await {
                            log::error!("cannot delete shell command from database: {}", e);
                            continue;
                        }
                        if shell.code != 0 {
                            act.error = Some(shell.stderr);
                        } else {
                            act.result = Some(shell.stdout);
                        }
                    }
                }
            }
            act.state = ActionState::Finished;
            if let Err(e) = act.update(&mut tx).await {
                log::error!("cannot update action in database: {}", e);
                continue;
            }

            let mut client = match Client::get(&client_id, &mut tx).await {
                Ok(client) => client,
                Err(e) => {
                    log::error!("cannot get client '{}' from database: {}", client_id, e);
                    break;
                }
            };
            client.last_online = Utc::now();

            if let Err(e) = client.update(&mut tx).await {
                log::error!("cannot update client '{}' in database: {}", client_id, e);
                break;
            }

            if let Err(e) = tx.commit().await {
                log::error!("cannot commit transaction: {}", e);
            }

            if res.result.is_none() {
                log::error!(
                    "Client '{}' returned empty result for '{}'. Disconnecting",
                    client_id,
                    res.id
                );
                break;
            }
        }
        log::debug!("stopped polling results for {}", client_id);

        let mut tx = match db.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                log::error!("cannot begin transaction: {}", e);
                return;
            }
        };

        let mut client = match Client::get(&client_id, &mut tx).await {
            Ok(client) => client,
            Err(e) => {
                log::error!("cannot get client '{}' from database: {}", client_id, e);
                return;
            }
        };
        client.connected = false;
        client.address = None;
        client.last_online = Utc::now();

        if let Err(e) = client.update(&mut tx).await {
            log::error!(
                "cannot update disconnected client '{}' in database: {}",
                client_id,
                e
            );
            return;
        }

        if let Err(e) = tx.commit().await {
            log::error!("cannot commit transaction: {}", e);
        }
    }

    async fn ping_client(pool: PgPool, client_id: String) {
        let mut i = tokio::time::interval(PING_INTERVAL);
        loop {
            i.tick().await;

            let mut tx = match pool.begin().await {
                Ok(tx) => tx,
                Err(e) => {
                    log::error!(target: "HC", "cannot begin transaction: {}", e);
                    break;
                }
            };

            let client = match Client::get(&client_id, &mut tx).await {
                Ok(client) => client,
                Err(e) => {
                    log::error!(target: "HC", "cannot get client from database: {}", e);
                    break;
                }
            };
            if !client.connected {
                log::info!(target: "HC", "client '{}' is not connected, stopping", client.id);
                break;
            }

            let act = model::Action::new(client.id, ActionCommand::Ping);
            let cmd = PingCommand::new(act.id.clone(), String::from("ping"));

            if let Err(e) = act.create(&mut tx).await {
                log::error!(target: "HC", "cannot create action in database: {}", e);
                break;
            }

            if let Err(e) = cmd.create(&mut tx).await {
                log::error!(target: "HC", "cannot create ping command in database: {}", e);
                break;
            }

            if let Err(e) = tx.commit().await {
                log::error!(target: "HC", "cannot commit transaction: {}", e);
                break;
            }
        }
    }
}

#[tonic::async_trait]
impl NotSsh for Server {
    type PollStream = Pin<
        Box<
            dyn futures_core::Stream<Item = std::result::Result<Action, tonic::Status>>
                + Send
                + 'static,
        >,
    >;

    async fn register(
        &self,
        request: tonic::Request<RegisterRequest>,
    ) -> std::result::Result<tonic::Response<RegisterResponse>, tonic::Status> {
        log::info!("Server: Register");

        let client = match request.remote_addr() {
            Some(addr) => Client::with_address(format!("{}", addr)),
            None => Client::new(),
        };
        let id = client.id.clone();
        let mut tx = match self.db.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                log::error!("cannot begin transaction: {}", e);
                return Err(tonic::Status::internal("internal error"));
            }
        };

        if let Err(e) = client.create(&mut tx).await {
            log::error!("cannot insert client in database: {}", e);
            return Err(e.into());
        }

        if let Err(e) = tx.commit().await {
            log::error!("cannot commit transaction: {}", e);
            return Err(tonic::Status::internal("internal error"));
        }

        log::info!("Server: new client registered. ID {}", id);
        Ok(tonic::Response::new(RegisterResponse { id }))
    }

    async fn poll(
        &self,
        request: tonic::Request<tonic::Streaming<Res>>,
    ) -> std::result::Result<tonic::Response<Self::PollStream>, tonic::Status> {
        log::info!("Server: Poll");

        let metadata = request.metadata();
        let id = metadata
            .get("x-client-id")
            .ok_or(tonic::Status::invalid_argument(
                "x-client-id header is missing",
            ))?;
        let id = id.to_str().unwrap().to_owned();

        let mut tx = match self.db.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                log::error!("cannot begin transaction: {}", e);
                return Err(tonic::Status::internal("internal error"));
            }
        };

        let mut client = match Client::get(&id, &mut tx).await {
            Ok(client) => client,
            Err(e) => {
                log::error!("cannot get client from database: {}", e);
                return Err(e.into());
            }
        };
        let client_id = client.id.clone();

        if client.connected {
            return Err(tonic::Status::invalid_argument(
                "client is already connected",
            ));
        }
        client.connected = true;
        client.last_online = Utc::now();
        if let Some(addr) = request.remote_addr() {
            client.address = Some(addr.to_string());
        }

        if let Err(e) = client.update(&mut tx).await {
            log::error!("cannot update client in database: {}", e);
            return Err(e.into());
        }

        if let Err(e) = tx.commit().await {
            log::error!("cannot commit transaction: {}", e);
            return Err(tonic::Status::internal("internal error"));
        }

        log::info!("Server: client with ID '{}' connected", id);
        tokio::spawn(Self::ping_client(self.db.clone(), client_id));

        let res = request.into_inner();
        let db = self.db.clone();
        tokio::spawn(Self::poll_results(db.clone(), id.clone(), res));
        let output = async_stream::try_stream! {
            loop {
                let mut tx = match db.begin().await {
                    Ok(tx) => tx,
                    Err(e) => {
                        log::error!("cannot begin transaction: {}", e);
                        continue;
                    },
                };

                let client = model::Client::get(&id, &mut tx).await?;
                // a hack to return error from try_stream macro, which only supports '?'
                client.connected.then_some(()).ok_or(tonic::Status::cancelled("client disconnected"))?;

                let mut act = match model::Action::get_next(&id, &mut tx).await? {
                    Some(act) => act,
                    None => {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    },
                };

                let peer_act = match act.command {
                    ActionCommand::Ping => {
                        let ping_cmd = PingCommand::get(&act.id, &mut tx).await?;
                        Action {
                            id: act.id.clone(),
                            command: Some(Command::ping(ping_cmd.data)),
                        }
                    },
                    ActionCommand::Purge => {
                        Action {
                            id: act.id.clone(),
                            command: Some(Command::purge()),
                        }
                    },
                    ActionCommand::Shell => {
                        let shell_cmd = ShellCommand::get(&act.id, &mut tx).await?;
                        Action {
                            id: act.id.clone(),
                            command: Some(Command::shell(shell_cmd.cmd, shell_cmd.args, shell_cmd.stdin)),
                        }
                    }
                };
                act.started_at = Some(Utc::now());
                act.state = ActionState::Running;
                act.update(&mut tx).await?;

                if let Err(e) = tx.commit().await {
                    log::error!("cannot commit transaction: {}", e);
                    continue;
                }

                yield peer_act;
            }
        };

        Ok(tonic::Response::new(Box::pin(output) as Self::PollStream))
    }
}
