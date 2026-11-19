//! Parameters in and values out of raw SQL.
//!
//! Parameters are always bound: the statement is parsed first so that Postgres says what type each `$n` has
//! (`where space_id = $1` makes `$1` a `uuid`), and the value is then encoded as that type in binary. No
//! parameter is ever written into the statement's text. A value maps to the type the statement wants:
//!
//! | JSON value | Types it can be |
//! | --- | --- |
//! | `null` | any |
//! | boolean | `bool` |
//! | integer | `int2`, `int4`, `int8`, `oid`, `float4`, `float8`, `numeric` |
//! | float | `float4`, `float8`, `numeric` |
//! | string | `text`, `varchar`, `bpchar`, `name`, `citext`, enums, `uuid`, `numeric`, `date`, `time`, `timestamp`, `timestamptz`, `json`, `jsonb` |
//! | `{"$bytes": "<base64>"}` | `bytea` |
//! | `{"$vector": [1, 2]}`, or an array of numbers | pgvector's `vector` |
//! | array | an array type of any of the above, one dimension |
//! | other object, array, string, number | `json`, `jsonb` |
//!
//! Results come back as JSON values: `numeric` as a string (no precision lost), timestamps and dates as RFC
//! 3339 strings (`timestamp` without a zone as UTC), `json` and `jsonb` decoded, `bytea` as base64 and
//! `vector` as an array of numbers (the gRPC surface sends those two natively), arrays as arrays. A type
//! outside the lists (`interval`, `timetz`, `inet`, ranges, composites, ...) is refused with `INVALID` before
//! the statement runs: cast it to text (`x::text`).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{
    DateTime, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone, Timelike, Utc,
};
use serde_json::{Map, Number, Value};
use sqlx::TypeInfo;
use sqlx::postgres::{PgTypeInfo, PgTypeKind};

use crate::error::{Error, Result};

/// A bound parameter, before it is known which type it is.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Vector(Vec<f32>),
    Json(Value),
    Array(Vec<Param>),
}

/// The parameter a JSON value stands for (see the module's table).
pub fn param(v: &Value) -> Result<Param> {
    Ok(match v {
        Value::Null => Param::Null,
        Value::Bool(b) => Param::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Param::Int(i),
            None => Param::Float(
                n.as_f64()
                    .ok_or_else(|| Error::invalid("a parameter is a number out of range"))?,
            ),
        },
        Value::String(s) => Param::Text(s.clone()),
        Value::Array(a) => Param::Array(a.iter().map(param).collect::<Result<_>>()?),
        Value::Object(m) if m.len() == 1 => match m.iter().next() {
            Some((k, Value::String(b))) if k == "$bytes" => Param::Bytes(
                STANDARD
                    .decode(b)
                    .map_err(|_| Error::invalid("$bytes is not base64"))?,
            ),
            Some((k, Value::Array(a))) if k == "$vector" => Param::Vector(
                a.iter()
                    .map(|n| {
                        n.as_f64()
                            .map(|f| f as f32)
                            .filter(|f| f.is_finite())
                            .ok_or_else(|| Error::invalid("$vector holds finite numbers"))
                    })
                    .collect::<Result<_>>()?,
            ),
            _ => Param::Json(v.clone()),
        },
        Value::Object(_) => Param::Json(v.clone()),
    })
}

fn type_name(ty: &PgTypeInfo) -> String {
    ty.name().to_ascii_lowercase()
}

/// Why a type cannot be a parameter or a result column, or `Ok`.
pub fn supported(ty: &PgTypeInfo) -> std::result::Result<(), String> {
    // `void` is a pseudo-type: what `pg_sleep()` and other procedures return. It reads as null.
    if type_name(ty) == "void" {
        return Ok(());
    }
    match ty.kind() {
        PgTypeKind::Array(e) => return supported(e),
        PgTypeKind::Domain(base) => return supported(base),
        PgTypeKind::Enum(_) => return Ok(()),
        PgTypeKind::Simple => {}
        _ => return Err(format!("the type {} is not supported", type_name(ty))),
    }
    match type_name(ty).as_str() {
        "bool" | "int2" | "int4" | "int8" | "oid" | "float4" | "float8" | "numeric" | "text"
        | "varchar" | "bpchar" | "name" | "\"char\"" | "citext" | "bytea" | "uuid" | "json"
        | "jsonb" | "date" | "time" | "timestamp" | "timestamptz" | "vector" | "void" => Ok(()),
        other => Err(format!(
            "the type {other} is not supported; cast it to text"
        )),
    }
}

