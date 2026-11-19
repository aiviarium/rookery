//! Who a write is by, for the history of Rookery's typed tables. Their trigger reads `rookery.actor`,
//! which a transaction sets for itself; the kernel runs each call in [`scope`] with the caller, and the
//! code that writes those tables opens its transaction with [`begin`].

use std::future::Future;

use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::error::Result;

tokio::task_local! {
    static ACTOR: String;
}

/// Runs `f` with writes to typed tables recorded as by `actor`, such as `user:<id>`.
pub async fn scope<F: Future>(actor: String, f: F) -> F::Output {
    ACTOR.scope(actor, f).await
}

/// The actor of the call this task runs, if any.
pub fn current() -> Option<String> {
    ACTOR.try_with(Clone::clone).ok()
}

/// Begins a transaction whose writes are by the current actor.
pub(crate) async fn begin(pool: &PgPool) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    mark(&mut tx, current().as_deref()).await?;
    Ok(tx)
}

/// Begins a transaction whose writes are by `actor`.
pub(crate) async fn begin_as(pool: &PgPool, actor: &str) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    mark(&mut tx, Some(actor)).await?;
    Ok(tx)
}

/// Sets the actor for the rest of the transaction `conn` is in.
pub(crate) async fn mark(conn: &mut PgConnection, actor: Option<&str>) -> Result<()> {
    if let Some(actor) = actor {
        sqlx::query("SELECT set_config('rookery.actor', $1, true)")
            .bind(actor)
            .execute(conn)
            .await?;
    }
    Ok(())
}
