//! What can go wrong in the store, and how it reaches a caller.

use std::fmt;

use rookery_proto::{ErrorCode, RpcError};
use serde_json::{Value, json};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One way a manifest's collection differs from the stored schema that cannot be applied without a
/// migration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDifference {
    pub collection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub description: String,
}

impl fmt::Display for SchemaDifference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.field {
            Some(field) => write!(f, "{}.{}: {}", self.collection, field, self.description),
            None => write!(f, "{}: {}", self.collection, self.description),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    NotFound(String),
    Conflict(String),
    Invalid(String),
    Forbidden(String),
    /// Something the store needs is not set up, such as the sealing key.
    Unavailable(String),
    /// Something took longer than its limit, such as a raw SQL statement.
    Timeout(String),
    Internal(String),
    /// An update named a revision that is not the row's.
    RevConflict {
        expected: i64,
        actual: i64,
    },
    /// The manifest cannot be reconciled with the stored schema without a migration.
    Schema(Vec<SchemaDifference>),
    /// A conflict with details, such as the answer that came first.
    ConflictWith {
        message: String,
        details: Value,
    },
    /// Invalid input with details, such as the constraint a value breaks.
    InvalidWith {
        message: String,
        details: Value,
    },
    Database(sqlx::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(m)
            | Error::Conflict(m)
            | Error::Invalid(m)
            | Error::Forbidden(m)
            | Error::Unavailable(m)
            | Error::Timeout(m)
            | Error::Internal(m) => f.write_str(m),
            Error::ConflictWith { message, .. } | Error::InvalidWith { message, .. } => {
                f.write_str(message)
            }
            Error::RevConflict { expected, actual } => write!(
                f,
                "the row changed meanwhile: it is at revision {actual}, not {expected}"
            ),
            Error::Schema(diffs) => write!(
                f,
                "the collections need a migration: {}",
                diffs
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Error::Database(e) => write!(f, "database: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::Database(d) if d.code().as_deref() == Some(crate::leader::DEPOSED) => {
                Error::Unavailable(d.message().to_owned())
            }
            _ => Error::Database(e),
        }
    }
}

impl From<rookery_filter::Error> for Error {
    fn from(e: rookery_filter::Error) -> Self {
        Error::Invalid(e.to_string())
    }
}

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }
    pub fn invalid_with(msg: impl Into<String>, details: Value) -> Self {
        Error::InvalidWith {
            message: msg.into(),
            details,
        }
    }
    /// A value that breaks a constraint of a field.
    pub fn constraint(
        collection: &str,
        field: &str,
        constraint: &str,
        msg: impl Into<String>,
    ) -> Self {
        Self::invalid_with(
            msg,
            json!({ "collection": collection, "field": field, "constraint": constraint }),
        )
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Error::Forbidden(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::NotFound(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::Conflict(msg.into())
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Error::Internal(msg.into())
    }

    /// Whether the write was refused because another server leads now.
    pub fn is_deposed(&self) -> bool {
        matches!(self, Error::Unavailable(m) if m.starts_with(crate::leader::DEPOSED_MESSAGE))
    }

    /// Whether Postgres refused a row as a duplicate.
    pub fn is_unique_violation(&self) -> bool {
        db_code(self).as_deref() == Some("23505")
    }

    /// The code a caller sees. Database errors go by their SQLSTATE: a duplicate or a serialization
    /// failure is `CONFLICT`, input only Postgres caught is `INVALID`.
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Invalid(_) | Error::InvalidWith { .. } | Error::Schema(_) => ErrorCode::Invalid,
            Error::Forbidden(_) => ErrorCode::Forbidden,
            Error::NotFound(_) => ErrorCode::NotFound,
            Error::Conflict(_) | Error::RevConflict { .. } | Error::ConflictWith { .. } => {
                ErrorCode::Conflict
            }
            Error::Unavailable(_) => ErrorCode::Unavailable,
            Error::Timeout(_) => ErrorCode::Timeout,
            Error::Internal(_) => ErrorCode::Internal,
            Error::Database(e) => match db_code(self).as_deref() {
                Some("23505") | Some("40001") | Some("40P01") => ErrorCode::Conflict,
                Some("57014") => ErrorCode::Timeout,
                Some(c) if c.starts_with("22") || c == "23503" || c == "23502" || c == "23514" => {
                    ErrorCode::Invalid
                }
                _ => match e {
                    sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::Io(_) => {
                        ErrorCode::Unavailable
                    }
                    sqlx::Error::RowNotFound => ErrorCode::NotFound,
                    _ => ErrorCode::Internal,
                },
            },
        }
    }

    /// What goes in `error.data.details`.
    pub fn details(&self) -> Option<Value> {
        match self {
            Error::RevConflict { expected, actual } => {
                Some(json!({ "expected": expected, "actual": actual }))
            }
            Error::Schema(diffs) => Some(json!({ "differences": diffs })),
            Error::ConflictWith { details, .. } | Error::InvalidWith { details, .. } => {
                Some(details.clone())
            }
            Error::Database(sqlx::Error::Database(d)) if d.constraint().is_some() => {
                Some(json!({ "constraint": d.constraint() }))
            }
            _ => None,
        }
    }
}

fn db_code(e: &Error) -> Option<String> {
    match e {
        Error::Database(sqlx::Error::Database(d)) => d.code().map(|c| c.into_owned()),
        _ => None,
    }
}

impl From<Error> for RpcError {
    fn from(e: Error) -> Self {
        let code = e.code();
        let details = e.details();
        let message = match (&e, code) {
            (Error::Database(sqlx::Error::PoolTimedOut), _) => {
                "the database does not answer".to_owned()
            }
            (
                Error::Database(sqlx::Error::Database(d)),
                ErrorCode::Invalid | ErrorCode::Conflict,
            ) => d.message().to_owned(),
            _ => e.to_string(),
        };
        let err = RpcError::new(code, message);
        match details {
            Some(d) => err.with_details(d),
            None => err,
        }
    }
}
