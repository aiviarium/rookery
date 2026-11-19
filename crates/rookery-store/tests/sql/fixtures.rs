//! Raw SQL fixtures: a plugin with a few collections whose role, views and schema are made.

use super::common::defs;
use rookery_store::collections::CollectionDef;
use rookery_store::testing::TestDb;
use serde_json::json;
use sqlx::{Connection, PgConnection};

pub fn notes_defs() -> Vec<CollectionDef> {
    defs(json!([
        { "name": "notes", "scope": "space", "trash": true, "fields": [
            { "name": "title", "type": "text", "search": true },
            { "name": "n", "type": "integer" },
            { "name": "raw", "type": "bytes", "maxBytes": 64 },
            { "name": "latencyMs", "type": "integer" }
        ]},
        { "name": "tags", "scope": "global", "fields": [{ "name": "name", "type": "text", "unique": true }] }
    ]))
}

/// Applies the collections of `plugin` and gives it raw SQL.
pub async fn setup(db: &TestDb, plugin: &str, direct: bool) {
    db.store
        .collections()
        .apply(plugin, &notes_defs())
        .await
        .expect("apply");
    db.store.sql().enable(plugin, direct).await.expect("enable");
}

/// A connection as the plugin's role, the way a service with `ROOKERY_DATABASE_URL` has one.
pub async fn direct(db: &TestDb, plugin: &str) -> PgConnection {
    let url = db
        .store
        .sql()
        .connection_url(plugin)
        .await
        .expect("credential")
        .expect("the plugin has direct connections");
    // No statement cache: `DISCARD ALL` (one of the things tests run) would drop the prepared statements.
    let opts: sqlx::postgres::PgConnectOptions = url.parse().expect("the url");
    PgConnection::connect_with(&opts.statement_cache_capacity(0))
        .await
        .expect("connecting as the plugin's role")
}

pub fn code(e: &sqlx::Error) -> String {
    match e {
        sqlx::Error::Database(d) => d.code().map(|c| c.into_owned()).unwrap_or_default(),
        other => format!("{other}"),
    }
}
