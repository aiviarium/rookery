//! What the leader keeps tidy every hour: the partitions of log collections (the coming period's made,
//! those past their retention dropped), the change log (a week kept: a live query that is further behind
//! reads again), idempotency keys past their day, and webhook deliveries and refusals past a week (an
//! accepted delivery's replay key is kept that long). A round runs only while the server still leads,
//! and the loop ends with the server.

use std::time::Duration;

use rookery_kernel::{Kernel, Methods};

const EVERY: Duration = Duration::from_secs(3600);
/// How long the change log keeps changes.
pub const CHANGES_KEPT: Duration = Duration::from_secs(7 * 24 * 3600);

pub fn register(methods: &mut Methods) {
    methods.on_started(|kernel| {
        let kernel = kernel.clone();
        tokio::spawn(async move { run(kernel).await });
    });
}

/// One round, for tests and for the loop. Nothing, and `false`, once another server leads.
pub async fn once(kernel: &Kernel) -> bool {
    let store = &kernel.store;
    match store.still_leads().await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!("another server leads: the hourly round stops");
            return false;
        }
        Err(e) => {
            tracing::warn!(error = %e, "checking the leader before the hourly round failed");
            return true;
        }
    }
    match store.collections().maintain_logs(chrono::Utc::now()).await {
        Ok(report) if !report.dropped.is_empty() => {
            tracing::info!(dropped = ?report.dropped, "partitions of log collections past their retention dropped")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "keeping the partitions of log collections failed"),
    }
    match store.feed().trim(CHANGES_KEPT).await {
        Ok(n) if n > 0 => tracing::debug!(changes = n, "the change log was trimmed"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "trimming the change log failed"),
    }
    if let Err(e) = rookery_store::idempotency::sweep(store).await {
        tracing::warn!(error = %e, "deleting idempotency keys past their day failed");
    }
    let week = chrono::Duration::seconds(crate::webhooks::REPLAY_KEPT_SECS);
    if let Err(e) = store
        .webhooks()
        .purge(chrono::Utc::now() - week, week)
        .await
    {
        tracing::warn!(error = %e, "deleting old webhook deliveries failed");
    }
    true
}

async fn run(kernel: Kernel) {
    loop {
        let leads = tokio::select! {
            _ = kernel.shutdown.cancelled() => return,
            leads = once(&kernel) => leads,
        };
        if !leads {
            return;
        }
        tokio::select! {
            _ = kernel.shutdown.cancelled() => return,
            _ = tokio::time::sleep(EVERY) => {}
        }
    }
}
