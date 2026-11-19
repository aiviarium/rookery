//! The numbered migrations in `migrations/`, embedded. Each runs once, in its own transaction, under an
//! advisory lock so that two servers starting at once do not both run it. A migration that changed after
//! it ran stops the start.

use sha2::{Digest, Sha256};
use sqlx::{Connection, PgPool};

use crate::error::{Error, Result};

pub(crate) struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// One entry per file in `migrations/`, in order. A new migration is a new file and a new entry; one
/// that ran is never edited.
pub(crate) static MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "auth",
        sql: include_str!("../migrations/0001_auth.sql"),
    },
    Migration {
        version: 2,
        name: "facilities",
        sql: include_str!("../migrations/0002_facilities.sql"),
    },
    Migration {
        version: 3,
        name: "leader",
        sql: include_str!("../migrations/0003_leader.sql"),
    },
    Migration {
        version: 4,
        name: "hosts",
        sql: include_str!("../migrations/0004_hosts.sql"),
    },
    Migration {
        version: 5,
        name: "plugins",
        sql: include_str!("../migrations/0005_plugins.sql"),
    },
    Migration {
        version: 6,
        name: "terminals",
        sql: include_str!("../migrations/0006_terminals.sql"),
    },
    Migration {
        version: 7,
        name: "data",
        sql: include_str!("../migrations/0007_data.sql"),
    },
    Migration {
        version: 8,
        name: "sql",
        sql: include_str!("../migrations/0008_sql.sql"),
    },
];

/// Any number taken by no other advisory lock of the server.
const MIGRATION_LOCK: i64 = 0x726f_6f6b_6d69_6772;

pub(crate) fn checksum(sql: &str) -> Vec<u8> {
    Sha256::digest(sql.as_bytes()).to_vec()
}

pub(crate) async fn run(pool: &PgPool) -> Result<()> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *conn)
        .await?;
    let done = apply(&mut conn).await;
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *conn)
        .await?;
    done
}

async fn apply(conn: &mut sqlx::PgConnection) -> Result<()> {
    sqlx::query("CREATE SCHEMA IF NOT EXISTS rookery")
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS rookery.migrations (
            version    integer PRIMARY KEY,
            name       text NOT NULL,
            checksum   bytea NOT NULL,
            applied_at timestamptz NOT NULL DEFAULT now()
        )",
    )
    .execute(&mut *conn)
    .await?;
    let ran: Vec<(i32, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM rookery.migrations ORDER BY version")
            .fetch_all(&mut *conn)
            .await?;
    for (version, sum) in &ran {
        match MIGRATIONS.iter().find(|m| m.version == *version) {
            Some(m) if checksum(m.sql) == *sum => {}
            Some(m) => {
                return Err(Error::Invalid(format!(
                    "migration {version} ({}) changed after it ran; a migration that ran is never edited",
                    m.name
                )));
            }
            None => {
                return Err(Error::Invalid(format!(
                    "the database has migration {version}, which this rookery does not know: it is older than the one that set the database up"
                )));
            }
        }
    }
    for m in MIGRATIONS
        .iter()
        .filter(|m| !ran.iter().any(|(v, _)| *v == m.version))
    {
        let mut tx = conn.begin().await?;
        sqlx::raw_sql(m.sql).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO rookery.migrations (version, name, checksum) VALUES ($1, $2, $3)")
            .bind(m.version)
            .bind(m.name)
            .bind(checksum(m.sql))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// A fingerprint of the migrations, for test templates.
#[cfg(feature = "testing")]
pub(crate) fn fingerprint() -> String {
    let mut digest = Sha256::new();
    for m in MIGRATIONS {
        digest.update(m.version.to_le_bytes());
        digest.update(checksum(m.sql));
    }
    hex::encode(&digest.finalize()[..6])
}
