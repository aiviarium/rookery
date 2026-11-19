//! `access.*`: grants on any resource URI, such as `env://studio`, with the levels view ⊂ use ⊂ manage.
//! The owner of a resource holds manage, and the org role admin holds manage on all. It also gives
//! the kernel the check behind `Level::Resource`.

use rookery_kernel::{Call, Caller, Empty, Grant, Level, Methods, RpcError};
use rookery_proto::access::{
    AccessFilter, AccessGrant, AccessRequire, AccessRevoke, GrantInfo, GrantList, LevelHeld,
    LevelOf, ResourceRef, Resources,
};
use rookery_store::access::Who;
use uuid::Uuid;

use crate::time;

/// The level the caller holds on a resource.
pub async fn level(call: &Call, resource: &str) -> Result<Option<Grant>, RpcError> {
    if call.is_admin() {
        return Ok(Some(Grant::Manage));
    }
    let principal = call.principal().uri();
    let user = match &call.caller {
        Caller::User { user, .. } => Uuid::parse_str(&user.id).ok(),
        _ => None,
    };
    let held = call
        .store()
        .access()
        .level(
            resource,
            &Who {
                principal: &principal,
                user,
            },
        )
        .await?;
    Ok(Grant::from_number(held))
}

pub async fn holds(call: &Call, resource: &str, wanted: Grant) -> Result<bool, RpcError> {
    Ok(level(call, resource)
        .await?
        .is_some_and(|held| held >= wanted))
}

pub fn register(methods: &mut Methods) {
    methods
        .set_access(|call, resource, wanted| async move { holds(&call, &resource, wanted).await });
    let manage = || Level::Resource {
        param: "resource".into(),
        grant: Grant::Manage,
    };
    methods
        .add("access.grant", |call: Call, p: AccessGrant| async move {
            let grant = call
                .store()
                .access()
                .grant(
                    &p.resource,
                    &p.principal,
                    p.level as i16,
                    &call.audit_name(),
                )
                .await?;
            let details = serde_json::json!({ "principal": p.principal, "level": p.level });
            call.audit("access.grant", Some(&p.resource), details).await;
            Ok(GrantInfo {
                principal: grant.principal,
                level: p.level,
                created_by: grant.created_by,
                created_at: time(grant.created_at),
            })
        })
        .summary(
            "Grants a principal (user:ID, group:ID or everyone) view, use or manage on a resource.",
        )
        .level(manage());
    methods
        .add("access.revoke", |call: Call, p: AccessRevoke| async move {
            if !call
                .store()
                .access()
                .revoke(&p.resource, &p.principal)
                .await?
            {
                return Err(RpcError::not_found(format!(
                    "{} has no grant on {}",
                    p.principal, p.resource
                )));
            }
            call.audit(
                "access.revoke",
                Some(&p.resource),
                serde_json::json!({ "principal": p.principal }),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Takes a principal's grant on a resource away.")
        .level(manage());
    methods
        .add("access.grants", |call: Call, p: ResourceRef| async move {
            let access = call.store().access();
            let grants = access.grants(&p.resource).await?;
            Ok(GrantList {
                owner: access.owner(&p.resource).await?.map(|u| u.to_string()),
                resource: p.resource,
                grants: grants
                    .into_iter()
                    .filter_map(|g| {
                        Some(GrantInfo {
                            principal: g.principal,
                            level: Grant::from_number(g.level)?,
                            created_by: g.created_by,
                            created_at: time(g.created_at),
                        })
                    })
                    .collect(),
            })
        })
        .summary("Who holds what on a resource.")
        .level(manage());
    methods
        .add("access.level", |call: Call, p: ResourceRef| async move {
            Ok(LevelHeld {
                level: level(&call, &p.resource).await?,
            })
        })
        .summary("The level the caller holds on a resource.")
        .level(Level::Own);
    methods
        .add("access.levelOf", |call: Call, p: LevelOf| async move {
            let principal = rookery_store::access::principal_name(&p.principal)?;
            let user = principal
                .strip_prefix("user:")
                .and_then(|id| Uuid::parse_str(id).ok());
            if let Some(user) = user
                && call
                    .store()
                    .users()
                    .get(user)
                    .await?
                    .is_some_and(|u| u.is_admin())
            {
                return Ok(LevelHeld {
                    level: Some(Grant::Manage),
                });
            }
            let held = call
                .store()
                .access()
                .level(
                    &p.resource,
                    &Who {
                        principal: &principal,
                        user,
                    },
                )
                .await?;
            Ok(LevelHeld {
                level: Grant::from_number(held),
            })
        })
        .summary("The level another principal holds on a resource.")
        .level(Level::Admin);
    methods
        .add(
            "access.require",
            |call: Call, p: AccessRequire| async move {
                if holds(&call, &p.resource, p.level).await? {
                    Ok(Empty {})
                } else {
                    Err(RpcError::forbidden(format!(
                        "this needs {} on {}",
                        rookery_kernel::methods::grant_name(p.level),
                        p.resource
                    )))
                }
            },
        )
        .summary("FORBIDDEN unless the caller holds a level on a resource.")
        .level(Level::Own);
    methods
        .add("access.filter", |call: Call, p: AccessFilter| async move {
            if p.resources.len() > 1000 {
                return Err(RpcError::invalid("at most 1000 resources at once"));
            }
            if call.is_admin() {
                return Ok(Resources {
                    resources: p.resources,
                });
            }
            let principal = call.principal().uri();
            let user = call.user().and_then(|u| Uuid::parse_str(&u.id).ok());
            let resources = call
                .store()
                .access()
                .filter(
                    &p.resources,
                    &Who {
                        principal: &principal,
                        user,
                    },
                    p.level as i16,
                )
                .await?;
            Ok(Resources { resources })
        })
        .summary("Those of some resources the caller holds a level on.")
        .level(Level::Own);
}
