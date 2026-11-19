//! The database adapter for the independent structured filter.

use rookery_filter::{Cmp, Fields, Node, Op, Operand};
use serde_json::Value;
use sqlx::{Postgres, QueryBuilder};

use super::Collection;
use super::values::{Bind, Kind, SqlBind, SqlKind};
use crate::error::Result;

impl Fields for Collection {
    fn kind(&self, field: &str) -> Option<Kind> {
        self.column(field).map(|(_, kind)| kind)
    }
}

/// A parsed, validated filter over one collection's rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter(rookery_filter::Filter);

impl Filter {
    /// A filter that matches every row.
    pub fn all() -> Self {
        Self(rookery_filter::Filter::all())
    }

    /// Parses a `where` object against a collection's fields.
    pub fn parse(coll: &Collection, value: &Value) -> Result<Self> {
        Ok(Self(rookery_filter::Filter::parse(coll, value)?))
    }

    /// Whether a row matches.
    pub fn matches(&self, row: &Value) -> bool {
        self.0.matches(row)
    }

    pub(crate) fn push_sql(&self, coll: &Collection, qb: &mut QueryBuilder<Postgres>) {
        push_node(self.0.node(), coll, qb);
    }
}

fn push_node(node: &Node, coll: &Collection, qb: &mut QueryBuilder<Postgres>) {
    match node {
        Node::And(items) | Node::Or(items) => {
            let and = matches!(node, Node::And(_));
            if items.is_empty() {
                qb.push(if and { "TRUE" } else { "FALSE" });
                return;
            }
            qb.push("(");
            for (i, n) in items.iter().enumerate() {
                if i > 0 {
                    qb.push(if and { " AND " } else { " OR " });
                }
                push_node(n, coll, qb);
            }
            qb.push(")");
        }
        Node::Not(inner) => {
            // Inner NULLs count as false, so NOT stays two-valued.
            qb.push("NOT coalesce(");
            push_node(inner, coll, qb);
            qb.push(", false)");
        }
        Node::Cmp(c) => push_cmp(c, &coll.column(&c.field).expect("validated field").0, qb),
    }
}

fn is_textual(k: &Kind) -> bool {
    matches!(k, Kind::Text | Kind::Enum(_) | Kind::Ref(_))
}

fn push_cmp(c: &Cmp, column: &str, qb: &mut QueryBuilder<Postgres>) {
    let ty = c.kind.sql_type();
    match (&c.op, &c.value) {
        (Op::Eq, Operand::One(b)) if b.is_null() => {
            qb.push(format!("{} IS NULL", column));
        }
        (Op::Eq, Operand::One(b)) => {
            qb.push(format!("{} = ", column));
            b.clone().push(qb, ty);
        }
        (Op::Ne, Operand::One(b)) if b.is_null() => {
            qb.push(format!("{} IS NOT NULL", column));
        }
        (Op::Ne, Operand::One(b)) => {
            qb.push(format!("{} IS DISTINCT FROM ", column));
            b.clone().push(qb, ty);
        }
        (Op::In | Op::Nin, Operand::Many(items)) => {
            if c.op == Op::Nin {
                qb.push("NOT ");
            }
            push_in(c, column, items, qb);
        }
        (Op::Lt | Op::Lte | Op::Gt | Op::Gte, Operand::One(b)) => {
            let sym = match c.op {
                Op::Lt => "<",
                Op::Lte => "<=",
                Op::Gt => ">",
                _ => ">=",
            };
            let collate = if is_textual(&c.kind) {
                " COLLATE \"C\""
            } else {
                ""
            };
            qb.push(format!("{}{collate} {sym} ", column));
            b.clone().push(qb, ty);
        }
        (Op::Contains, Operand::One(b)) => {
            qb.push(format!("{} @> ", column));
            b.clone().push(qb, "text[]");
        }
        _ => {
            qb.push("FALSE");
        }
    }
}

/// `(coalesce(col IN (...), false) OR col IS NULL)`: two-valued, so NOT can wrap it.
fn push_in(c: &Cmp, column: &str, items: &[Bind], qb: &mut QueryBuilder<Postgres>) {
    let ty = c.kind.sql_type();
    let has_null = items.iter().any(Bind::is_null);
    let values: Vec<&Bind> = items.iter().filter(|b| !b.is_null()).collect();
    qb.push("(");
    if values.is_empty() {
        qb.push("FALSE");
    } else {
        qb.push(format!("coalesce({} IN (", column));
        for (i, b) in values.into_iter().enumerate() {
            if i > 0 {
                qb.push(", ");
            }
            (*b).clone().push(qb, ty);
        }
        qb.push("), false)");
    }
    if has_null {
        qb.push(format!(" OR {} IS NULL", column));
    }
    qb.push(")");
}
