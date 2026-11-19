//! Structured filters: parsing, validation and two-valued matching over JSON rows.
//! The shape and semantics are in `FILTER.md` at the crate root.

use std::cmp::Ordering;

use chrono::NaiveDate;
use serde_json::Value;
use uuid::Uuid;

mod error;
mod reference;
mod values;

pub use error::{Error, Result};
pub use reference::{parse_reference, valid_scheme};
pub use values::{Bind, Kind, normalize_ref, parse_datetime, to_bind};

/// Supplies field kinds by their names in JSON rows, including any system fields.
pub trait Fields {
    fn kind(&self, field: &str) -> Option<Kind>;
}

impl Fields for std::collections::BTreeMap<String, Kind> {
    fn kind(&self, field: &str) -> Option<Kind> {
        self.get(field).cloned()
    }
}

const MAX_NODES: usize = 256;
const MAX_DEPTH: usize = 16;
const MAX_LIST: usize = 1000;

/// A parsed, validated filter over one collection's rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    node: Node,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Cmp(Cmp),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cmp {
    /// The API name, as in output rows.
    pub field: String,
    pub kind: Kind,
    pub op: Op,
    pub value: Operand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    In,
    Nin,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    One(Bind),
    Many(Vec<Bind>),
}

impl Filter {
    /// A filter that matches every row.
    pub fn all() -> Filter {
        Filter {
            node: Node::And(Vec::new()),
        }
    }

    /// Parses a `where` object against a collection's fields.
    pub fn parse(coll: &impl Fields, v: &Value) -> Result<Filter> {
        let mut count = 0;
        let node = parse_node(coll, v, 0, &mut count)?;
        Ok(Filter { node })
    }

    /// Whether a row (in the form the store returns rows) matches.
    pub fn matches(&self, row: &Value) -> bool {
        eval(&self.node, row)
    }

    /// The validated expression tree, for adapters such as SQL compilers.
    pub fn node(&self) -> &Node {
        &self.node
    }
}

fn parse_node(coll: &impl Fields, v: &Value, depth: usize, count: &mut usize) -> Result<Node> {
    if depth > MAX_DEPTH {
        return Err(Error::invalid("where is nested too deeply"));
    }
    let obj = v
        .as_object()
        .ok_or_else(|| Error::invalid("where must be an object"))?;
    let mut nodes = Vec::new();
    for (key, val) in obj {
        *count += 1;
        if *count > MAX_NODES {
            return Err(Error::invalid("where is too large"));
        }
        match key.as_str() {
            "and" | "or" => {
                let items = val
                    .as_array()
                    .ok_or_else(|| Error::invalid(format!("{key} takes a list")))?;
                let parsed = items
                    .iter()
                    .map(|i| parse_node(coll, i, depth + 1, count))
                    .collect::<Result<Vec<_>>>()?;
                nodes.push(if key == "and" {
                    Node::And(parsed)
                } else {
                    Node::Or(parsed)
                });
            }
            "not" => nodes.push(Node::Not(Box::new(parse_node(
                coll,
                val,
                depth + 1,
                count,
            )?))),
            field => {
                let kind = coll
                    .kind(field)
                    .ok_or_else(|| Error::invalid(format!("unknown field {field} in where")))?;
                match val {
                    Value::Object(ops) => {
                        if ops.is_empty() {
                            return Err(Error::invalid(format!("{field}: empty operator object")));
                        }
                        for (op, operand) in ops {
                            nodes.push(Node::Cmp(parse_cmp(field, &kind, op, operand)?));
                        }
                    }
                    other => nodes.push(Node::Cmp(parse_cmp(field, &kind, "eq", other)?)),
                }
            }
        }
    }
    Ok(if nodes.len() == 1 {
        nodes.pop().expect("one node")
    } else {
        Node::And(nodes)
    })
}

fn parse_cmp(field: &str, kind: &Kind, op: &str, v: &Value) -> Result<Cmp> {
    let err = |m: String| Error::invalid(format!("{field}: {m}"));
    let op = match op {
        "eq" => Op::Eq,
        "ne" => Op::Ne,
        "in" => Op::In,
        "nin" => Op::Nin,
        "lt" => Op::Lt,
        "lte" => Op::Lte,
        "gt" => Op::Gt,
        "gte" => Op::Gte,
        "contains" => Op::Contains,
        other => return Err(err(format!("unknown operator {other}"))),
    };
    if matches!(kind, Kind::Bytes | Kind::Vector) && !(matches!(op, Op::Eq | Op::Ne) && v.is_null())
    {
        return Err(err(
            "only eq/ne null is supported for bytes and vector".into()
        ));
    }
    let value = match op {
        Op::Eq | Op::Ne => {
            if matches!(kind, Kind::Refs(_)) {
                return Err(err("use contains on a refs field".into()));
            }
            if v.is_array() && !matches!(kind, Kind::Json) {
                return Err(err("a list needs the in operator".into()));
            }
            Operand::One(to_bind(kind, v).map_err(|e| err(e.to_string()))?)
        }
        Op::In | Op::Nin => {
            if matches!(kind, Kind::Refs(_) | Kind::Json | Kind::Richtext) {
                return Err(err(format!(
                    "in does not apply to {} fields",
                    kind_name(kind)
                )));
            }
            let items = v.as_array().ok_or_else(|| err("in takes a list".into()))?;
            if items.len() > MAX_LIST {
                return Err(err(format!("at most {MAX_LIST} values")));
            }
            Operand::Many(
                items
                    .iter()
                    .map(|i| to_bind(kind, i).map_err(|e| err(e.to_string())))
                    .collect::<Result<_>>()?,
            )
        }
        Op::Lt | Op::Lte | Op::Gt | Op::Gte => {
            if !kind.ordered() {
                return Err(err(format!("cannot compare {} fields", kind_name(kind))));
            }
            if v.is_null() {
                return Err(err("cannot compare with null".into()));
            }
            Operand::One(to_bind(kind, v).map_err(|e| err(e.to_string()))?)
        }
        Op::Contains => {
            let Kind::Refs(to) = kind else {
                return Err(err("contains applies to refs fields".into()));
            };
            let items: Vec<&Value> = match v {
                Value::Array(a) => a.iter().collect(),
                other => vec![other],
            };
            let mut out = Vec::with_capacity(items.len());
            for i in items {
                let s = i
                    .as_str()
                    .ok_or_else(|| err("contains takes a reference or a list of them".into()))?;
                out.push(normalize_ref(s, to.as_deref()).map_err(|e| err(e.to_string()))?);
            }
            Operand::One(Bind::TextArr(out))
        }
    };
    Ok(Cmp {
        field: field.to_string(),
        kind: kind.clone(),
        op,
        value,
    })
}

