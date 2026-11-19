//! `admin.*`: people and groups, for admins, and `audit.list`.

use rookery_kernel::{Call, Empty, Level, Methods, RpcError};
use rookery_proto::IdRef;
use rookery_proto::access::{
    AuditEntry, AuditList, AuditQuery, GroupCreate, GroupInfo, GroupList, Membership, SetDisabled,
    SetRole, UserInfo, UserList,
};
use rookery_store::users::User;

use crate::{parse_id, time};

fn user_info(u: User) -> UserInfo {
    UserInfo {
        id: u.id.to_string(),
        admin: u.is_admin(),
        disabled: u.is_disabled(),
        issuer: if u.issuer.starts_with("rookery-local:") {
            "local".into()
        } else {
            u.issuer.clone()
        },
        name: u.name,
        email: u.email,
        created_at: time(u.created_at),
        last_seen_at: u.last_seen_at.map(time),
    }
}

fn group_info(g: rookery_store::access::Group) -> GroupInfo {
    GroupInfo {
        id: g.id.to_string(),
        name: g.name,
        claim: g.claim,
        members: g.members,
    }
}

pub fn register(methods: &mut Methods) {
    methods
        .add("admin.users", |call: Call, _: Empty| async move {
            Ok(UserList {
                users: call
                    .store()
                    .users()
                    .list()
                    .await?
                    .into_iter()
                    .map(user_info)
                    .collect(),
            })
        })
        .summary("Everyone who has signed in.")
        .level(Level::Admin);
    methods
        .add("admin.setRole", |call: Call, p: SetRole| async move {
            let id = parse_id("user", &p.id)?;
            let user = call.store().users().set_role(id, p.admin).await?;
            // Their connections end, and come back with the role they have now.
            call.kernel.conns.refresh_user(&id.to_string());
            call.audit(
                "user.setRole",
                Some(&format!("user://{id}")),
                serde_json::json!({ "admin": p.admin }),
            )
            .await;
            Ok(user_info(user))
        })
        .summary("Makes a person an admin, or a member. The last admin stays one.")
        .level(Level::Admin);
    methods
        .add("admin.setDisabled", |call: Call, p: SetDisabled| async move {
            let id = parse_id("user", &p.id)?;
            let user = call.store().users().set_disabled(id, p.disabled).await?;
            if p.disabled {
                call.kernel.conns.revoke_user(&id.to_string());
            }
            call.audit("user.setDisabled", Some(&format!("user://{id}")), serde_json::json!({ "disabled": p.disabled })).await;
            Ok(user_info(user))
        })
        .summary("Turns an account off, which ends its sessions and tokens, or on again. The last admin stays on.")
        .level(Level::Admin);
    methods
        .add("admin.groups", |call: Call, _: Empty| async move {
            Ok(GroupList {
                groups: call
                    .store()
                    .access()
                    .groups()
                    .await?
                    .into_iter()
                    .map(group_info)
                    .collect(),
            })
        })
        .summary("The groups, with how many members each has.")
        .level(Level::Admin);
    methods
        .add("admin.createGroup", |call: Call, p: GroupCreate| async move {
            let name = p.name.trim();
            if name.is_empty() || name.chars().count() > 100 {
                return Err(RpcError::invalid("a group's name is 1 to 100 characters"));
            }
            let group = call.store().access().create_group(name, p.claim.as_deref()).await?;
            call.audit("group.create", Some(&format!("group://{}", group.id)), serde_json::json!({ "name": name, "claim": p.claim })).await;
            Ok(group_info(group))
        })
        .summary("Makes a group; with a claim, it has the members of that group of the identity provider.")
        .level(Level::Admin);
    methods
        .add("admin.deleteGroup", |call: Call, p: IdRef| async move {
            let id = parse_id("group", &p.id)?;
            call.store().access().delete_group(id).await?;
            call.audit(
                "group.delete",
                Some(&format!("group://{id}")),
                serde_json::json!({}),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Removes a group and its grants.")
        .level(Level::Admin);
    methods
        .add("admin.addMember", |call: Call, p: Membership| async move {
            let (group, user) = (parse_id("group", &p.group)?, parse_id("user", &p.user)?);
            call.store().access().add_member(group, user).await?;
            call.audit(
                "group.addMember",
                Some(&format!("group://{group}")),
                serde_json::json!({ "user": user }),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Puts a person in a group.")
        .level(Level::Admin);
    methods
        .add(
            "admin.removeMember",
            |call: Call, p: Membership| async move {
                let (group, user) = (parse_id("group", &p.group)?, parse_id("user", &p.user)?);
                call.store().access().remove_member(group, user).await?;
                call.audit(
                    "group.removeMember",
                    Some(&format!("group://{group}")),
                    serde_json::json!({ "user": user }),
                )
                .await;
                Ok(Empty {})
            },
        )
        .summary("Takes a person out of a group.")
        .level(Level::Admin);
    methods
        .add("audit.list", |call: Call, p: AuditQuery| async move {
            let filter = rookery_store::audit::Filter {
                resource: p.resource,
                principal: p.principal,
                action: p.action,
                before: p.before,
                limit: p.limit.unwrap_or(100),
            };
            let entries = call.store().audit().list(&filter).await?;
            Ok(AuditList {
                entries: entries
                    .into_iter()
                    .map(|e| AuditEntry {
                        id: e.id,
                        at: time(e.at),
                        principal: e.principal,
                        action: e.action,
                        resource: e.resource,
                        details: e.details,
                    })
                    .collect(),
            })
        })
        .summary("The audit log, newest first: who did what to which resource.")
        .level(Level::Admin);
}
