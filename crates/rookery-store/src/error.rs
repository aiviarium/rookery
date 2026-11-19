//! What can go wrong in the store, and how it reaches a caller.

use rookery_proto::RpcError;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub enum Error {
    NotFound(String),
    Conflict(String),
    Invalid(String),
    /// Something the store needs is not set up, such as the sealing key.
    Unavailable(String),
    Database(sqlx::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound(m) | Error::Conflict(m) | Error::Invalid(m) | Error::Unavailable(m) => {
                f.write_str(m)
            }
            Error::Database(e) => write!(f, "database: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Error::Database(e)
    }
}

impl Error {
    /// Whether Postgres refused a row as a duplicate.
    pub fn is_unique_violation(&self) -> bool {
        matches!(self, Error::Database(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505"))
    }
}

impl From<Error> for RpcError {
    fn from(e: Error) -> Self {
        match e {
            Error::NotFound(m) => RpcError::not_found(m),
            Error::Conflict(m) => RpcError::conflict(m),
            Error::Invalid(m) => RpcError::invalid(m),
            Error::Unavailable(m) => RpcError::unavailable(m),
            Error::Database(sqlx::Error::PoolTimedOut) => {
                RpcError::unavailable("the database does not answer")
            }
            Error::Database(e) => RpcError::internal(format!("database: {e}")),
        }
    }
}
