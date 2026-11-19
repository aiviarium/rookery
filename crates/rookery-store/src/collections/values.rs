//! Converting JSON values to typed SQL parameters, and stored rows back to JSON.

use base64::{Engine as _, engine::general_purpose::STANDARD};

pub(crate) use rookery_filter::{Bind, Kind};
use serde_json::{Map, Value};
use sqlx::{Postgres, QueryBuilder};

use super::schema::{FieldDef, FieldType};
use crate::error::{Error, Result};

pub(crate) trait SqlKind {
    fn of(ty: &FieldType) -> Kind;
    fn sql_type(&self) -> &'static str;
}

pub(crate) fn kind_of(ty: &FieldType) -> Kind {
    match ty {
        FieldType::Bytes { .. } => Kind::Bytes,
        FieldType::Vector { .. } => Kind::Vector,
        FieldType::Blob => Kind::Text,
        FieldType::Text | FieldType::Longtext | FieldType::Path => Kind::Text,
        FieldType::Richtext => Kind::Richtext,
        FieldType::Integer | FieldType::Sequence { .. } => Kind::Integer,
        FieldType::Number => Kind::Number,
        FieldType::Boolean => Kind::Boolean,
        FieldType::Date => Kind::Date,
        FieldType::Datetime => Kind::Datetime,
        FieldType::Enum { values } => {
            Kind::Enum(values.iter().map(|v| v.value().to_string()).collect())
        }
        FieldType::Json => Kind::Json,
        FieldType::Ref { to } => Kind::Ref(to.clone()),
        FieldType::Refs { to } => Kind::Refs(to.clone()),
    }
}

impl SqlKind for Kind {
    fn of(ty: &FieldType) -> Kind {
        kind_of(ty)
    }

    fn sql_type(&self) -> &'static str {
        match self {
            Kind::Bytes => "bytea",
            Kind::Vector => "vector",
            Kind::Text | Kind::Enum(_) | Kind::Ref(_) => "text",
            Kind::Richtext | Kind::Json => "jsonb",
            Kind::Integer => "bigint",
            Kind::Number => "double precision",
            Kind::Boolean => "boolean",
            Kind::Date => "date",
            Kind::Datetime => "timestamptz",
            Kind::Refs(_) => "text[]",
            Kind::Uuid => "uuid",
        }
    }
}

pub(crate) trait SqlBind {
    fn push(self, qb: &mut QueryBuilder<Postgres>, sql_type: &str);
}

impl SqlBind for Bind {
    /// Pushes `$n::<type>` (or `NULL::<type>`).
    fn push(self, qb: &mut QueryBuilder<Postgres>, sql_type: &str) {
        match self {
            Bind::Null => {
                qb.push("NULL");
            }
            Bind::Text(v) => {
                qb.push_bind(v);
            }
            Bind::Int(v) => {
                qb.push_bind(v);
            }
            Bind::Float(v) => {
                qb.push_bind(v);
            }
            Bind::Bool(v) => {
                qb.push_bind(v);
            }
            Bind::Date(v) => {
                qb.push_bind(v);
            }
            Bind::Time(v) => {
                qb.push_bind(v);
            }
            Bind::Json(v) => {
                qb.push_bind(sqlx::types::Json(v));
            }
            Bind::TextArr(v) => {
                qb.push_bind(v);
            }
            Bind::Uuid(v) => {
                qb.push_bind(v);
            }
        }
        qb.push("::");
        qb.push(sql_type);
    }
}

pub(crate) fn to_bind(kind: &Kind, value: &Value) -> Result<Bind> {
    rookery_filter::to_bind(kind, value).map_err(Into::into)
}