// ── parameters ───────────────────────────────────────────────────────────────────────────────────

const PG_EPOCH_DAYS: i64 = 10_957; // days from 1970-01-01 to 2000-01-01

fn mismatch(ty: &PgTypeInfo, what: &str) -> Error {
    Error::invalid(format!("a {what} cannot be bound as {}", type_name(ty)))
}

/// `p` as the binary form of `ty`; `None` is SQL NULL.
pub fn encode(ty: &PgTypeInfo, p: &Param) -> Result<Option<Vec<u8>>> {
    if matches!(p, Param::Null) {
        return Ok(None);
    }
    match ty.kind() {
        PgTypeKind::Domain(base) => return encode(base, p),
        PgTypeKind::Enum(_) => {
            return match p {
                Param::Text(s) => Ok(Some(s.as_bytes().to_vec())),
                _ => Err(mismatch(ty, "value that is not text")),
            };
        }
        PgTypeKind::Array(elem) => return encode_array(ty, elem, p),
        PgTypeKind::Simple => {}
        _ => return Err(mismatch(ty, "value")),
    }
    let name = type_name(ty);
    let int = |p: &Param| match p {
        Param::Int(i) => Ok(*i),
        _ => Err(mismatch(ty, "value that is not an integer")),
    };
    let bytes = match name.as_str() {
        "bool" => match p {
            Param::Bool(b) => vec![*b as u8],
            _ => return Err(mismatch(ty, "value that is not a boolean")),
        },
        "int2" => i16::try_from(int(p)?)
            .map_err(|_| mismatch(ty, "number out of range"))?
            .to_be_bytes()
            .to_vec(),
        "int4" => i32::try_from(int(p)?)
            .map_err(|_| mismatch(ty, "number out of range"))?
            .to_be_bytes()
            .to_vec(),
        "int8" => int(p)?.to_be_bytes().to_vec(),
        "oid" => u32::try_from(int(p)?)
            .map_err(|_| mismatch(ty, "number out of range"))?
            .to_be_bytes()
            .to_vec(),
        "float4" => (float(ty, p)? as f32).to_be_bytes().to_vec(),
        "float8" => float(ty, p)?.to_be_bytes().to_vec(),
        "numeric" => match p {
            Param::Int(i) => numeric_binary(&i.to_string())?,
            Param::Float(f) => numeric_binary(
                &Number::from_f64(*f)
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
            )?,
            Param::Text(s) => numeric_binary(s)?,
            _ => return Err(mismatch(ty, "value that is not a number")),
        },
        "text" | "varchar" | "bpchar" | "name" | "\"char\"" | "citext" => match p {
            Param::Text(s) => s.as_bytes().to_vec(),
            _ => return Err(mismatch(ty, "value that is not text")),
        },
        "bytea" => match p {
            Param::Bytes(b) => b.clone(),
            _ => return Err(mismatch(ty, "value that is not a byte string")),
        },
        "uuid" => match p {
            Param::Text(s) => uuid::Uuid::parse_str(s)
                .map_err(|_| mismatch(ty, "text that is not a UUID"))?
                .as_bytes()
                .to_vec(),
            _ => return Err(mismatch(ty, "value that is not text")),
        },
        "json" => json_text(p)?.into_bytes(),
        "jsonb" => {
            let mut b = vec![1u8];
            b.extend(json_text(p)?.into_bytes());
            b
        }
        "date" => match p {
            Param::Text(s) => {
                let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .map_err(|_| mismatch(ty, "text that is not a date (YYYY-MM-DD)"))?;
                let days = d
                    .signed_duration_since(NaiveDate::from_ymd_opt(2000, 1, 1).expect("a date"))
                    .num_days();
                i32::try_from(days)
                    .map_err(|_| mismatch(ty, "date out of range"))?
                    .to_be_bytes()
                    .to_vec()
            }
            _ => return Err(mismatch(ty, "value that is not text")),
        },
        "time" => match p {
            Param::Text(s) => {
                let t = NaiveTime::parse_from_str(s, "%H:%M:%S%.f")
                    .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
                    .map_err(|_| mismatch(ty, "text that is not a time (HH:MM:SS)"))?;
                let micros = t.num_seconds_from_midnight() as i64 * 1_000_000
                    + (t.nanosecond() / 1000) as i64;
                micros.to_be_bytes().to_vec()
            }
            _ => return Err(mismatch(ty, "value that is not text")),
        },
        "timestamptz" | "timestamp" => match p {
            Param::Text(s) => {
                let utc: DateTime<Utc> = match DateTime::parse_from_rfc3339(s) {
                    Ok(d) => d.with_timezone(&Utc),
                    Err(_) if name == "timestamp" => {
                        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                            .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f"))
                            .map(|n| Utc.from_utc_datetime(&n))
                            .map_err(|_| mismatch(ty, "text that is not a timestamp (RFC 3339)"))?
                    }
                    Err(_) => return Err(mismatch(ty, "text that is not an RFC 3339 timestamp")),
                };
                let micros = utc.timestamp_micros() - PG_EPOCH_DAYS * 86_400 * 1_000_000;
                micros.to_be_bytes().to_vec()
            }
            _ => return Err(mismatch(ty, "value that is not text")),
        },
        "vector" => {
            let floats: Vec<f32> = match p {
                Param::Vector(v) => v.clone(),
                Param::Array(a) => a
                    .iter()
                    .map(|x| match x {
                        Param::Int(i) => Ok(*i as f32),
                        Param::Float(f) => Ok(*f as f32),
                        _ => Err(mismatch(ty, "value that is not a list of numbers")),
                    })
                    .collect::<Result<_>>()?,
                _ => return Err(mismatch(ty, "value that is not a list of numbers")),
            };
            if floats.iter().any(|f| !f.is_finite()) {
                return Err(Error::invalid("a vector holds finite numbers"));
            }
            let dim = u16::try_from(floats.len())
                .map_err(|_| Error::invalid("a vector has at most 65535 elements"))?;
            let mut b = dim.to_be_bytes().to_vec();
            b.extend([0, 0]);
            for f in floats {
                b.extend(f.to_be_bytes());
            }
            b
        }
        other => {
            return Err(Error::invalid(format!(
                "a parameter of type {other} is not supported; write $n::text::{other}"
            )));
        }
    };
    Ok(Some(bytes))
}

