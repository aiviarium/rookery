//! Raw SQL's pure logic: names and quoting, view definitions, grant diffs, migration files. No database.

use std::collections::BTreeSet;

use rookery_store::sql::grants::{self, Grant, Object};
use rookery_store::sql::migrations::{self, SqlMigration};
use rookery_store::sql::names::{self, Names};
use rookery_store::sql::scram;
use rookery_store::sql::value::{self, Param};
use serde_json::json;

use super::common::defs;

/// The longest id and the shortest, and ones with every character the manifest schema allows.
const EDGE_IDS: &[&str] = &[
    "a",
    "a-",
    "a--b",
    "z9",
    "a-b-c",
    "abcdefghij-abcdefghij-abcdefghij-abcdefg", // 40
];

#[test]
fn names_are_derived_from_the_id_and_every_one_is_one_identifier() {
    for id in EDGE_IDS {
        for tag in [None, Some("ab"), Some("t0123456789a")] {
            let n = Names::new(id, tag).unwrap();
            let u = id.replace('-', "_");
            assert_eq!(n.raw_schema, format!("x_{u}"));
            assert_eq!(n.view_schema, format!("v_{u}"));
            assert_eq!(n.data_schema, format!("p_{u}"));
            assert_eq!(
                n.role,
                match tag {
                    None => format!("rookery_x_{u}"),
                    Some(t) => format!("rookery_{t}_x_{u}"),
                }
            );
            // Postgres truncates identifiers at 63 bytes; a name that long would silently be another's.
            for name in [&n.role, &n.raw_schema, &n.view_schema, &n.data_schema] {
                assert!(name.len() <= 63, "{name}");
                assert!(
                    name.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                );
                assert_eq!(names::quote_ident(name), format!("\"{name}\""));
            }
        }
    }
}

#[test]
fn ids_the_schema_allows_but_the_store_does_not_are_refused_not_truncated() {
    // The manifest schema allows 63 characters; a role name has to fit 63 with its prefix.
    let long = format!("a{}", "b".repeat(40));
    assert_eq!(long.len(), 41);
    for bad in [
        long.as_str(),
        "",
        "A",
        "1a",
        "-a",
        "a_b",
        "a b",
        "a\"b",
        "a'b",
        "a;drop schema rookery",
        "a.b",
        "é",
        "a\0b",
    ] {
        assert!(Names::new(bad, None).is_err(), "{bad:?}");
    }
}

#[test]
fn tagged_and_untagged_roles_never_meet() {
    // Every tag ever written, against every id that could make the same name untagged.
    let mut seen = BTreeSet::new();
    for id in ["a", "t1-x", "ab-x", "x", "x-a", "ab"] {
        for tag in [None, Some("ab"), Some("t1"), Some("x1")] {
            let n = Names::new(id, tag).unwrap();
            assert!(seen.insert(n.role.clone()), "{} is made twice", n.role);
        }
    }
    assert!(names::check_tag("a").is_err());
    assert!(names::check_tag("A1").is_err());
    assert!(names::check_tag("a_b").is_err());
    assert!(names::check_tag("abcdefghijklm").is_err());
    assert!(names::check_tag("ab").is_ok());
}

