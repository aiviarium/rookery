//! The server's records of terminals: sessions on hosts, whose they are, their size and how they
//! ended. A host is named here by its name and kept by its id, so a rename carries its terminals.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};

pub struct Terminals<'a> {
    pub(crate) pool: &'a PgPool,
    /// The leader's epoch every write names: see [`crate::leader`].
    pub(crate) fence: Option<i64>,
}

/// One terminal, with its host's name.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Terminal {
    pub id: String,
    /// The host's name now.
    pub host: String,
    pub session: String,
    pub owner: Option<Uuid>,
    pub run_as: String,
    pub title: String,
    pub command: Value,
    pub cwd: Option<String>,
    pub state: String,
    pub exit_code: Option<i32>,
    pub cols: i32,
    pub rows: i32,
    pub offset: i64,
    pub last_input: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

/// A terminal to record.
pub struct New<'a> {
    pub id: &'a str,
    pub host: &'a str,
    pub session: &'a str,
    pub owner: Option<Uuid>,
    pub run_as: &'a str,
    pub title: &'a str,
    pub command: &'a Value,
    pub cwd: Option<&'a str>,
    pub cols: i32,
    pub rows: i32,
}

const COLUMNS: &str = "t.id, h.name AS host, t.session, t.owner, t.run_as, t.title, t.command, t.cwd, \
     t.state, t.exit_code, t.cols, t.rows, t.\"offset\", t.last_input, t.created_at, t.ended_at";

/// What `list` returns.
#[derive(Debug, Clone, Default)]
pub struct Query<'a> {
    pub host: Option<&'a str>,
    /// Only this person's.
    pub owner: Option<Uuid>,
    /// Also ended ones, the newest 50 of them.
    pub ended: bool,
}