fn float(ty: &PgTypeInfo, p: &Param) -> Result<f64> {
    match p {
        Param::Int(i) => Ok(*i as f64),
        Param::Float(f) => Ok(*f),
        _ => Err(mismatch(ty, "value that is not a number")),
    }
}

fn json_text(p: &Param) -> Result<String> {
    let v = match p {
        Param::Json(v) => v.clone(),
        Param::Bool(b) => Value::Bool(*b),
        Param::Int(i) => Value::from(*i),
        Param::Float(f) => Number::from_f64(*f)
            .map(Value::Number)
            .ok_or_else(|| Error::invalid("a JSON number is finite"))?,
        Param::Text(s) => Value::String(s.clone()),
        Param::Array(a) => Value::Array(
            a.iter()
                .map(|x| {
                    json_text(x).and_then(|t| {
                        serde_json::from_str(&t).map_err(|e| Error::invalid(e.to_string()))
                    })
                })
                .collect::<Result<_>>()?,
        ),
        Param::Bytes(_) | Param::Vector(_) => {
            return Err(Error::invalid(
                "bytes and vectors are not JSON; encode them as text or numbers",
            ));
        }
        Param::Null => Value::Null,
    };
    serde_json::to_string(&v).map_err(|e| Error::invalid(e.to_string()))
}

