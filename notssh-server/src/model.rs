use chrono::{DateTime, Duration, Utc};
use notssh_util::error;
use sqlx::{Executor, PgExecutor, Postgres, QueryBuilder};
use uuid::Uuid;

pub struct ListOptions {
    limit: Option<i64>,
    offset: Option<i64>,
}

impl ListOptions {
    pub fn new() -> Self {
        Self {
            limit: None,
            offset: None,
        }
    }

    pub fn _limit(self, limit: i64) -> Self {
        Self {
            limit: Some(limit),
            ..self
        }
    }

    pub fn _offset(self, offset: i64) -> Self {
        Self {
            offset: Some(offset),
            ..self
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct Client {
    pub id: String,
    pub address: Option<String>,
    pub connected: bool,
    pub last_online: DateTime<Utc>,
}

impl Client {
    pub fn new() -> Self {
        let id = Uuid::new_v4().to_string();
        Self {
            id,
            address: None,
            connected: false,
            last_online: Utc::now(),
        }
    }

    pub fn with_address(address: String) -> Self {
        let id = Uuid::new_v4().to_string();
        Self {
            id,
            address: Some(address),
            connected: false,
            last_online: Utc::now(),
        }
    }

    pub async fn get(
        id: &str,
        ex: impl PgExecutor<'_, Database = Postgres>,
    ) -> error::Result<Self> {
        sqlx::query_as("SELECT * FROM clients WHERE id = $1")
            .bind(id)
            .fetch_one(ex)
            .await
            .map_err(From::from)
    }

    pub async fn create(self, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query(
            "INSERT INTO clients (id, address, connected, last_online) VALUES ($1, $2, $3, $4)",
        )
        .bind(self.id)
        .bind(self.address)
        .bind(self.connected)
        .bind(self.last_online)
        .execute(ex)
        .await?;
        Ok(())
    }

    pub async fn update(self, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("UPDATE clients SET (connected, last_online) = ($1, $2) WHERE id = $3")
            .bind(self.connected)
            .bind(self.last_online)
            .bind(self.id)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn delete(id: &str, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("DELETE FROM clients WHERE id = $1")
            .bind(id)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn list(
        opts: ListOptions,
        ex: impl Executor<'_, Database = Postgres>,
    ) -> error::Result<Vec<Self>> {
        let mut builder = QueryBuilder::new("SELECT * FROM clients");
        if let Some(limit) = opts.limit {
            builder.push(" LIMIT ").push_bind(limit);
        }
        if let Some(offset) = opts.offset {
            builder.push(" OFFSET ").push_bind(offset);
        }
        builder
            .build_query_as()
            .fetch_all(ex)
            .await
            .map_err(From::from)
    }
}

#[derive(Debug, sqlx::Type)]
#[repr(i16)]
pub enum ActionCommand {
    Ping,
    Purge,
    Shell,
}

#[derive(Debug, sqlx::Type)]
#[repr(i16)]
pub enum ActionState {
    Pending,
    Running,
    Finished,
}

#[derive(Debug, sqlx::FromRow)]
pub struct Action {
    pub id: String,
    pub client_id: String,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub timeout: Option<i64>,
    pub command: ActionCommand,
    pub state: ActionState,
    pub error: Option<Vec<u8>>, // using bytes here, so it is possible to get files as response
    pub result: Option<Vec<u8>>, // same
}

impl Action {
    pub fn new(client_id: String, command: ActionCommand) -> Self {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        Self {
            id,
            client_id,
            created_at,
            started_at: None,
            timeout: None,
            command,
            state: ActionState::Pending,
            error: None,
            result: None,
        }
    }

    pub fn _with_timeout(client_id: String, command: ActionCommand, timeout: Duration) -> Self {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        Self {
            id,
            client_id,
            created_at,
            started_at: None,
            timeout: Some(timeout.num_seconds()),
            command,
            state: ActionState::Pending,
            error: None,
            result: None,
        }
    }

    pub async fn get(id: &str, ex: impl Executor<'_, Database = Postgres>) -> error::Result<Self> {
        sqlx::query_as("SELECT * FROM actions WHERE id = $1")
            .bind(id)
            .fetch_one(ex)
            .await
            .map_err(From::from)
    }

    pub async fn get_next(
        client_id: &str,
        ex: impl Executor<'_, Database = Postgres>,
    ) -> error::Result<Option<Self>> {
        sqlx::query_as(
            "SELECT * FROM actions WHERE client_id = $1 AND state = $2 ORDER BY created_at LIMIT 1",
        )
        .bind(client_id)
        .bind(ActionState::Pending as i16)
        .fetch_optional(ex)
        .await
        .map_err(From::from)
    }

    pub async fn create(self, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("INSERT INTO actions (id, client_id, created_at, started_at, timeout, command, state, error, result)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)")
            .bind(self.id)
            .bind(self.client_id)
            .bind(self.created_at)
            .bind(self.started_at)
            .bind(self.timeout)
            .bind(self.command)
            .bind(self.state)
            .bind(self.error)
            .bind(self.result)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn update(self, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("UPDATE actions SET (started_at, state, error, result) = ($1, $2, $3, $4) WHERE id = $5")
            .bind(self.started_at)
            .bind(self.state)
            .bind(self.error)
            .bind(self.result)
            .bind(self.id)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn delete(self, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("DELETE FROM actions WHERE id = $1")
            .bind(self.id)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn _list(
        opts: ListOptions,
        ex: impl Executor<'_, Database = Postgres>,
    ) -> error::Result<Vec<Self>> {
        let mut builder = QueryBuilder::new("SELECT * FROM actions ORDER by created_at");
        if let Some(limit) = opts.limit {
            builder.push(" LIMIT ").push_bind(limit);
        }
        if let Some(offset) = opts.offset {
            builder.push(" OFFSET ").push_bind(offset);
        }
        builder
            .build_query_as()
            .fetch_all(ex)
            .await
            .map_err(From::from)
    }

    pub async fn list_by_state(
        state: ActionState,
        opts: ListOptions,
        ex: impl Executor<'_, Database = Postgres>,
    ) -> error::Result<Vec<Self>> {
        let mut builder = QueryBuilder::new("SELECT * FROM actions WHERE state = ");
        builder.push_bind(state as i16).push(" ORDER by created_at");
        if let Some(limit) = opts.limit {
            builder.push(" LIMIT ").push_bind(limit);
        }
        if let Some(offset) = opts.offset {
            builder.push(" OFFSET ").push_bind(offset);
        }
        builder
            .build_query_as()
            .fetch_all(ex)
            .await
            .map_err(From::from)
    }
}

#[derive(sqlx::FromRow)]
pub struct PingCommand {
    id: String,
    pub data: String,
}

impl PingCommand {
    pub fn new(id: String, data: String) -> Self {
        Self { id, data }
    }

    pub async fn get(id: &str, ex: impl Executor<'_, Database = Postgres>) -> error::Result<Self> {
        sqlx::query_as("SELECT * FROM ping WHERE id = $1")
            .bind(id)
            .fetch_one(ex)
            .await
            .map_err(From::from)
    }

    pub async fn create(self, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("INSERT INTO ping (id, data) VALUES ($1, $2)")
            .bind(self.id)
            .bind(self.data)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn delete(id: &str, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("DELETE FROM ping WHERE id = $1")
            .bind(id)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn _list(
        opts: ListOptions,
        ex: impl Executor<'_, Database = Postgres>,
    ) -> error::Result<Vec<Self>> {
        let mut builder = QueryBuilder::new("SELECT * FROM ping");
        if let Some(limit) = opts.limit {
            builder.push(" LIMIT ").push_bind(limit);
        }
        if let Some(offset) = opts.offset {
            builder.push(" OFFSET ").push_bind(offset);
        }
        builder
            .build_query_as()
            .fetch_all(ex)
            .await
            .map_err(From::from)
    }
}

// TODO do I even need this?
#[derive(sqlx::FromRow)]
pub struct PurgeCommand {
    id: String,
}

impl PurgeCommand {
    pub fn new(id: String) -> Self {
        Self { id }
    }

    pub async fn _get(id: &str, ex: impl Executor<'_, Database = Postgres>) -> error::Result<Self> {
        sqlx::query_as("SELECT * FROM purge WHERE id = $1")
            .bind(id)
            .fetch_one(ex)
            .await
            .map_err(From::from)
    }

    pub async fn create(self, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("INSERT INTO purge (id) VALUES ($1)")
            .bind(self.id)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn delete(id: &str, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("DELETE FROM purge WHERE id = $1")
            .bind(id)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn _list(
        opts: ListOptions,
        ex: impl Executor<'_, Database = Postgres>,
    ) -> error::Result<Vec<Self>> {
        let mut builder = QueryBuilder::new("SELECT * FROM purge");
        if let Some(limit) = opts.limit {
            builder.push(" LIMIT ").push_bind(limit);
        }
        if let Some(offset) = opts.offset {
            builder.push(" OFFSET ").push_bind(offset);
        }
        builder
            .build_query_as()
            .fetch_all(ex)
            .await
            .map_err(From::from)
    }
}

#[derive(sqlx::FromRow)]
pub struct ShellCommand {
    id: String,
    pub cmd: String,
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
}

impl ShellCommand {
    pub fn new(id: String, cmd: String, args: Vec<String>, stdin: Vec<u8>) -> Self {
        Self {
            id,
            cmd,
            args,
            stdin,
        }
    }

    pub async fn get(id: &str, ex: impl Executor<'_, Database = Postgres>) -> error::Result<Self> {
        sqlx::query_as("SELECT * FROM shell WHERE id = $1")
            .bind(id)
            .fetch_one(ex)
            .await
            .map_err(From::from)
    }

    pub async fn create(self, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("INSERT INTO shell (id, cmd, args, stdin) VALUES ($1, $2, $3, $4)")
            .bind(self.id)
            .bind(self.cmd)
            .bind(self.args)
            .bind(self.stdin)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn delete(id: &str, ex: impl Executor<'_, Database = Postgres>) -> error::Result<()> {
        sqlx::query("DELETE FROM shell WHERE id = $1")
            .bind(id)
            .execute(ex)
            .await?;
        Ok(())
    }

    pub async fn _list(
        opts: ListOptions,
        ex: impl Executor<'_, Database = Postgres>,
    ) -> error::Result<Vec<Self>> {
        let mut builder = QueryBuilder::new("SELECT * FROM shell");
        if let Some(limit) = opts.limit {
            builder.push(" LIMIT ").push_bind(limit);
        }
        if let Some(offset) = opts.offset {
            builder.push(" OFFSET ").push_bind(offset);
        }
        builder
            .build_query_as()
            .fetch_all(ex)
            .await
            .map_err(From::from)
    }
}