#[test]
fn quoting_keeps_a_name_one_identifier_and_refuses_unsafe_literals() {
    assert_eq!(names::quote_ident("a\"b"), "\"a\"\"b\"");
    assert_eq!(names::quote_ident("a;--"), "\"a;--\"");
    assert_eq!(names::qualified("x_a", "t"), "\"x_a\".\"t\"");
    assert_eq!(names::quote_literal("abc$1:+/=").unwrap(), "'abc$1:+/='");
    for bad in ["a'b", "a\\b", "a\nb", "a\0b"] {
        assert!(names::quote_literal(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn views_are_the_fields_plus_the_system_columns_and_never_the_search_column() {
    let n = Names::new("demo-app", None).unwrap();
    let d = defs(json!([{
        "name": "notes", "scope": "space", "trash": true,
        "fields": [{ "name": "title", "type": "text", "search": true }, { "name": "latencyMs", "type": "integer" }]
    }]));
    let v = names::views(&n, &d).unwrap();
    assert_eq!(
        v.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
        ["notes", "notes__all"]
    );
    assert_eq!(
        v[0].create,
        "CREATE VIEW \"v_demo_app\".\"notes\" AS SELECT \"id\", \"space_id\", \"user_id\", \"created_at\", \"updated_at\", \
         \"created_by\", \"updated_by\", \"rev\", \"title\", \"latencyMs\" FROM \"p_demo_app\".\"notes\" WHERE deleted_at IS NULL"
    );
    assert!(v[1].create.contains("\"rev\", \"deleted_at\", \"title\""));
    assert!(!v[1].create.contains("WHERE"));
    assert!(v.iter().all(|v| !v.create.contains("_search")));
    // A collection `notes__all` would have the name of notes' second view.
    let clash = defs(json!([
        { "name": "notes", "scope": "global", "fields": [] },
        { "name": "notes__all", "scope": "global", "fields": [] }
    ]));
    assert!(names::views(&n, &clash).is_err());
}

#[test]
fn a_role_is_due_connect_usage_and_select_on_its_views_and_nothing_else() {
    let n = Names::new("demo", None).unwrap();
    let d = defs(json!([{ "name": "notes", "scope": "global", "fields": [] }]));
    let views = names::views(&n, &d).unwrap();
    let due = grants::due(&n, "rookery", &views);
    let sql: Vec<String> = due.iter().map(|g| g.grant_sql(&n.role)).collect();
    assert_eq!(
        sql,
        [
            "GRANT CONNECT ON DATABASE \"rookery\" TO \"rookery_x_demo\"",
            "GRANT USAGE ON SCHEMA \"v_demo\" TO \"rookery_x_demo\"",
            "GRANT SELECT ON TABLE \"v_demo\".\"notes\" TO \"rookery_x_demo\"",
            "GRANT SELECT ON TABLE \"v_demo\".\"notes__all\" TO \"rookery_x_demo\"",
        ]
    );
}

#[test]
fn the_grant_diff_revokes_what_is_not_due_and_grants_what_is_missing() {
    let n = Names::new("demo", Some("ab")).unwrap();
    let d = defs(json!([{ "name": "notes", "scope": "global", "fields": [] }]));
    let due = grants::due(&n, "rookery", &names::views(&n, &d).unwrap());
    // Holding exactly what is due: nothing to do.
    assert_eq!(grants::diff(&due, &due), grants::Diff::default());
    // Holding nothing: everything is granted.
    let from_nothing = grants::diff(&BTreeSet::new(), &due);
    assert_eq!(from_nothing.grant.len(), 4);
    assert!(from_nothing.revoke.is_empty());
    // Holding extras (a schema of another plugin, a table of the kernel, a function, a column) and missing one view.
    let mut current = due.clone();
    let missing = Grant {
        on: Object::Table("\"v_demo\".\"notes__all\"".into()),
        privilege: "SELECT".into(),
    };
    current.remove(&missing);
    let extras = [
        Grant {
            on: Object::Schema("rookery".into()),
            privilege: "USAGE".into(),
        },
        Grant {
            on: Object::Schema("v_other".into()),
            privilege: "USAGE".into(),
        },
        Grant {
            on: Object::Table("\"p_demo\".\"notes\"".into()),
            privilege: "INSERT".into(),
        },
        Grant {
            on: Object::Table("\"v_demo\".\"notes\"".into()),
            privilege: "UPDATE".into(),
        },
        Grant {
            on: Object::Function("rookery.f(integer)".into()),
            privilege: "EXECUTE".into(),
        },
        Grant {
            on: Object::Column {
                table: "\"rookery\".\"users\"".into(),
                column: "email".into(),
            },
            privilege: "SELECT".into(),
        },
        Grant {
            on: Object::Database("rookery".into()),
            privilege: "CREATE".into(),
        },
    ];
    current.extend(extras.iter().cloned());
    let diff = grants::diff(&current, &due);
    assert_eq!(diff.grant, vec![missing]);
    assert_eq!(diff.revoke.len(), extras.len());
    let revokes: Vec<String> = diff.revoke.iter().map(|g| g.revoke_sql(&n.role)).collect();
    assert!(
        revokes.contains(
            &"REVOKE SELECT (\"email\") ON TABLE \"rookery\".\"users\" FROM \"rookery_ab_x_demo\""
                .to_string()
        )
    );
    assert!(revokes.contains(
        &"REVOKE EXECUTE ON FUNCTION rookery.f(integer) FROM \"rookery_ab_x_demo\"".to_string()
    ));
    assert!(
        revokes.contains(
            &"REVOKE CREATE ON DATABASE \"rookery\" FROM \"rookery_ab_x_demo\"".to_string()
        )
    );
}

#[test]
fn migration_files_are_ordered_by_number_and_named_in_the_error() {
    let f = |n: &str| (n.to_string(), format!("-- {n}"));
    let ordered = migrations::order(vec![
        f("0010_later.sql"),
        f("0002_add_scores.sql"),
        f("0001_init.sql"),
    ])
    .unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|m| (m.version, m.file.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, "0001_init.sql"),
            (2, "0002_add_scores.sql"),
            (10, "0010_later.sql")
        ]
    );
    for bad in [
        "init.sql",
        "0001.sql",
        "0000_zero.sql",
        "0001_init.txt",
        "x1_init.sql",
        "0001_a b.sql",
        "0001_.sql",
        "1234567890_x.sql",
    ] {
        let e = migrations::order(vec![f(bad)]).unwrap_err().to_string();
        assert!(e.contains(bad), "{bad}: {e}");
    }
    let e = migrations::order(vec![f("0001_a.sql"), f("1_b.sql")])
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("0001_a.sql") && e.contains("1_b.sql") && e.contains("same number"),
        "{e}"
    );
}

#[test]
fn applied_migrations_are_not_pending_and_a_changed_one_is_refused() {
    let files = migrations::order(vec![
        ("0001_a.sql".into(), "create table a ();".into()),
        ("0002_b.sql".into(), "create table b ();".into()),
    ])
    .unwrap();
    let sum = |i: usize| files[i].checksum();
    let pending = migrations::pending(&files, &[(1, sum(0))]).unwrap();
    assert_eq!(pending.iter().map(|m| m.version).collect::<Vec<_>>(), [2]);
    assert!(
        migrations::pending(&files, &[(1, sum(0)), (2, sum(1))])
            .unwrap()
            .is_empty()
    );
    let e = migrations::pending(&files, &[(1, "0".repeat(64))])
        .unwrap_err()
        .to_string();
    assert!(e.contains("0001_a.sql"), "{e}");
    let _: &SqlMigration = &files[0];
}

#[test]
fn the_password_verifier_has_the_scram_shape() {
    let v = scram::verifier_with_salt("pencil", &[1; 16]);
    assert!(
        v.starts_with("SCRAM-SHA-256$4096:AQEBAQEBAQEBAQEBAQEBAQ==$"),
        "{v}"
    );
    assert_eq!(v.matches(':').count(), 2);
    assert_ne!(scram::verifier("pencil"), scram::verifier("pencil"));
}

#[test]
fn parameters_are_read_from_json_without_losing_bytes_or_vectors() {
    assert_eq!(value::param(&json!(null)).unwrap(), Param::Null);
    assert_eq!(value::param(&json!(7)).unwrap(), Param::Int(7));
    assert_eq!(value::param(&json!(1.5)).unwrap(), Param::Float(1.5));
    assert_eq!(
        value::param(&json!({"$bytes": "AAH/"})).unwrap(),
        Param::Bytes(vec![0, 1, 255])
    );
    assert_eq!(
        value::param(&json!({"$vector": [1, 2.5]})).unwrap(),
        Param::Vector(vec![1.0, 2.5])
    );
    // Any other object is jsonb, including one that merely has a key of that name next to others.
    assert!(matches!(
        value::param(&json!({"$bytes": "AAH/", "x": 1})).unwrap(),
        Param::Json(_)
    ));
    assert!(matches!(
        value::param(&json!({"a": {"b": 1}})).unwrap(),
        Param::Json(_)
    ));
    assert!(matches!(
        value::param(&json!([1, "a", null])).unwrap(),
        Param::Array(_)
    ));
    assert!(value::param(&json!({"$bytes": "***"})).is_err());
    assert!(value::param(&json!({"$vector": ["a"]})).is_err());
}

#[test]
fn a_log_collection_has_one_view_without_the_columns_of_a_mutable_table() {
    let n = Names::new("demo-app", None).unwrap();
    let d = defs(json!([{
        "name": "pings", "kind": "log", "scope": "space",
        "fields": [{ "name": "target", "type": "text" }]
    }]));
    let v = names::views(&n, &d).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(
        v[0].create,
        "CREATE VIEW \"v_demo_app\".\"pings\" AS SELECT \"seq\", \"id\", \"space_id\", \"user_id\", \"created_at\", \
         \"created_by\", \"target\" FROM \"p_demo_app\".\"pings\""
    );
}