fn kind_name(k: &Kind) -> &'static str {
    match k {
        Kind::Bytes => "bytes",
        Kind::Vector => "vector",
        Kind::Text => "text",
        Kind::Richtext => "richtext",
        Kind::Integer => "integer",
        Kind::Number => "number",
        Kind::Boolean => "boolean",
        Kind::Date => "date",
        Kind::Datetime => "datetime",
        Kind::Enum(_) => "enum",
        Kind::Json => "json",
        Kind::Ref(_) => "ref",
        Kind::Refs(_) => "refs",
        Kind::Uuid => "id",
    }
}

// --- In memory ---------------------------------------------------------------------------

fn eval(node: &Node, row: &Value) -> bool {
    match node {
        Node::And(items) => items.iter().all(|n| eval(n, row)),
        Node::Or(items) => items.iter().any(|n| eval(n, row)),
        Node::Not(inner) => !eval(inner, row),
        Node::Cmp(c) => eval_cmp(c, row.get(&c.field).unwrap_or(&Value::Null)),
    }
}

fn eval_cmp(c: &Cmp, v: &Value) -> bool {
    match (&c.op, &c.value) {
        (Op::Eq, Operand::One(b)) => {
            if b.is_null() {
                v.is_null()
            } else {
                !v.is_null() && equal(&c.kind, v, b)
            }
        }
        (Op::Ne, Operand::One(b)) => {
            if b.is_null() {
                !v.is_null()
            } else {
                v.is_null() || !equal(&c.kind, v, b)
            }
        }
        (Op::In, Operand::Many(items)) => in_list(&c.kind, v, items),
        (Op::Nin, Operand::Many(items)) => !in_list(&c.kind, v, items),
        (Op::Lt | Op::Lte | Op::Gt | Op::Gte, Operand::One(b)) => {
            if v.is_null() {
                return false;
            }
            match compare(&c.kind, v, b) {
                Some(o) => match c.op {
                    Op::Lt => o == Ordering::Less,
                    Op::Lte => o != Ordering::Greater,
                    Op::Gt => o == Ordering::Greater,
                    _ => o != Ordering::Less,
                },
                None => false,
            }
        }
        (Op::Contains, Operand::One(Bind::TextArr(want))) => match v.as_array() {
            Some(have) => want
                .iter()
                .all(|w| have.iter().any(|h| h.as_str() == Some(w.as_str()))),
            None => false,
        },
        _ => false,
    }
}

fn in_list(kind: &Kind, v: &Value, items: &[Bind]) -> bool {
    items.iter().any(|b| {
        if b.is_null() {
            v.is_null()
        } else {
            !v.is_null() && equal(kind, v, b)
        }
    })
}

fn equal(kind: &Kind, v: &Value, b: &Bind) -> bool {
    match (kind, b) {
        (Kind::Json | Kind::Richtext, Bind::Json(j)) => json_eq(v, j),
        _ => compare(kind, v, b) == Some(Ordering::Equal),
    }
}

/// Compares a row value with a parameter. Text compares by code point (as `COLLATE "C"`).
fn compare(kind: &Kind, v: &Value, b: &Bind) -> Option<Ordering> {
    match b {
        Bind::Text(s) => Some(v.as_str()?.cmp(s.as_str())),
        Bind::Int(i) => v.as_f64()?.partial_cmp(&(*i as f64)),
        Bind::Float(f) => v.as_f64()?.partial_cmp(f),
        Bind::Bool(x) => Some(v.as_bool()?.cmp(x)),
        Bind::Date(d) => Some(
            NaiveDate::parse_from_str(v.as_str()?, "%Y-%m-%d")
                .ok()?
                .cmp(d),
        ),
        Bind::Time(t) => Some(parse_datetime(v)?.cmp(t)),
        Bind::Uuid(u) => Some(Uuid::parse_str(v.as_str()?).ok()?.cmp(u)),
        Bind::Json(j) if matches!(kind, Kind::Json | Kind::Richtext) => {
            if json_eq(v, j) {
                Some(Ordering::Equal)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// JSON equality as jsonb has it: numbers compare by value, object key order is irrelevant.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, p)| y.get(k).is_some_and(|q| json_eq(p, q)))
        }
        _ => a == b,
    }
}
