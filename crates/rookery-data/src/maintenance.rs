//! What the leader keeps tidy every hour: the partitions of log collections (the coming period's made,
//! those past their retention dropped), the change log (a week kept: a live query that is further behind
//! reads again), and idempotency keys past their day.

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

/// One round, for tests and for the loop.
pub async fn once(kernel: &Kernel) {
    let store = &kernel.store;
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
}

async fn run(kernel: Kernel) {
    loop {
        once(&kernel).await;
        tokio::select! {
            _ = kernel.shutdown.cancelled() => return,
            _ = tokio::time::sleep(EVERY) => {}
        }
    }
}
