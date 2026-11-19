//! Entity URIs: `<scheme>://<id>`.

use std::fmt;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Uri {
    pub scheme: String,
    pub id: String,
}

impl Uri {
    pub fn new(scheme: impl Into<String>, id: impl Into<String>) -> Self {
        Uri {
            scheme: scheme.into(),
            id: id.into(),
        }
    }

    /// Parses `<scheme>://<id>`. The scheme is lower-case letters, digits, `+`, `-` and `.`,
    /// starting with a letter; the id is non-empty and has no whitespace.
    pub fn parse(s: &str) -> Result<Uri> {
        let (scheme, id) = rookery_filter::parse_reference(s)?;
        Ok(Uri::new(scheme, id))
    }
}

pub(crate) use rookery_filter::valid_scheme;

impl fmt::Display for Uri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.id)
    }
}

impl std::str::FromStr for Uri {
    type Err = Error;
    fn from_str(s: &str) -> Result<Uri> {
        Uri::parse(s)
    }
}

pub fn space_uri(id: uuid::Uuid) -> String {
    format!("space://{id}")
}
pub fn user_uri(id: uuid::Uuid) -> String {
    format!("user://{id}")
}
pub fn plugin_uri(id: &str) -> String {
    format!("plugin://{id}")
}