impl Terminals<'_> {
    /// Records a terminal on a host by its name. `CONFLICT` when the session is one already.
    pub async fn insert(&self, new: New<'_>) -> Result<Terminal> {
        let sql = format!(
            "WITH _f AS MATERIALIZED (SELECT rookery.fence($11) AS ok),
             t AS (
                 INSERT INTO rookery.terminals (id, host, session, owner, run_as, title, command, cwd, cols, rows)
                 SELECT $1, h.id, $3, $4, $5, $6, $7, $8, $9, $10
                 FROM rookery.hosts h CROSS JOIN _f
                 WHERE _f.ok AND h.name = $2
                 ON CONFLICT (host, session) DO NOTHING
                 RETURNING *
             )
             SELECT {COLUMNS} FROM t JOIN rookery.hosts h ON h.id = t.host"
        );
        let found: Option<Terminal> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(new.id)
            .bind(new.host)
            .bind(new.session)
            .bind(new.owner)
            .bind(new.run_as)
            .bind(new.title)
            .bind(new.command)
            .bind(new.cwd)
            .bind(new.cols)
            .bind(new.rows)
            .bind(self.fence)
            .fetch_optional(self.pool)
            .await?;
        match found {
            Some(found) => Ok(found),
            None if self.by_session(new.host, new.session).await?.is_some() => Err(
                Error::Conflict(format!("the session {} is a terminal already", new.session)),
            ),
            None => Err(Error::NotFound(format!("no host {}", new.host))),
        }
    }

    pub async fn get(&self, id: &str) -> Result<Option<Terminal>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM rookery.terminals t JOIN rookery.hosts h ON h.id = t.host WHERE t.id = $1"
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(self.pool)
            .await?)
    }

    pub async fn by_session(&self, host: &str, session: &str) -> Result<Option<Terminal>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM rookery.terminals t JOIN rookery.hosts h ON h.id = t.host
             WHERE h.name = $1 AND t.session = $2"
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(host)
            .bind(session)
            .fetch_optional(self.pool)
            .await?)
    }

    /// Running terminals first, newest first, then up to 50 that ended when asked for.
    pub async fn list(&self, query: Query<'_>) -> Result<Vec<Terminal>> {
        let sql = format!(
            "(SELECT {COLUMNS} FROM rookery.terminals t JOIN rookery.hosts h ON h.id = t.host
              WHERE t.state = 'running' AND ($1::text IS NULL OR h.name = $1) AND ($2::uuid IS NULL OR t.owner = $2)
              ORDER BY t.created_at DESC)
             UNION ALL
             (SELECT {COLUMNS} FROM rookery.terminals t JOIN rookery.hosts h ON h.id = t.host
              WHERE $3 AND t.state <> 'running' AND ($1::text IS NULL OR h.name = $1) AND ($2::uuid IS NULL OR t.owner = $2)
              ORDER BY t.ended_at DESC NULLS LAST LIMIT 50)"
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(query.host)
            .bind(query.owner)
            .bind(query.ended)
            .fetch_all(self.pool)
            .await?)
    }

    pub async fn set_title(&self, id: &str, title: &str) -> Result<bool> {
        let done = sqlx::query(
            "WITH _f AS MATERIALIZED (SELECT rookery.fence($3) AS ok)
             UPDATE rookery.terminals SET title = $2 FROM _f WHERE id = $1 AND _f.ok",
        )
        .bind(id)
        .bind(title)
        .bind(self.fence)
        .execute(self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn set_size(&self, id: &str, cols: i32, rows: i32) -> Result<()> {
        sqlx::query(
            "WITH _f AS MATERIALIZED (SELECT rookery.fence($4) AS ok)
             UPDATE rookery.terminals SET cols = $2, rows = $3 FROM _f WHERE id = $1 AND _f.ok",
        )
        .bind(id)
        .bind(cols)
        .bind(rows)
        .bind(self.fence)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    /// The output offset seen, which only grows.
    pub async fn set_offset(&self, id: &str, offset: i64) -> Result<()> {
        sqlx::query(
            "WITH _f AS MATERIALIZED (SELECT rookery.fence($3) AS ok)
             UPDATE rookery.terminals SET \"offset\" = greatest(\"offset\", $2) FROM _f WHERE id = $1 AND _f.ok",
        )
        .bind(id)
        .bind(offset)
        .bind(self.fence)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_last_input(&self, id: &str, input: &Value) -> Result<()> {
        sqlx::query(
            "WITH _f AS MATERIALIZED (SELECT rookery.fence($3) AS ok)
             UPDATE rookery.terminals SET last_input = $2 FROM _f WHERE id = $1 AND _f.ok",
        )
        .bind(id)
        .bind(input)
        .bind(self.fence)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    /// Marks a running terminal ended. Returns whether it was running.
    pub async fn end(&self, id: &str, state: &str, exit_code: Option<i32>) -> Result<bool> {
        let done = sqlx::query(
            "WITH _f AS MATERIALIZED (SELECT rookery.fence($4) AS ok)
             UPDATE rookery.terminals SET state = $2, exit_code = $3, ended_at = now()
             FROM _f WHERE id = $1 AND state = 'running' AND _f.ok",
        )
        .bind(id)
        .bind(state)
        .bind(exit_code)
        .bind(self.fence)
        .execute(self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Forgets a terminal.
    pub async fn delete(&self, id: &str) -> Result<bool> {
        let done = sqlx::query(
            "WITH _f AS MATERIALIZED (SELECT rookery.fence($2) AS ok)
             DELETE FROM rookery.terminals USING _f WHERE id = $1 AND _f.ok",
        )
        .bind(id)
        .bind(self.fence)
        .execute(self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Who runs terminals on a host, with how many and the newest start.
    pub async fn presence(&self, host: &str) -> Result<Vec<(Uuid, String, i64, DateTime<Utc>)>> {
        Ok(sqlx::query_as(
            "SELECT u.id, u.name, count(*), max(t.created_at)
             FROM rookery.terminals t
             JOIN rookery.hosts h ON h.id = t.host
             JOIN rookery.users u ON u.id = t.owner
             WHERE h.name = $1 AND t.state = 'running'
             GROUP BY u.id, u.name ORDER BY max(t.created_at) DESC",
        )
        .bind(host)
        .fetch_all(self.pool)
        .await?)
    }
}
