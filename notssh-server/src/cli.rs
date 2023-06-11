use std::time::Duration;

use crate::notssh_cli::{
    list_response, not_ssh_cli_server::NotSshCli, ListRequest, ListResponse, PingRequest,
    PingResponse, PurgeRequest, PurgeResponse, ShellRequest, ShellResponse,
};
use notssh_util::error;
use sqlx::PgPool;

use crate::model::{self, ActionCommand, ActionState, Client, ListOptions};

pub struct CliServer {
    db: PgPool,
}

impl CliServer {
    const PING_TIMEOUT: Duration = Duration::from_secs(10);
    const PURGE_TIMEOUT: Duration = Duration::from_secs(60);
    const SHELL_TIMEOUT: Duration = Duration::from_secs(3600);

    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[tonic::async_trait]
impl NotSshCli for CliServer {
    async fn list(
        &self,
        _request: tonic::Request<ListRequest>,
    ) -> std::result::Result<tonic::Response<ListResponse>, tonic::Status> {
        log::info!("Control server: List");

        let mut tx = match self.db.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                log::error!("cannot begin transaction: {}", e);
                return Err(tonic::Status::internal("internal error"));
            }
        };

        let clients = match Client::list(ListOptions::new(), &mut tx).await {
            Ok(c) => c,
            Err(e) => {
                log::error!("cannot get users from database: {}", e);
                return Err(e.into());
            }
        };

        if let Err(e) = tx.commit().await {
            log::error!("cannot commit transaction: {}", e);
            return Err(tonic::Status::internal("internal error"));
        }

