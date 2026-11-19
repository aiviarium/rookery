//! `leases.*`: fenced locks of a plugin. The fence grows each time a lease changes hands, so that what a
//! lapsed holder writes can be told apart.
//!
//! A lease is held by a worker: the caller and the worker it names (`plugin:fleet/replica-2`), or the
//! caller and its connection on `/ws`. Two workers of one plugin are two holders, so only one of them
//! has the lease at a time. A connection's leases are released when it closes.

use std::time::Duration;

use rookery_kernel::conn::ConnHandle;
use rookery_kernel::{Call, Caller, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::jobs::{LeaseAcquire, LeaseInfo, LeaseRef, LeaseState};
use rookery_store::leases::Lease;

use crate::{scoped, time};

fn info(plugin: &str, lease: Lease) -> LeaseInfo {
    LeaseInfo {
        name: lease
            .name
            .strip_prefix(&format!("{plugin}/"))
            .unwrap_or(&lease.name)
            .to_owned(),
        holder: lease.holder,
        fence: lease.fence,
        expires_at: lease.expires_at.map(time),
    }
}

/// This kernel's own name, so that the connections of two servers, or of two terms of one, are never
/// the same holder.
struct Instance(uuid::Uuid);

impl Default for Instance {
    fn default() -> Self {
        Instance(uuid::Uuid::now_v7())
    }
}

/// The connections that took a lease, whose leases are released when they close.
#[derive(Default)]
struct Holding(std::sync::Mutex<std::collections::HashSet<u64>>);

fn conn_holder(kernel: &Kernel, caller: &Caller, conn: &ConnHandle) -> String {
    format!(
        "{}/conn:{}:{}",
        caller.principal().uri(),
        kernel.facility::<Instance>().0.simple(),
        conn.id()
    )
}

/// The worker a call is from: the one it names, or its connection.
fn holder(call: &Call, worker: Option<&str>) -> Result<String, RpcError> {
    match worker {
        Some(w)
            if w.is_empty()
                || w.len() > 100
                || w.chars().any(char::is_control)
                || w.starts_with("conn:") =>
        {
            Err(RpcError::invalid(
                "a worker is 1 to 100 characters, and does not start with conn:",
            ))
        }
        Some(w) => Ok(format!("{}/{w}", call.audit_name())),
        None => match call.conn() {
            Ok(conn) => {
                call.kernel
                    .facility::<Holding>()
                    .0
                    .lock()
                    .unwrap()
                    .insert(conn.id());
                Ok(conn_holder(&call.kernel, &call.caller, conn))
            }
            Err(_) => Err(RpcError::invalid(
                "a lease is held by a worker: call on /ws, or name the worker",
            )),
        },
    }
}

fn ttl(ms: u64) -> Result<Duration, RpcError> {
    if ms == 0 {
        return Err(RpcError::invalid("ttlMs is at least 1"));
    }
    Ok(Duration::from_millis(ms))
}

pub fn register(methods: &mut Methods) {
    methods.on_connection_closed(|kernel, conn| {
        if !kernel.facility::<Holding>().0.lock().unwrap().remove(&conn.id()) {
            return;
        }
        let holder = conn_holder(kernel, conn.caller(), conn);
        let kernel = kernel.clone();
        tokio::spawn(async move {
            if let Err(e) = kernel.store.leases().release_held_by(&holder).await {
                tracing::warn!(error = %e, "releasing the leases of a closed connection failed; they expire");
            }
        });
    });
    methods
        .add("leases.acquire", |call: Call, p: LeaseAcquire| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let holder = holder(&call, p.worker.as_deref())?;
            let lease = call.store().leases().acquire(&scoped(&plugin, &p.name)?, &holder, ttl(p.ttl_ms)?).await?;
            Ok(info(&plugin, lease))
        })
        .summary("Takes a lease that is free or expired, with a new fence; CONFLICT while another worker holds it.")
        .level(Level::Plugin);
    methods
        .add("leases.renew", |call: Call, p: LeaseAcquire| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let fence = p
                .fence
                .ok_or_else(|| RpcError::invalid("renewing names the fence"))?;
            let lease = call
                .store()
                .leases()
                .renew(
                    &scoped(&plugin, &p.name)?,
                    &holder(&call, p.worker.as_deref())?,
                    fence,
                    ttl(p.ttl_ms)?,
                )
                .await?;
            Ok(info(&plugin, lease))
        })
        .summary("Extends a lease the caller holds at its fence; CONFLICT once it lapsed.")
        .level(Level::Plugin);
    methods
        .add("leases.release", |call: Call, p: LeaseRef| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let fence = p
                .fence
                .ok_or_else(|| RpcError::invalid("releasing names the fence"))?;
            if !call
                .store()
                .leases()
                .release(
                    &scoped(&plugin, &p.name)?,
                    &holder(&call, p.worker.as_deref())?,
                    fence,
                )
                .await?
            {
                return Err(RpcError::conflict(
                    "the caller does not hold the lease at that fence",
                ));
            }
            Ok(Empty {})
        })
        .summary("Gives up a lease the caller holds.")
        .level(Level::Plugin);
    methods
        .add("leases.inspect", |call: Call, p: LeaseRef| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let lease = call
                .store()
                .leases()
                .inspect(&scoped(&plugin, &p.name)?)
                .await?;
            Ok(LeaseState {
                lease: lease.map(|l| info(&plugin, l)),
            })
        })
        .summary("Who holds a lease, at which fence, until when.")
        .level(Level::Plugin);
}
