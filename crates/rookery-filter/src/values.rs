//! Typed comparable values and validation shared by matching and database adapters.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::{Error, Result};

/// The type of a column as the query layer sees it: a field type, or a system column.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Bytes,
    Vector,
    Text,
    Richtext,
    Integer,
    Number,
    Boolean,
    Date,
    Datetime,
    Enum(Vec<String>),
    Json,
    Ref(Option<String>),
    Refs(Option<String>),
    Uuid,
}

impl Kind {
    /// Whether `<`, `>` and ordering make sense.
    pub(crate) fn ordered(&self) -> bool {
        !matches!(
            self,
            Kind::Richtext
                | Kind::Json
                | Kind::Refs(_)
                | Kind::Boolean
                | Kind::Bytes
                | Kind::Vector
        )
    }
}

/// A validated, typed value usable by matchers and database adapters.
#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    Null,
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Date(NaiveDate),
    Time(DateTime<Utc>),
    Json(Value),
    TextArr(Vec<String>),
    Uuid(Uuid),
}

impl Bind {
    pub fn is_null(&self) -> bool {
        matches!(self, Bind::Null)
    }
}

/// Normalises a reference: a URI is checked against `to`; a bare id becomes `<to>://<id>`.
pub fn normalize_ref(v: &str, to: Option<&str>) -> Result<String> {
    if v.contains("://") {
        let (scheme, _) = crate::parse_reference(v)?;
        if let Some(to) = to
            && scheme != to
        {
            return Err(Error::invalid(format!(
                "expected a {to}:// reference, got {v:?}"
            )));
        }
        Ok(v.to_string())
    } else if let Some(to) = to {
        let reference = format!("{to}://{v}");
        crate::parse_reference(&reference)?;
        Ok(reference)
    } else {
        Err(Error::invalid(format!("expected an entity URI, got {v:?}")))
    }
}

/// Converts a JSON value to a parameter of the given kind, validating it.
pub fn to_bind(kind: &Kind, v: &Value) -> Result<Bind> {
    if v.is_null() {
        return Ok(Bind::Null);
    }
    let want = |what: &str| Error::invalid(format!("expected {what}, got {v}"));
    Ok(match kind {
        Kind::Bytes | Kind::Vector => {
            return Err(want("null (this field is only filterable for null)"));
        }
        Kind::Text => Bind::Text(v.as_str().ok_or_else(|| want("a string"))?.to_string()),
        Kind::Richtext => {
            if !v.is_object() {
                return Err(want("a rich text document (an object)"));
            }
            Bind::Json(v.clone())
        }
        Kind::Integer => match v {
            Value::Number(n) => match n.as_i64() {
                Some(i) => Bind::Int(i),
                None => match n.as_f64() {
                    Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Bind::Int(f as i64),
                    _ => return Err(want("an integer")),
                },
            },
            _ => return Err(want("an integer")),
        },
        Kind::Number => Bind::Float(v.as_f64().ok_or_else(|| want("a number"))?),
        Kind::Boolean => Bind::Bool(v.as_bool().ok_or_else(|| want("a boolean"))?),
        Kind::Date => {
            let s = v.as_str().ok_or_else(|| want("a date (YYYY-MM-DD)"))?;
            Bind::Date(
                NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .map_err(|_| want("a date (YYYY-MM-DD)"))?,
            )
        }
        Kind::Datetime => {
            Bind::Time(parse_datetime(v).ok_or_else(|| want("an RFC 3339 date-time"))?)
        }
        Kind::Enum(values) => {
            let s = v.as_str().ok_or_else(|| want("a string"))?;
            if !values.iter().any(|x| x == s) {
                return Err(Error::invalid(format!(
                    "{s:?} is not one of {}",
                    values.join(", ")
                )));
            }
            Bind::Text(s.to_string())
        }
        Kind::Json => Bind::Json(v.clone()),
        Kind::Ref(to) => Bind::Text(normalize_ref(
            v.as_str().ok_or_else(|| want("a reference"))?,
            to.as_deref(),
        )?),
        Kind::Refs(to) => {
            let items = v.as_array().ok_or_else(|| want("a list of references"))?;
            let mut out: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                let r = normalize_ref(
                    item.as_str().ok_or_else(|| want("a list of references"))?,
                    to.as_deref(),
                )?;
                if !out.contains(&r) {
                    out.push(r);
                }
            }
            Bind::TextArr(out)
        }
        Kind::Uuid => Bind::Uuid(
            v.as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| want("a UUID"))?,
        ),
    })
}

pub fn parse_datetime(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v.as_str()?)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}