/// Converts and validates a value for a field on write.
pub(crate) fn input(collection: &str, field: &FieldDef, v: &Value) -> Result<Bind> {
    if !v.is_null() {
        match &field.ty {
            FieldType::Bytes { max_bytes } => {
                let bytes = decode_bytes(v)
                    .map_err(|e| Error::invalid(format!("field {}: {e}", field.name)))?;
                if bytes.len() > *max_bytes as usize {
                    return Err(Error::constraint(
                        collection,
                        &field.name,
                        "maxBytes",
                        format!("field {} exceeds {max_bytes} bytes", field.name),
                    ));
                }
                return Ok(Bind::Text(format!("\\x{}", hex::encode(bytes))));
            }
            FieldType::Vector { dimensions, .. } => {
                let numbers = vector_values(v)?;
                if numbers.len() != *dimensions as usize {
                    return Err(Error::constraint(
                        collection,
                        &field.name,
                        "dimensions",
                        format!("field {} needs {dimensions} dimensions", field.name),
                    ));
                }
                return Ok(Bind::Text(
                    serde_json::to_string(&numbers).map_err(|e| Error::invalid(e.to_string()))?,
                ));
            }
            FieldType::Blob => {
                let id = v
                    .as_str()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .ok_or_else(|| {
                        Error::invalid(format!("field {} needs a blob id", field.name))
                    })?;
                return Ok(Bind::Text(id.to_string()));
            }
            _ => {}
        }
    }
    if matches!(field.ty, FieldType::Enum { .. }) {
        super::constraints::check_field_value(collection, field, v).map_err(Error::from)?;
    }
    to_bind(&Kind::of(&field.ty), v)
        .map_err(|e| Error::invalid(format!("field {}: {e}", field.name)))
}

/// System columns: (column, API name).
pub(crate) const SYSTEM_COLUMNS: &[(&str, &str)] = &[
    ("id", "id"),
    ("space_id", "spaceId"),
    ("user_id", "userId"),
    ("created_at", "createdAt"),
    ("updated_at", "updatedAt"),
    ("created_by", "createdBy"),
    ("updated_by", "updatedBy"),
    ("rev", "rev"),
    ("deleted_at", "deletedAt"),
    ("seq", "seq"),
];

/// Turns `to_jsonb(row) - '_search'` into the API form: system columns in camelCase, columns
/// that do not apply to the collection's scope and options left out.
pub(crate) fn row_out(coll: &super::Collection, v: Value) -> Value {
    row_out_fields(coll, v, |_| true)
}

/// As [`row_out`], with only the declared fields `keep` accepts (the others are not in `v`).
pub(crate) fn row_out_fields(
    coll: &super::Collection,
    v: Value,
    keep_field: impl Fn(&FieldDef) -> bool,
) -> Value {
    let Value::Object(mut m) = v else { return v };
    let mut out = Map::with_capacity(m.len());
    for (col, api) in SYSTEM_COLUMNS {
        // `seq` is a system column of log collections only; elsewhere it may be a declared field.
        if *col == "seq" && !coll.def.is_log() {
            continue;
        }
        let keep = match *col {
            "space_id" => coll.def.scope == super::Scope::Space,
            "user_id" => coll.def.scope == super::Scope::User,
            "deleted_at" => coll.def.trash,
            _ => true,
        };
        if let Some(val) = m.remove(*col)
            && keep
        {
            out.insert((*api).to_string(), val);
        }
    }
    for f in coll.def.fields.iter().filter(|f| keep_field(f)) {
        let mut val = m.remove(&f.name).unwrap_or(Value::Null);
        match &f.ty {
            FieldType::Bytes { .. } => {
                if let Some(hex) = val.as_str().and_then(|s| s.strip_prefix("\\x")) {
                    val = Value::String(
                        STANDARD.encode(hex::decode(hex).expect("Postgres bytea hex")),
                    );
                }
            }
            FieldType::Vector { .. } => {
                if let Some(s) = val.as_str() {
                    val = serde_json::json!(
                        vector_values(
                            &serde_json::from_str(s).expect("Postgres vector JSON array")
                        )
                        .expect("Postgres finite float32 vector")
                    );
                }
            }
            _ => {}
        }
        out.insert(f.name.clone(), val);
    }
    Value::Object(out)
}

/// JSON binary values use canonical padded base64.
pub fn decode_bytes(v: &Value) -> Result<Vec<u8>> {
    STANDARD
        .decode(
            v.as_str()
                .ok_or_else(|| Error::invalid("bytes must be base64"))?,
        )
        .map_err(|_| Error::invalid("bytes must be canonical base64"))
}

/// Quantize to the declared float32 storage type; reject overflow and non-finite elements.
pub fn vector_values(v: &Value) -> Result<Vec<f32>> {
    v.as_array()
        .ok_or_else(|| Error::invalid("vector must be an array"))?
        .iter()
        .map(|v| {
            let f = v
                .as_f64()
                .ok_or_else(|| Error::invalid("vector elements must be numbers"))?
                as f32;
            if !f.is_finite() {
                return Err(Error::invalid(
                    "vector elements must be finite float32 numbers",
                ));
            }
            Ok(f)
        })
        .collect()
}