fn encode_array(ty: &PgTypeInfo, elem: &PgTypeInfo, p: &Param) -> Result<Option<Vec<u8>>> {
    let items = match p {
        Param::Array(a) => a,
        // A JSON array of numbers a `vector[]`... is not a thing; pgvector's arrays are rare.
        _ => return Err(mismatch(ty, "value that is not a list")),
    };
    let oid = elem.oid().ok_or_else(|| {
        Error::invalid(format!("an array of {} is not supported", type_name(elem)))
    })?;
    let mut out = Vec::new();
    if items.is_empty() {
        out.extend(0i32.to_be_bytes());
        out.extend(0i32.to_be_bytes());
        out.extend(oid.0.to_be_bytes());
        return Ok(Some(out));
    }
    let has_null = items.iter().any(|i| matches!(i, Param::Null));
    out.extend(1i32.to_be_bytes());
    out.extend((has_null as i32).to_be_bytes());
    out.extend(oid.0.to_be_bytes());
    out.extend((items.len() as i32).to_be_bytes());
    out.extend(1i32.to_be_bytes());
    for item in items {
        if matches!(item, Param::Array(_)) && !matches!(type_name(elem).as_str(), "json" | "jsonb")
        {
            return Err(Error::invalid("only one-dimensional arrays can be bound"));
        }
        match encode(elem, item)? {
            None => out.extend((-1i32).to_be_bytes()),
            Some(b) => {
                out.extend((b.len() as i32).to_be_bytes());
                out.extend(b);
            }
        }
    }
    Ok(Some(out))
}

// ── numeric ──────────────────────────────────────────────────────────────────────────────────────

