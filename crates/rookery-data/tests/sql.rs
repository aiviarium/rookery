//! `sql.*` through the methods: a plugin with `database.sql` applied with its SQL migrations, its own
//! statements and transactions, and what neither it nor a person reaches.

use rookery_data::plugins::{self, PluginData};
use rookery_data::sql::Database;
use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Caller, Methods, RpcError, test_kernel};
use rookery_store::sql::migrations::SqlMigration;
use serde_json::{Value, json};

fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_data::register(&mut methods);
    methods
}

fn code(result: Result<Value, RpcError>) -> &'static str {
    result
        .err()
        .map(|e| e.error_code().as_str())
        .unwrap_or("ok")
}

fn ledger(direct: bool) -> PluginData {
    PluginData {
        plugin: "ledger".into(),
        collections: json!([{
            "name": "entries",
            "scope": "global",
            "fields": [{ "name": "amount", "type": "integer" }]
        }]),
        database: Database {
            sql: true,
            direct,
            service_migrations: false,
        },
        sql_migrations: vec![SqlMigration {
            version: 1,
            file: "0001_totals.sql".into(),
            sql: "create table totals (day date primary key, amount bigint not null)".into(),
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_plugin_runs_sql_in_its_own_schema_and_reads_its_collections() {
    let k: TestKernel = test_kernel!(methods());
    plugins::apply(&k.kernel, &ledger(false), &plugins::NoTransforms)
        .await
        .unwrap();
    let ledger = Caller::Plugin("ledger".into());
    k.call(
        &ledger,
        "data.insert",
        json!({ "collection": "entries", "values": { "amount": 5 } }),
    )
    .await
    .unwrap();
    // Its collections are views it reads; its migration made a table it writes.
    let rows = k
        .call(
            &ledger,
            "sql.query",
            json!({ "sql": "select amount from entries", "params": [] }),
        )
        .await
        .unwrap();
    assert_eq!(rows["rows"], json!([{ "amount": 5 }]));
    let n = k
        .call(
            &ledger,
            "sql.execute",
            json!({ "sql": "insert into totals values ($1::date, $2)", "params": ["2026-11-19", 5] }),
        )
        .await
        .unwrap();
    assert_eq!(n["rows"], 1);
    assert_eq!(
        code(
            k.call(
                &ledger,
                "sql.execute",
                json!({ "sql": "update entries set amount = 6", "params": [] })
            )
            .await
        ),
        "FORBIDDEN"
    );
    // Rookery's own tables and other plugins' are out of reach.
    let refused = k
        .call(
            &ledger,
            "sql.query",
            json!({ "sql": "select * from rookery.users", "params": [] }),
        )
        .await;
    assert_eq!(code(refused), "FORBIDDEN");
    // A transaction commits what it wrote; a rolled back one keeps nothing.
    let tx = k.call(&ledger, "sql.begin", json!({})).await.unwrap()["tx"].clone();
    k.call(
        &ledger,
        "sql.txExecute",
        json!({ "tx": tx, "sql": "insert into totals values ('2026-11-20', 1)", "params": [] }),
    )
    .await
    .unwrap();
    k.call(&ledger, "sql.commit", json!({ "tx": tx }))
        .await
        .unwrap();
    let tx = k.call(&ledger, "sql.begin", json!({})).await.unwrap()["tx"].clone();
    k.call(
        &ledger,
        "sql.txExecute",
        json!({ "tx": tx, "sql": "delete from totals", "params": [] }),
    )
    .await
    .unwrap();
    k.call(&ledger, "sql.rollback", json!({ "tx": tx }))
        .await
        .unwrap();
    let count = k
        .call(
            &ledger,
            "sql.query",
            json!({ "sql": "select count(*)::int as n from totals", "params": [] }),
        )
        .await
        .unwrap();
    assert_eq!(count["rows"][0]["n"], 2);
    // Raw SQL is a plugin's own: a person, even an admin, does not run it.
    let admin = k.person("Maria", true).await;
    assert_eq!(
        code(
            k.call(
                &admin,
                "sql.query",
                json!({ "plugin": "ledger", "sql": "select 1", "params": [] })
            )
            .await
        ),
        "FORBIDDEN"
    );
    // Without direct connections there is no credential to give out.
    assert_eq!(
        code(
            k.call(&admin, "sql.credential", json!({ "plugin": "ledger" }))
                .await
        ),
        "NOT_FOUND"
    );
    k.close().await;
}

#[tokio::test]
async fn a_direct_credential_is_an_admins_and_a_plugin_that_drops_sql_loses_its_role() {
    let k: TestKernel = test_kernel!(methods());
    plugins::apply(&k.kernel, &ledger(true), &plugins::NoTransforms)
        .await
        .unwrap();
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let first = k
        .call(&admin, "sql.credential", json!({ "plugin": "ledger" }))
        .await
        .unwrap();
    assert!(first["url"].as_str().unwrap().starts_with("postgres"));
    assert_eq!(
        code(
            k.call(&ops, "sql.credential", json!({ "plugin": "ledger" }))
                .await
        ),
        "FORBIDDEN"
    );
    let rotated = k
        .call(
            &admin,
            "sql.rotateCredential",
            json!({ "plugin": "ledger" }),
        )
        .await
        .unwrap();
    assert_ne!(rotated["url"], first["url"]);
    // The next load without database.sql takes raw SQL away; the data stays.
    let mut dropped = ledger(false);
    dropped.database = Database::default();
    dropped.sql_migrations.clear();
    plugins::apply(&k.kernel, &dropped, &plugins::NoTransforms)
        .await
        .unwrap();
    assert!(!k.db.store.sql().is_enabled("ledger").await.unwrap());
    let ledger = Caller::Plugin("ledger".into());
    assert_ne!(
        code(
            k.call(
                &ledger,
                "sql.query",
                json!({ "sql": "select 1", "params": [] })
            )
            .await
        ),
        "ok"
    );
    k.close().await;
}
