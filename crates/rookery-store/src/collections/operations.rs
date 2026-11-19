//! Pure atomic field-operation semantics. The write path calls this while holding the row lock,
//! then validates the resulting patch through ordinary type, constraint and transition checks.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};

use super::schema::{CollectionDef, FieldType};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldOp {
    Inc(Number),
    Add(Vec<String>),
    Remove(Vec<String>),
}
pub type FieldOps = BTreeMap<String, FieldOp>;

pub fn apply(
    def: &CollectionDef,
    old: &Value,
    patch: &Map<String, Value>,
    ops: &FieldOps,
) -> Result<Map<String, Value>> {
    let mut result = patch.clone();
    for (name, op) in ops {
        if patch.contains_key(name) {
            return Err(Error::invalid(format!(
                "field {name} occurs in both patch and ops"
            )));
        }
        let field = def
            .field(name)
            .ok_or_else(|| Error::invalid(format!("unknown field {name}")))?;
        let value = old.get(name).unwrap_or(&Value::Null);
        let value = match (&field.ty, op) {
            (FieldType::Integer, FieldOp::Inc(n)) => {
                let rookery_filter::Bind::Int(n) = rookery_filter::to_bind(
                    &rookery_filter::Kind::Integer,
                    &Value::Number(n.clone()),
                )?
                else {
                    return Err(Error::invalid("integer inc needs an integer"));
                };
                let old = value
                    .as_i64()
                    .ok_or_else(|| Error::invalid("cannot increment a null integer"))?;
                json!(
                    old.checked_add(n)
                        .ok_or_else(|| Error::invalid("integer increment overflows"))?
                )
            }
            (FieldType::Number, FieldOp::Inc(n)) => {
                let n = n
                    .as_f64()
                    .ok_or_else(|| Error::invalid("inc needs a finite number"))?;
                let old = value
                    .as_f64()
                    .ok_or_else(|| Error::invalid("cannot increment a null number"))?;
                let n = Number::from_f64(old + n)
                    .ok_or_else(|| Error::invalid("number increment overflows"))?;
                Value::Number(n)
            }
            (FieldType::Refs { to }, FieldOp::Add(items) | FieldOp::Remove(items)) => {
                let mut refs: Vec<String> = match value {
                    Value::Null => Vec::new(),
                    Value::Array(a) => a
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                    _ => return Err(Error::invalid("refs value is not an array")),
                };
                for item in items {
                    let item = rookery_filter::normalize_ref(item, to.as_deref())?;
                    match op {
                        FieldOp::Add(_) if !refs.contains(&item) => refs.push(item),
                        FieldOp::Remove(_) => refs.retain(|r| r != &item),
                        _ => {}
                    }
                }
                json!(refs)
            }
            _ => {
                return Err(Error::invalid(format!(
                    "operation does not apply to {} field {name}",
                    field.ty.name()
                )));
            }
        };
        result.insert(name.clone(), value);
    }
    Ok(result)
}