        let clients = clients
            .into_iter()
            .map(|c| list_response::Client {
                id: c.id,
                address: c.address.unwrap_or(String::from("-")),
                connected: c.connected,
            })
            .collect();
        Ok(tonic::Response::new(ListResponse { clients }))
    }

    async fn ping(
        &self,
        request: tonic::Request<PingRequest>,
    ) -> std::result::Result<tonic::Response<PingResponse>, tonic::Status> {
        log::info!("Control server: Ping");

        let request = request.into_inner();
        let mut tx = match self.db.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                log::error!("cannot being transaction: {}", e);
                return Err(tonic::Status::internal("internal error"));
            }
        };
        let client = match Client::get(&request.id, &mut tx).await {
            Ok(c) => c,
            Err(e) => {
                log::error!("cannot get client from database: {}", e);
                return Err(e.into());
            }
        };

        let act = model::Action::new(client.id, ActionCommand::Ping);
        let cmd = model::PingCommand::new(act.id.clone(), String::from("ping"));
        let id = act.id.clone();
        let msg = cmd.data.clone();

        if let Err(e) = act.create(&mut tx).await {
            log::error!("cannot create action in database: {}", e);
            return Err(e.into());
        }

        if let Err(e) = cmd.create(&mut tx).await {
            log::error!("cannot create ping command in database: {}", e);
            return Err(e.into());
        }

        if let Err(e) = tx.commit().await {
            log::error!("cannot commit transaction: {}", e);
            return Err(tonic::Status::internal("internal error"));
        }

        let act =
            match tokio::time::timeout(Self::PING_TIMEOUT, wait_for_result(&id, self.db.clone()))
                .await
            {
                Ok(r) => match r {
                    Ok(r) => r,
                    Err(e) => {
                        log::error!("cannot get action from database: {}", e);
                        return Err(tonic::Status::internal("internal error"));
                    }
                },
                Err(e) => {
                    log::error!("gave up waiting after for result after {}", e);
                    return Err(tonic::Status::deadline_exceeded("action timeout"));
                }
            };

        if let Some(result) = act.result {
            let result = String::from_utf8(result)
                .map_err(|_| error::Error::bad_request("cannot parse ping response"))?;
            if result == msg {
                return Ok(tonic::Response::new(PingResponse {}));
            }
        }

        Err(tonic::Status::unavailable(
            "could not receive ping from client",
        ))
    }

    async fn purge(
        &self,
        request: tonic::Request<PurgeRequest>,
    ) -> std::result::Result<tonic::Response<PurgeResponse>, tonic::Status> {
        log::info!("Control server: Purge");

        let request = request.into_inner();
        let mut tx = match self.db.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                log::error!("cannot begin transaction: {}", e);
                return Err(tonic::Status::internal("internal error"));
            }
        };
        let client = match Client::get(&request.id, &mut tx).await {
            Ok(c) => c,
            Err(e) => {
                log::error!("cannot get client from database: {}", e);
                return Err(e.into());
            }
        };

        let act = model::Action::new(client.id, ActionCommand::Purge);
        let id = act.id.clone();

        if let Err(e) = act.create(&mut tx).await {
            log::error!("cannot create action in database: {}", e);
            return Err(e.into());
        }

        if let Err(e) = tx.commit().await {
            log::error!("cannot commit transaction: {}", e);
            return Err(tonic::Status::internal("internal error"));
        }

        let act =
            match tokio::time::timeout(Self::PURGE_TIMEOUT, wait_for_result(&id, self.db.clone()))
                .await
            {
                Ok(r) => match r {
                    Ok(r) => r,
                    Err(e) => {
                        log::error!("cannot get action from database: {}", e);
                        return Err(tonic::Status::internal("internal error"));
                    }
                },
                Err(e) => {
                    log::error!("gave up waiting after for result after {}", e);
                    return Err(tonic::Status::deadline_exceeded("action timeout"));
                }
            };

        if let Some(result) = act.result {
            let result = String::from_utf8(result)
                .map_err(|_| error::Error::bad_request("cannot parse purge response"))?;
            if result == "purged" {
                return Ok(tonic::Response::new(PurgeResponse { text: result }));
            }
        }

        Err(tonic::Status::unavailable(
            "could not receive purge result from client",
        ))
    }

    async fn shell(
        &self,
        request: tonic::Request<ShellRequest>,
    ) -> std::result::Result<tonic::Response<ShellResponse>, tonic::Status> {
        log::info!("Control server: Shell");

        let request = request.into_inner();
        let mut tx = match self.db.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                log::error!("cannot begin transaction: {}", e);
                return Err(tonic::Status::internal("internal error"));
            }
        };
        let client = match Client::get(&request.id, &mut tx).await {
            Ok(c) => c,
            Err(e) => {
                log::error!("cannot get client from database: {}", e);
                return Err(e.into());
            }
        };

        let act = model::Action::new(client.id, ActionCommand::Shell);
        let cmd =
            model::ShellCommand::new(act.id.clone(), request.cmd, request.args, request.stdin);
        let id = act.id.clone();

        if let Err(e) = act.create(&mut tx).await {
            log::error!("cannot create action in database: {}", e);
            return Err(e.into());
        }

        if let Err(e) = cmd.create(&mut tx).await {
            log::error!("cannot create purge command in database: {}", e);
            return Err(e.into());
        }

        if let Err(e) = tx.commit().await {
            log::error!("cannot commit transaction: {}", e);
            return Err(tonic::Status::internal("internal error"));
        }

        let act =
            match tokio::time::timeout(Self::SHELL_TIMEOUT, wait_for_result(&id, self.db.clone()))
                .await
            {
                Ok(r) => match r {
                    Ok(r) => r,
                    Err(e) => {
                        log::error!("cannot get action from database: {}", e);
                        return Err(tonic::Status::internal("internal error"));
                    }
                },
                Err(e) => {
                    log::error!("gave up waiting for result after {}", e);
                    return Err(tonic::Status::deadline_exceeded("action timeout"));
                }
            };

        if let Some(result) = act.result {
            return Ok(tonic::Response::new(ShellResponse {
                stdout: result,
                stderr: Vec::new(),
            }));
        }

        if let Some(error) = act.error {
            return Ok(tonic::Response::new(ShellResponse {
                stdout: Vec::new(),
                stderr: error,
            }));
        }

        Err(tonic::Status::unavailable(
            "cannot receive shell result from client",
        ))
    }
}

async fn wait_for_result(id: &str, pool: PgPool) -> error::Result<model::Action> {
    let mut i = tokio::time::interval(Duration::from_secs(2));
    let act = loop {
        i.tick().await;

        let mut tx = match pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                log::error!("cannot begin transaction: {}", e);
                continue;
            }
        };
        let act = match model::Action::get(&id, &mut tx).await {
            Ok(act) => act,
            Err(e) => {
                log::error!("cannot get action from database: {}", e);
                continue;
            }
        };
        if let ActionState::Finished = act.state {
            break act;
        }
    };
    Ok(act)
}
