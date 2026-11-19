//! Pure field constraint and enum transition checks. No database dependency, so unit tests run
//! directly on [`CollectionDef`] and JSON values.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;
use serde_json::{Map, Value};

use super::schema::{CollectionDef, FieldDef, FieldType};
use crate::error::Error;

/// One violation of a field constraint or enum transition rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintViolation {
    pub collection: String,
    pub field: String,
    pub constraint: &'static str,
    pub message: String,
}

impl From<ConstraintViolation> for Error {
    fn from(v: ConstraintViolation) -> Self {
        Error::constraint(&v.collection, &v.field, v.constraint, v.message)
    }
}

fn violation(
    collection: &str,
    field: &str,
    constraint: &'static str,
    message: impl Into<String>,
) -> ConstraintViolation {
    ConstraintViolation {
        collection: collection.to_string(),
        field: field.to_string(),
        constraint,
        message: message.into(),
    }
}

/// The compiled form of a field's `pattern`. Patterns come from manifests, so there are few of them and they
/// are checked on every write: each is compiled once and kept.
fn compiled(pattern: &str) -> Result<Regex, regex::Error> {
    static CACHE: LazyLock<Mutex<HashMap<String, Regex>>> = LazyLock::new(Default::default);
    if let Some(re) = CACHE.lock().unwrap().get(pattern) {
        return Ok(re.clone());
    }
    let re = Regex::new(pattern)?;
    CACHE
        .lock()
        .unwrap()
        .insert(pattern.to_string(), re.clone());
    Ok(re)
}

/// Checks one field's constraints (`required`, `enum`, `min`, `max`, `minLength`, `maxLength`,
/// `pattern`) against `value`. Type parsing for dates, refs, etc. is handled by `values::input`.
pub fn check_field_value(
    collection: &str,
    field: &FieldDef,
    value: &Value,
) -> Result<(), ConstraintViolation> {
    if value.is_null() {
        if field.required {
            return Err(violation(
                collection,
                &field.name,
                "required",
                format!("field {} is required", field.name),
            ));
        }
        return Ok(());
    }

    match &field.ty {
        FieldType::Enum { values } => {
            let Some(s) = value.as_str() else {
                return Err(violation(
                    collection,
                    &field.name,
                    "enum",
                    format!("field {} must be an enum string", field.name),
                ));
            };
            if !values.iter().any(|v| v.value() == s) {
                let allowed: Vec<&str> = values.iter().map(|v| v.value()).collect();
                return Err(violation(
                    collection,
                    &field.name,
                    "enum",
                    format!(
                        "field {}: {:?} is not one of {}",
                        field.name,
                        s,
                        allowed.join(", ")
                    ),
                ));
            }
        }
        FieldType::Integer | FieldType::Number => {
            if let Some(num) = value.as_number() {
                let n = num
                    .as_f64()
                    .or_else(|| num.as_i64().map(|i| i as f64))
                    .unwrap_or(f64::NAN);
                if let Some(min) = field.min
                    && n < min
                {
                    return Err(violation(
                        collection,
                        &field.name,
                        "min",
                        format!("field {} must be at least {min}", field.name),
                    ));
                }
                if let Some(max) = field.max
                    && n > max
                {
                    return Err(violation(
                        collection,
                        &field.name,
                        "max",
                        format!("field {} must be at most {max}", field.name),
                    ));
                }
            }
        }
        FieldType::Text | FieldType::Longtext | FieldType::Path => {
            if let Some(s) = value.as_str() {
                let len = s.chars().count();
                if let Some(min_len) = field.min_length
                    && len < min_len as usize
                {
                    return Err(violation(
                        collection,
                        &field.name,
                        "minLength",
                        format!("field {} must be at least {min_len} characters", field.name),
                    ));
                }
                if let Some(max_len) = field.max_length
                    && len > max_len as usize
                {
                    return Err(violation(
                        collection,
                        &field.name,
                        "maxLength",
                        format!("field {} must be at most {max_len} characters", field.name),
                    ));
                }
                if let Some(pat) = &field.pattern {
                    let re = compiled(pat).map_err(|e| {
                        violation(
                            collection,
                            &field.name,
                            "pattern",
                            format!("field {}: invalid pattern {pat:?}: {e}", field.name),
                        )
                    })?;
                    if !re.is_match(s) {
                        return Err(violation(
                            collection,
                            &field.name,
                            "pattern",
                            format!("field {} does not match pattern {pat:?}", field.name),
                        ));
                    }
                }
            }
        }
        _ => {}
    }

    Ok(())
}

/// Checks whether changing `field` from `old` to `new` is permitted by `field.transitions`.
///
/// No-ops (`old == new`) and setting an unset (`null`) enum field to a value are allowed.
/// States with no entry in `transitions` are terminal, and clearing a non-null enum field to
/// `null` when `transitions` is declared is refused unless listed (which cannot happen for valid
/// `transitions`, whose targets must be declared enum values).
pub fn check_transition(
    collection: &str,
    field: &FieldDef,
    old: &Value,
    new: &Value,
) -> Result<(), ConstraintViolation> {
    let Some(transitions) = &field.transitions else {
        return Ok(());
    };
    if !matches!(field.ty, FieldType::Enum { .. }) {
        return Ok(());
    }
    if old.is_null() {
        return Ok(());
    }
    let Some(from) = old.as_str() else {
        return Ok(());
    };
    if new.is_null() {
        return Err(violation(
            collection,
            &field.name,
            "transitions",
            format!(
                "field {}: transition from {:?} to null is not allowed",
                field.name, from
            ),
        ));
    }
    let Some(to) = new.as_str() else {
        return Ok(());
    };
    if from == to {
        return Ok(());
    }
    if transitions
        .get(from)
        .is_some_and(|allowed| allowed.iter().any(|candidate| candidate == to))
    {
        return Ok(());
    }
    Err(violation(
        collection,
        &field.name,
        "transitions",
        format!(
            "field {}: transition from {:?} to {:?} is not allowed",
            field.name, from, to
        ),
    ))
}

/// Checks every field constraint on an insert object (applying defaults for omitted fields).
pub fn check_insert(
    def: &CollectionDef,
    values: &Map<String, Value>,
) -> Result<(), ConstraintViolation> {
    for f in &def.fields {
        if matches!(f.ty, FieldType::Sequence { .. }) {
            continue;
        }
        let val = values
            .get(&f.name)
            .or(f.default.as_ref())
            .unwrap_or(&Value::Null);
        check_field_value(&def.name, f, val)?;
    }
    Ok(())
}

/// Checks every field constraint and transition on an update patch against an existing row.
pub fn check_update(
    def: &CollectionDef,
    old: &Map<String, Value>,
    patch: &Map<String, Value>,
) -> Result<(), ConstraintViolation> {
    for (k, v) in patch {
        let Some(f) = def.field(k) else { continue };
        check_field_value(&def.name, f, v)?;
        let prev = old.get(k).unwrap_or(&Value::Null);
        check_transition(&def.name, f, prev, v)?;
    }
    Ok(())
}
