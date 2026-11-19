//! `leases.*`: fenced locks of a plugin. The fence grows each time a lease changes hands, so that what a
//! lapsed holder writes can be told apart.

use std::time::Duration;

use rookery_kernel::{Call, Empty, Level, Methods, RpcError};
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

fn ttl(ms: u64) -> Result<Duration, RpcError> {
    if ms == 0 {
        return Err(RpcError::invalid("ttlMs is at least 1"));
    }
    Ok(Duration::from_millis(ms))
}

pub fn register(methods: &mut Methods) {
    methods
        .add("leases.acquire", |call: Call, p: LeaseAcquire| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let lease = call.store().leases().acquire(&scoped(&plugin, &p.name)?, &call.audit_name(), ttl(p.ttl_ms)?).await?;
            Ok(info(&plugin, lease))
        })
        .summary("Takes a lease that is free or expired, with a new fence; CONFLICT while another holds it.")
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
                    &call.audit_name(),
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
                .release(&scoped(&plugin, &p.name)?, &call.audit_name(), fence)
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
