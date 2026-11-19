//! `schedules.*`: cron expressions in a time zone, or intervals, that enqueue jobs.
//!
//! A plugin declares schedules (`contributes.schedules`), which the plugin system syncs with
//! [`sync_declared`], and makes others with `schedules.create`; they enqueue its own kinds of job,
//! `<plugin>/<job>`, as the plugin. An admin makes schedules of the server's own kinds too, such as
//! `apply` with the subject `env://studio`, enqueued as the admin. A firing while the schedule's last job
//! is still open starts no second one, and runs missed while no server ran follow the schedule's `missed`
//! policy. `schedules.fire` runs one now, whether or not it is due or enabled.

use std::time::Duration;

use chrono::Utc;
use rookery_kernel::{Call, Caller, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::schedules::{
    ScheduleContribution, ScheduleCreate, ScheduleFire, ScheduleInfo, ScheduleList,
    ScheduleListQuery, ScheduleRef, ScheduleSetEnabled,
};
use rookery_store::collections::system::PLUGIN as ROOKERY;
use rookery_store::schedules::{NewSchedule, Schedule, ScheduleQuery, next_run_in, parse_tz};

use crate::{parse_id, rpc, time};

const TICK: Duration = Duration::from_secs(1);
/// The shortest time between two runs of a schedule.
pub const MIN_INTERVAL_SECS: i64 = 60;

pub fn register(methods: &mut Methods) {
    methods.on_started(|kernel| {
        let kernel = kernel.clone();
        tokio::spawn(async move { tick(kernel).await });
    });
    methods
        .add("schedules.create", create)
        .summary("Makes or replaces a schedule: a plugin's of its own jobs, or an admin's of any kind a handler runs.")
        .level(Level::Own);
    methods
        .add("schedules.list", list)
        .summary("Schedules: a plugin's own, those made for the caller, or with all every one, for an admin.")
        .level(Level::Own);
    methods
        .add("schedules.setEnabled", |call: Call, p: ScheduleSetEnabled| async move {
            let s = mine(&call, &p.id).await?;
            let s = call
                .store()
                .schedules()
                .set_enabled(s.id, p.enabled)
                .await
                .map_err(rpc)?;
            Ok(info(&s))
        })
        .summary("Pauses or resumes a schedule. A resumed one runs next from now, so the runs of the pause are not missed.")
        .level(Level::Own);
    methods
        .add("schedules.fire", fire)
        .summary("Runs a schedule's job now, whether or not it is due or enabled, with more params merged in.")
        .level(Level::Own);
    methods
        .add(
            "schedules.remove",
            |call: Call, p: ScheduleRef| async move {
                let s = mine(&call, &p.id).await?;
                if s.declared {
                    return Err(RpcError::invalid(
                        "this schedule is declared in its plugin's manifest: pause it instead",
                    ));
                }
                call.store().schedules().delete(s.id).await.map_err(rpc)?;
                Ok(Empty {})
            },
        )
        .summary("Removes a schedule that was made, not declared.")
        .level(Level::Own);
}

pub fn info(s: &Schedule) -> ScheduleInfo {
    ScheduleInfo {
        id: s.id.to_string(),
        plugin: s.plugin.clone(),
        name: s.name.clone(),
        subject: s.subject.clone(),
        title: s.title.clone(),
        job: s.kind.clone(),
        params: s.params.clone(),
        cron: s.cron.clone(),
        every_secs: s.interval_secs,
        timezone: s.timezone.clone(),
        missed: s.missed.clone(),
        enabled: s.enabled,
        paused_reason: s.paused_reason.clone(),
        declared: s.declared,
        user: s.on_behalf_of.map(|u| u.to_string()),
        next_run_at: s.next_run_at.map(time),
        last_run_at: s.last_run_at.map(time),
        last_job_id: s.last_job_id.map(|t| t.to_string()),
    }
}

fn check_timing(
    what: &str,
    cron: Option<&str>,
    every: Option<i64>,
    tz: Option<&str>,
) -> Result<(), RpcError> {
    if cron.is_some() == every.is_some() {
        return Err(RpcError::invalid(format!(
            "{what} has a cron expression or everySecs, one of the two"
        )));
    }
    if every.is_some_and(|e| e < MIN_INTERVAL_SECS) {
        return Err(RpcError::invalid(format!(
            "{what}: everySecs is at least {MIN_INTERVAL_SECS}"
        )));
    }
    let tz = tz.unwrap_or("UTC");
    parse_tz(tz).map_err(|e| RpcError::invalid(format!("{what}: {e}")))?;
    // The next runs of a cron expression, a minute apart at least.
    let mut at = next_run_in(cron, every, tz, Utc::now())
        .map_err(|e| RpcError::invalid(format!("{what}: {e}")))?;
    for _ in 0..8 {
        let Ok(next) = next_run_in(cron, every, tz, at) else {
            break;
        };
        if (next - at).num_seconds() < MIN_INTERVAL_SECS {
            return Err(RpcError::invalid(format!(
                "{what} runs more often than once every {MIN_INTERVAL_SECS} s"
            )));
        }
        at = next;
    }
    Ok(())
}

/// A plugin's own kind of job, `<plugin>/<job>`, from a name with or without the prefix.
fn plugin_kind(plugin: &str, job: &str) -> Result<String, RpcError> {
    let name = job.strip_prefix(&format!("{plugin}/")).unwrap_or(job);
    if name.is_empty() || name.len() > 200 || name.contains('/') {
        return Err(RpcError::invalid(format!(
            "{job:?} is no kind of job of the plugin {plugin}"
        )));
    }
    Ok(format!("{plugin}/{name}"))
}

/// Makes a plugin's declared schedules what its manifest says: called when it loads.
pub async fn sync_declared(
    kernel: &Kernel,
    plugin: &str,
    declared: &[ScheduleContribution],
) -> Result<(), RpcError> {
    let mut defs = Vec::new();
    for s in declared {
        let name =
            s.id.strip_prefix(&format!("{plugin}."))
                .or_else(|| s.id.strip_prefix(&format!("{plugin}/")))
                .unwrap_or(&s.id);
        let what = format!("the schedule {}", s.id);
        let every = s.every_secs.map(i64::from);
        check_timing(&what, s.cron.as_deref(), every, s.timezone.as_deref())?;
        defs.push(NewSchedule {
            title: s.title.clone(),
            params: s.params.clone().unwrap_or_else(|| serde_json::json!({})),
            cron: s.cron.clone(),
            interval_secs: every,
            timezone: s.timezone.clone(),
            missed: s.missed.clone(),
            enabled: s.enabled.unwrap_or(true),
            ..NewSchedule::new(
                plugin,
                name,
                plugin_kind(plugin, &s.job)?,
                format!("plugin:{plugin}"),
            )
        });
    }
    kernel
        .store
        .schedules()
        .sync_declared(plugin, &defs)
        .await
        .map_err(rpc)?;
    Ok(())
}

async fn create(call: Call, p: ScheduleCreate) -> Result<ScheduleInfo, RpcError> {
    let (plugin, kind) = match (&call.caller, p.plugin.as_deref()) {
        (Caller::Plugin(own), None) => (own.clone(), plugin_kind(own, &p.job)?),
        (Caller::Plugin(own), Some(named)) if named == own => {
            (own.clone(), plugin_kind(own, &p.job)?)
        }
        (Caller::Plugin(_), Some(_)) => {
            return Err(RpcError::forbidden("a plugin makes schedules of its own"));
        }
        _ if !call.is_admin() => {
            return Err(RpcError::forbidden(
                "schedules are made by plugins, and by admins",
            ));
        }
        (_, None) | (_, Some(ROOKERY)) => {
            if !rookery_jobs::handles(&call.kernel, &p.job) {
                return Err(RpcError::invalid(format!(
                    "no handler in the server runs jobs of the kind {}",
                    p.job
                )));
            }
            (ROOKERY.to_string(), p.job.clone())
        }
        (_, Some(named)) => {
            let plugin = rookery_kernel::methods::plugin_id(named)?;
            let kind = plugin_kind(&plugin, &p.job)?;
            (plugin, kind)
        }
    };
    if p.name.trim().is_empty() || p.name.len() > 200 {
        return Err(RpcError::invalid(
            "a schedule's name is 1 to 200 characters",
        ));
    }
    if let Some(subject) = &p.subject {
        rookery_store::access::check_resource(subject).map_err(rpc)?;
    }
    check_timing(
        "the schedule",
        p.cron.as_deref(),
        p.every_secs,
        p.timezone.as_deref(),
    )?;
    let params = if p.params.is_null() {
        serde_json::json!({})
    } else if p.params.is_object() {
        p.params
    } else {
        return Err(RpcError::invalid("params are an object"));
    };
    let s = NewSchedule {
        title: p.title,
        params,
        subject: p.subject,
        cron: p.cron,
        interval_secs: p.every_secs,
        timezone: p.timezone,
        missed: p.missed,
        enabled: p.enabled.unwrap_or(true),
        ..NewSchedule::new(&plugin, p.name.trim(), kind, call.audit_name())
    };
    let s = call.store().schedules().upsert(&s).await.map_err(rpc)?;
    Ok(info(&s))
}

async fn list(call: Call, p: ScheduleListQuery) -> Result<ScheduleList, RpcError> {
    let mut q = ScheduleQuery {
        plugin: p.plugin.clone(),
        ..Default::default()
    };
    match &call.caller {
        Caller::Plugin(plugin) => q.plugin = Some(plugin.clone()),
        _ if p.all && call.is_admin() => {}
        Caller::User { .. } => q.user = Some(call.user_id()?),
        _ => {}
    }
    let all = call.store().schedules().list(&q).await.map_err(rpc)?;
    Ok(ScheduleList {
        schedules: all.iter().map(info).collect(),
    })
}

/// A schedule the caller may change: a plugin its own, a person those made for them, an admin any.
async fn mine(call: &Call, id: &str) -> Result<Schedule, RpcError> {
    let s = call
        .store()
        .schedules()
        .get(parse_id("schedule", id)?)
        .await
        .map_err(rpc)?
        .ok_or_else(|| RpcError::not_found("no such schedule"))?;
    let ok = match &call.caller {
        Caller::Plugin(plugin) => &s.plugin == plugin,
        _ if call.is_admin() => true,
        Caller::User { user, .. } => {
            s.on_behalf_of.map(|b| b.to_string()).as_ref() == Some(&user.id)
        }
        _ => false,
    };
    if ok {
        Ok(s)
    } else {
        Err(RpcError::not_found("no such schedule"))
    }
}

async fn fire(call: Call, p: ScheduleFire) -> Result<serde_json::Value, RpcError> {
    let s = mine(&call, &p.id).await?;
    if p.params.as_ref().is_some_and(|v| !v.is_object()) {
        return Err(RpcError::invalid("params are an object"));
    }
    let job = call
        .store()
        .schedules()
        .fire(s.id, p.params.as_ref())
        .await
        .map_err(rpc)?;
    rookery_jobs::wake(&call.kernel);
    Ok(serde_json::json!({ "job": job.id.to_string() }))
}

/// Fires what is due, every second, on the leader.
async fn tick(kernel: Kernel) {
    loop {
        tokio::select! {
            _ = kernel.shutdown.cancelled() => return,
            _ = tokio::time::sleep(TICK) => {}
        }
        match kernel.store.schedules().pause_lost(Utc::now()).await {
            Ok(paused) => {
                for s in &paused {
                    told_paused(&kernel, s).await;
                }
            }
            Err(e) => tracing::warn!(error = %e, "checking who schedules run as failed"),
        }
        match kernel.store.schedules().enqueue_due(Utc::now()).await {
            Ok(jobs) if !jobs.is_empty() => {
                tracing::debug!(jobs = jobs.len(), "schedules fired");
                rookery_jobs::wake(&kernel);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "firing schedules failed"),
        }
    }
}

/// Tells the person a schedule ran as, or was made for, that the server paused it.
async fn told_paused(kernel: &Kernel, s: &Schedule) {
    tracing::info!(schedule = %s.id, reason = ?s.paused_reason, "a schedule was paused");
    let owner = s
        .principal
        .strip_prefix("user:")
        .and_then(|u| uuid::Uuid::parse_str(u).ok())
        .or(s.on_behalf_of);
    let Some(user) = owner else { return };
    let title = format!(
        "The schedule {} was paused",
        s.title.as_deref().unwrap_or(&s.name)
    );
    let data = serde_json::json!({ "schedule": s.id.to_string(), "plugin": s.plugin });
    if let Err(e) = kernel
        .store
        .notify()
        .send(rookery_store::notify::New {
            user,
            kind: "rookery/schedulePaused",
            title: &title,
            body: s.paused_reason.as_deref(),
            data: Some(&data),
            actions: None,
            sent_by: "system:rookery",
        })
        .await
    {
        tracing::warn!(error = %e, "telling of a paused schedule failed");
    }
}
