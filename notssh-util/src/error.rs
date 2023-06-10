pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum ErrorKind {
    DB,
    NotFound,
    IO,
    BadRequest,
    Tonic,
    Arg,
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    description: String,
}

impl Error {
    pub fn new(kind: ErrorKind, description: String) -> Self {
        Self { kind, description }
    }

    pub fn db(description: impl Into<String>) -> Self {
        Self::new(ErrorKind::DB, description.into())
    }

    pub fn not_found(description: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, description.into())
    }

    pub fn io(description: impl Into<String>) -> Self {
        Self::new(ErrorKind::IO, description.into())
    }

    pub fn bad_request(description: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, description.into())
    }

    pub fn tonic(description: impl Into<String>) -> Self {
        Self::new(ErrorKind::Tonic, description.into())
    }

    pub fn arg(description: impl Into<String>) -> Self {
        Self::new(ErrorKind::Arg, description.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description)
    }
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for Error {
    fn from(value: tokio::sync::mpsc::error::SendError<T>) -> Self {
        Self::io(format!("{}", value))
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for Error {
    fn from(value: tokio::sync::oneshot::error::RecvError) -> Self {
        Self::io(format!("{}", value))
    }
}

impl From<Error> for tonic::Status {
    fn from(value: Error) -> Self {
        match value.kind {
            ErrorKind::NotFound => Self::not_found(value.description),
            ErrorKind::BadRequest => Self::invalid_argument(value.description),
            _ => Self::internal("internal error"),
        }
    }
}

impl From<tonic::metadata::errors::ToStrError> for Error {
    fn from(value: tonic::metadata::errors::ToStrError) -> Self {
        Self::bad_request(format!("{}", value))
    }
}

impl From<sqlx::Error> for Error {
    fn from(value: sqlx::Error) -> Self {
        match value {
            sqlx::Error::RowNotFound => Self::not_found("not found"),
            _ => Self::db(value.to_string()),
        }
    }
}

impl From<tonic::Status> for Error {
    fn from(value: tonic::Status) -> Self {
        Self::tonic(value.message())
    }
}

impl std::error::Error for Error {}
