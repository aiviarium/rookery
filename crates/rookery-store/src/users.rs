//! People who signed in, by an identity provider or a local account.

use chrono::{DateTime, Utc};
use sqlx::AssertSqlSafe;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::util::new_id;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub issuer: String,
    pub subject: String,
    pub name: String,
    pub email: Option<String>,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
    pub fn is_disabled(&self) -> bool {
        self.disabled_at.is_some()
    }
}

/// What an identity provider, or a sign-in link, says about a person.
#[derive(Debug, Clone, Default)]
pub struct Login {
    pub issuer: String,
    pub subject: String,
    pub name: String,
    pub email: Option<String>,
    /// Makes the person an admin. A sign-in never takes the role away; `admin.setRole` does.
    pub admin: bool,
}

const COLS: &str = "id, issuer, subject, name, email, role, created_at, last_seen_at, disabled_at";

pub struct Users<'a> {
    pub(crate) pool: &'a PgPool,
}

impl Users<'_> {
    /// Makes the user on their first sign-in, or brings their name and email up to date. Returns the user
    /// and whether they are new.
    pub async fn upsert_login(&self, login: &Login) -> Result<(User, bool)> {
        #[derive(sqlx::FromRow)]
        struct Upserted {
            #[sqlx(flatten)]
            user: User,
            inserted: bool,
        }
        let role = if login.admin { "admin" } else { "member" };
        let mut tx = self.pool.begin().await?;
        // xmax is 0 for a row this statement inserted, and set for one it updated.
        let row: Upserted = sqlx::query_as(AssertSqlSafe(format!(
            "INSERT INTO rookery.users AS u (id, issuer, subject, name, email, role, last_seen_at)
             VALUES ($1, $2, $3, $4, $5, $6, now())
             ON CONFLICT (issuer, subject) DO UPDATE SET
                 name = EXCLUDED.name, email = EXCLUDED.email, last_seen_at = now(),
                 role = CASE WHEN EXCLUDED.role = 'admin' THEN 'admin' ELSE u.role END
             RETURNING {COLS}, (xmax = 0) AS inserted"
        )))
        .bind(new_id())
        .bind(&login.issuer)
        .bind(&login.subject)
        .bind(&login.name)
        .bind(&login.email)
        .bind(role)
        .fetch_one(&mut *tx)
        .await?;
        index_user(&mut tx, &row.user, row.inserted).await?;
        crate::spaces::ensure_personal(&mut tx, &row.user).await?;
        tx.commit().await?;
        Ok((row.user, row.inserted))
    }

    /// Gives every person who lacks them their entity `user://<id>`, its grant and their personal space:
    /// people from before the data layer, and those an import writes into `rookery.users` itself. Returns
    /// how many it brought up to date. An import runs it after its last user.
    pub async fn backfill(&self) -> Result<u64> {
        #[derive(sqlx::FromRow)]
        struct Missing {
            #[sqlx(flatten)]
            user: User,
            unindexed: bool,
        }
        let missing: Vec<Missing> = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT * FROM (SELECT {COLS}, NOT EXISTS (SELECT 1 FROM rookery.entities e
                                WHERE e.scheme = 'user' AND e.id = u.id::text) AS unindexed
                            FROM rookery.users u) m
             WHERE unindexed OR NOT EXISTS (SELECT 1 FROM rookery.spaces s WHERE s.owner_id = m.id AND s.personal)"
        )))
        .fetch_all(self.pool)
        .await?;
        for Missing { user, unindexed } in &missing {
            let mut tx = self.pool.begin().await?;
            // The grant comes with the entity, so a grant an admin took away stays away.
            index_user(&mut tx, user, *unindexed).await?;
            crate::spaces::ensure_personal(&mut tx, user).await?;
            tx.commit().await?;
        }
        Ok(missing.len() as u64)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<User>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.users WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(self.pool)
        .await?)
    }

    pub async fn by_email(&self, email: &str) -> Result<Option<User>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!("SELECT {COLS} FROM rookery.users WHERE lower(email) = lower($1) ORDER BY created_at LIMIT 1")))
            .bind(email)
            .fetch_optional(self.pool)
            .await?)
    }

    pub async fn list(&self) -> Result<Vec<User>> {
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.users ORDER BY lower(name), id"
        )))
        .fetch_all(self.pool)
        .await?)
    }

    /// The people who are not disabled whose name, a word of it, or email starts with `text`.
    pub async fn search(&self, text: &str, limit: i64) -> Result<Vec<User>> {
        let pattern = format!(
            "{}%",
            text.to_lowercase()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        Ok(sqlx::query_as(AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.users
             WHERE disabled_at IS NULL AND (lower(name) LIKE $1 OR lower(name) LIKE '% ' || $1 OR lower(email) LIKE $1)
             ORDER BY lower(name), id LIMIT $2"
        )))
        .bind(pattern)
        .bind(limit)
        .fetch_all(self.pool)
        .await?)
    }

    pub async fn any(&self) -> Result<bool> {
        Ok(
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rookery.users)")
                .fetch_one(self.pool)
                .await?,
        )
    }

    pub async fn has_enabled_admin(&self) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM rookery.users WHERE role = 'admin' AND disabled_at IS NULL)",
        )
        .fetch_one(self.pool)
        .await?)
    }

    /// Sets the org role. The last admin who is not disabled cannot stop being one.
    pub async fn set_role(&self, id: Uuid, admin: bool) -> Result<User> {
        let mut tx = crate::actor::begin(self.pool).await?;
        // One change of roles at a time, so two admins cannot each remove the other.
        sqlx::query("LOCK TABLE rookery.users IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await?;
        if !admin {
            let others: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM rookery.users WHERE role = 'admin' AND disabled_at IS NULL AND id <> $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            if others == 0 {
                return Err(Error::Conflict(
                    "the server needs one admin; make another first".into(),
                ));
            }
        }
        let user = sqlx::query_as::<_, User>(AssertSqlSafe(format!(
            "UPDATE rookery.users SET role = $2 WHERE id = $1 RETURNING {COLS}"
        )))
        .bind(id)
        .bind(if admin { "admin" } else { "member" })
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::NotFound("no such user".into()))?;
        tx.commit().await?;
        Ok(user)
    }

    /// Turns an account off or on. A disabled account's sessions and tokens stop working; the last admin
    /// cannot be disabled.
    pub async fn set_disabled(&self, id: Uuid, disabled: bool) -> Result<User> {
        let mut tx = crate::actor::begin(self.pool).await?;
        sqlx::query("LOCK TABLE rookery.users IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await?;
        if disabled {
            let others: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM rookery.users WHERE role = 'admin' AND disabled_at IS NULL AND id <> $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            let is_admin: Option<String> =
                sqlx::query_scalar("SELECT role FROM rookery.users WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if is_admin.as_deref() == Some("admin") && others == 0 {
                return Err(Error::Conflict(
                    "the server needs one admin; make another first".into(),
                ));
            }
        }
        let user = sqlx::query_as::<_, User>(AssertSqlSafe(format!(
            "UPDATE rookery.users SET disabled_at = CASE WHEN $2 THEN coalesce(disabled_at, now()) END
             WHERE id = $1 RETURNING {COLS}"
        )))
        .bind(id)
        .bind(disabled)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::NotFound("no such user".into()))?;
        tx.commit().await?;
        Ok(user)
    }
}

/// A person's entity, `user://<id>`, which mentions and references name. Everyone may see it, and it
/// is private: what hangs below it, such as a person's own rows, does not inherit that. It holds the name
/// and nothing else: emails are for admins, through `users.*`.
async fn index_user(conn: &mut sqlx::PgConnection, user: &User, grant: bool) -> Result<()> {
    let uri = crate::uri::user_uri(user.id);
    crate::entities::upsert(
        &mut *conn,
        &crate::entities::EntityRecord {
            uri: uri.clone(),
            title: user.name.clone(),
            owner_id: Some(user.id),
            private: true,
            ..Default::default()
        },
        None,
    )
    .await?;
    if grant {
        sqlx::query(
            "INSERT INTO rookery.grants (resource, principal, level, created_by) VALUES ($1, 'everyone', 1, 'system:rookery') ON CONFLICT DO NOTHING",
        )
        .bind(&uri)
        .execute(conn)
        .await?;
    }
    Ok(())
}