/// `numeric`'s binary form of a decimal text (`-12.50`, `1e3`, `NaN`).
fn numeric_binary(text: &str) -> Result<Vec<u8>> {
    let bad = || Error::invalid(format!("{text:?} is not a number"));
    let t = text.trim();
    let mut out = Vec::new();
    if t.eq_ignore_ascii_case("nan") {
        out.extend(0i16.to_be_bytes());
        out.extend(0i16.to_be_bytes());
        out.extend(0xC000u16.to_be_bytes());
        out.extend(0i16.to_be_bytes());
        return Ok(out);
    }
    let (negative, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mantissa, exp) = match rest.find(['e', 'E']) {
        Some(i) => (&rest[..i], rest[i + 1..].parse::<i64>().map_err(|_| bad())?),
        None => (rest, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if (int_part.is_empty() && frac_part.is_empty())
        || !int_part
            .bytes()
            .chain(frac_part.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(bad());
    }
    if exp.unsigned_abs() > 100_000 {
        return Err(bad());
    }
    // All digits with the decimal point after `point` of them.
    let mut digits: Vec<u8> = int_part
        .bytes()
        .chain(frac_part.bytes())
        .map(|b| b - b'0')
        .collect();
    let mut point = int_part.len() as i64 + exp;
    if point < 0 {
        let pad = (-point) as usize;
        let mut z = vec![0u8; pad];
        z.extend(digits);
        digits = z;
        point = 0;
    } else if point > digits.len() as i64 {
        digits.resize(point as usize, 0);
    }
    let point = point as usize;
    let dscale = digits.len() - point;
    // Groups of four, aligned at the decimal point.
    let int_digits = &digits[..point];
    let lead = (4 - int_digits.len() % 4) % 4;
    let mut groups: Vec<i16> = Vec::new();
    let mut aligned: Vec<u8> = vec![0; lead];
    aligned.extend(int_digits);
    let int_groups = aligned.len() / 4;
    let frac = &digits[point..];
    aligned.extend(frac);
    while !aligned.len().is_multiple_of(4) {
        aligned.push(0);
    }
    for c in aligned.chunks(4) {
        groups.push(c.iter().fold(0i16, |a, d| a * 10 + *d as i16));
    }
    let mut weight = int_groups as i64 - 1;
    while groups.first() == Some(&0) {
        groups.remove(0);
        weight -= 1;
    }
    while groups.last() == Some(&0) {
        groups.pop();
    }
    if groups.is_empty() {
        weight = 0;
    }
    let weight = i16::try_from(weight).map_err(|_| bad())?;
    let negative = negative && !groups.is_empty();
    out.extend((groups.len() as i16).to_be_bytes());
    out.extend(weight.to_be_bytes());
    out.extend((if negative { 0x4000u16 } else { 0 }).to_be_bytes());
    out.extend((dscale as i16).to_be_bytes());
    for g in groups {
        out.extend(g.to_be_bytes());
    }
    Ok(out)
}

fn numeric_text(b: &[u8]) -> Result<String> {
    let bad = || Error::internal("a numeric of an unknown form");
    if b.len() < 8 {
        return Err(bad());
    }
    let ndigits = i16::from_be_bytes([b[0], b[1]]) as usize;
    let weight = i16::from_be_bytes([b[2], b[3]]) as i64;
    let sign = u16::from_be_bytes([b[4], b[5]]);
    let dscale = i16::from_be_bytes([b[6], b[7]]).max(0) as usize;
    match sign {
        0xC000 => return Ok("NaN".into()),
        0xD000 => return Ok("Infinity".into()),
        0xF000 => return Ok("-Infinity".into()),
        _ => {}
    }
    if b.len() < 8 + ndigits * 2 {
        return Err(bad());
    }
    let digits: Vec<i16> = (0..ndigits)
        .map(|i| i16::from_be_bytes([b[8 + i * 2], b[9 + i * 2]]))
        .collect();
    let mut s = String::new();
    if sign == 0x4000 {
        s.push('-');
    }
    if weight < 0 {
        s.push('0');
    } else {
        for i in 0..=weight {
            let d = digits.get(i as usize).copied().unwrap_or(0);
            if i == 0 {
                s.push_str(&d.to_string());
            } else {
                s.push_str(&format!("{d:04}"));
            }
        }
    }
    if dscale > 0 {
        s.push('.');
        let mut frac = String::new();
        let mut i = weight + 1;
        while frac.len() < dscale {
            let d = if i >= 0 {
                digits.get(i as usize).copied().unwrap_or(0)
            } else {
                0
            };
            frac.push_str(&format!("{d:04}"));
            i += 1;
        }
        frac.truncate(dscale);
        s.push_str(&frac);
    }
    Ok(s)
}

// ── results ──────────────────────────────────────────────────────────────────────────────────────

fn rfc3339(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn finite(f: f64) -> Value {
    Number::from_f64(f).map(Value::Number).unwrap_or_else(|| {
        Value::String(if f.is_nan() {
            "NaN".into()
        } else if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        })
    })
}

/// The JSON value of a column's binary bytes of type `ty`.
pub fn decode(ty: &PgTypeInfo, b: &[u8]) -> Result<Value> {
    let short = || Error::internal(format!("a {} value is too short", type_name(ty)));
    match ty.kind() {
        PgTypeKind::Domain(base) => return decode(base, b),
        PgTypeKind::Enum(_) => return Ok(Value::String(String::from_utf8_lossy(b).into_owned())),
        PgTypeKind::Array(elem) => return decode_array(elem, b),
        _ => {}
    }
    Ok(match type_name(ty).as_str() {
        "bool" => Value::Bool(*b.first().ok_or_else(short)? != 0),
        "int2" => Value::from(i16::from_be_bytes(b.try_into().map_err(|_| short())?)),
        "int4" => Value::from(i32::from_be_bytes(b.try_into().map_err(|_| short())?)),
        "int8" => Value::from(i64::from_be_bytes(b.try_into().map_err(|_| short())?)),
        "oid" => Value::from(u32::from_be_bytes(b.try_into().map_err(|_| short())?)),
        "float4" => finite(f32::from_be_bytes(b.try_into().map_err(|_| short())?) as f64),
        "float8" => finite(f64::from_be_bytes(b.try_into().map_err(|_| short())?)),
        "numeric" => Value::String(numeric_text(b)?),
        "text" | "varchar" | "bpchar" | "name" | "\"char\"" | "citext" => {
            Value::String(String::from_utf8_lossy(b).into_owned())
        }
        "void" => Value::Null,
        "bytea" => Value::String(STANDARD.encode(b)),
        "uuid" => Value::String(
            uuid::Uuid::from_slice(b)
                .map_err(|_| short())?
                .hyphenated()
                .to_string(),
        ),
        "json" => {
            serde_json::from_slice(b).map_err(|e| Error::internal(format!("json column: {e}")))?
        }
        "jsonb" => serde_json::from_slice(b.get(1..).ok_or_else(short)?)
            .map_err(|e| Error::internal(format!("jsonb column: {e}")))?,
        "date" => {
            let days =
                i32::from_be_bytes(b.try_into().map_err(|_| short())?) as i64 + PG_EPOCH_DAYS;
            let d = NaiveDate::from_ymd_opt(1970, 1, 1)
                .expect("a date")
                .checked_add_signed(chrono::Duration::days(days))
                .ok_or_else(|| Error::internal("a date out of range"))?;
            Value::String(d.format("%Y-%m-%d").to_string())
        }
        "time" => {
            let micros = i64::from_be_bytes(b.try_into().map_err(|_| short())?);
            let t = NaiveTime::from_num_seconds_from_midnight_opt(
                (micros / 1_000_000) as u32,
                ((micros % 1_000_000) * 1000) as u32,
            )
            .ok_or_else(|| Error::internal("a time out of range"))?;
            Value::String(if micros % 1_000_000 == 0 {
                t.format("%H:%M:%S").to_string()
            } else {
                t.format("%H:%M:%S%.6f").to_string()
            })
        }
        "timestamptz" | "timestamp" => {
            let micros = i64::from_be_bytes(b.try_into().map_err(|_| short())?)
                + PG_EPOCH_DAYS * 86_400 * 1_000_000;
            let d = DateTime::<Utc>::from_timestamp_micros(micros)
                .ok_or_else(|| Error::internal("a timestamp out of range"))?;
            Value::String(rfc3339(d))
        }
        "vector" => {
            if b.len() < 4 {
                return Err(short());
            }
            let dim = u16::from_be_bytes([b[0], b[1]]) as usize;
            if b.len() < 4 + dim * 4 {
                return Err(short());
            }
            Value::Array(
                (0..dim)
                    .map(|i| {
                        finite(f32::from_be_bytes(
                            b[4 + i * 4..8 + i * 4].try_into().expect("four bytes"),
                        ) as f64)
                    })
                    .collect(),
            )
        }
        other => {
            return Err(Error::invalid(format!(
                "a result column of type {other} is not supported; cast it to text"
            )));
        }
    })
}

fn decode_array(elem: &PgTypeInfo, b: &[u8]) -> Result<Value> {
    let short = || Error::internal("an array value is too short");
    let int = |at: usize| -> Result<i32> {
        Ok(i32::from_be_bytes(
            b.get(at..at + 4)
                .ok_or_else(short)?
                .try_into()
                .expect("four bytes"),
        ))
    };
    let ndim = int(0)?;
    if ndim == 0 {
        return Ok(Value::Array(vec![]));
    }
    let mut at = 12;
    let mut dims = Vec::new();
    for _ in 0..ndim {
        dims.push(int(at)?.max(0) as usize);
        at += 8;
    }
    fn build(elem: &PgTypeInfo, b: &[u8], dims: &[usize], at: &mut usize) -> Result<Value> {
        let short = || Error::internal("an array value is too short");
        let mut out = Vec::new();
        for _ in 0..dims[0] {
            if dims.len() > 1 {
                out.push(build(elem, b, &dims[1..], at)?);
            } else {
                let len = i32::from_be_bytes(
                    b.get(*at..*at + 4)
                        .ok_or_else(short)?
                        .try_into()
                        .expect("four bytes"),
                );
                *at += 4;
                if len < 0 {
                    out.push(Value::Null);
                } else {
                    let end = *at + len as usize;
                    out.push(decode(elem, b.get(*at..end).ok_or_else(short)?)?);
                    *at = end;
                }
            }
        }
        Ok(Value::Array(out))
    }
    build(elem, b, &dims, &mut at)
}

/// A row as an object, from column names and values. A repeated name keeps its last value.
pub fn row(names: &[String], values: Vec<Value>) -> Value {
    let mut m = Map::new();
    for (n, v) in names.iter().zip(values) {
        m.insert(n.clone(), v);
    }
    Value::Object(m)
}
