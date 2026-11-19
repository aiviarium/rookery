//! The read views `v_<plugin>.<collection>` and `<collection>__all`, kept in step with the collection
//! tables. A view depends on its table's columns, so a change to a collection drops its plugin's views first
//! and creates them again from the new definition in the same transaction.

use sqlx::PgConnection;

use super::names::{Names, View, definition_hash, qualified, quote_ident, quote_literal, views};
use crate::collections::CollectionDef;
use crate::error::Result;

/// Whether the plugin has raw SQL enabled (a row in `rookery.plugin_sql`).
pub(crate) async fn is_enabled(conn: &mut PgConnection, plugin: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rookery.plugin_sql WHERE plugin = $1)")
            .bind(plugin)
            .fetch_one(conn)
            .await?,
    )
}

/// The views that exist in a plugin's view schema, with the hash of their definition.
async fn existing(conn: &mut PgConnection, schema: &str) -> Result<Vec<(String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT c.relname::text, obj_description(c.oid, 'pg_class') FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind = 'v'",
    )
    .bind(schema)
    .fetch_all(conn)
    .await?)
}

/// Drops every view of the plugin (before a change to its tables).
pub(crate) async fn drop_all(conn: &mut PgConnection, names: &Names) -> Result<()> {
    for (name, _) in existing(&mut *conn, &names.view_schema).await? {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP VIEW IF EXISTS {}",
            qualified(&names.view_schema, &name)
        )))
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Makes the plugin's views the ones `defs` imply: drops the others, creates the missing or changed ones
/// and lets the plugin's role select from them. Returns the views.
pub(crate) async fn sync(
    conn: &mut PgConnection,
    names: &Names,
    defs: &[CollectionDef],
) -> Result<Vec<View>> {
    let wanted = views(names, defs)?;
    let have = existing(&mut *conn, &names.view_schema).await?;
    for (name, _) in &have {
        if !wanted.iter().any(|v| &v.name == name) {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP VIEW IF EXISTS {}",
                qualified(&names.view_schema, name)
            )))
            .execute(&mut *conn)
            .await?;
        }
    }
    for v in &wanted {
        let hash = definition_hash(&v.create);
        let qualified_name = qualified(&names.view_schema, &v.name);
        match have.iter().find(|(n, _)| n == &v.name) {
            Some((_, Some(h))) if *h == hash => continue,
            Some(_) => {
                sqlx::query(sqlx::AssertSqlSafe(format!("DROP VIEW {qualified_name}")))
                    .execute(&mut *conn)
                    .await?;
            }
            None => {}
        }
        sqlx::query(sqlx::AssertSqlSafe(v.create.to_string()))
            .execute(&mut *conn)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "COMMENT ON VIEW {qualified_name} IS {}",
            quote_literal(&hash)?
        )))
        .execute(&mut *conn)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "GRANT SELECT ON {qualified_name} TO {}",
            quote_ident(&names.role)
        )))
        .execute(&mut *conn)
        .await?;
    }
    Ok(wanted)
}
