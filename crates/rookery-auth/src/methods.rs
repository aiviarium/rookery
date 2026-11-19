//! `auth.*`.

use chrono::{DateTime, Utc};
use rookery_kernel::{Call, Caller, Empty, Level, Methods, RpcError, VERSION};
use rookery_proto::IdRef;
use rookery_proto::auth::{
    AuthMode, DeviceInfo, DeviceList, LinkCreate, LinkCreated, LinkInfo, LinkKind, LinkList, Me,
    TokenCreate, TokenCreated, TokenInfo, TokenList,
};
use rookery_store::auth::{AccessToken, DeviceSession};
use rookery_store::links::{Pending, kind_name};
use uuid::Uuid;

use crate::links::{clean_name, link_url};
use crate::session::user_uri;

fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn parse_id(what: &str, id: &str) -> Result<Uuid, RpcError> {
    Uuid::parse_str(id).map_err(|_| RpcError::invalid(format!("{id:?} is not the id of a {what}")))
}

fn device_info(s: &DeviceSession, current: Option<Uuid>) -> DeviceInfo {
    DeviceInfo {
        id: s.id.to_string(),
        label: s.label.clone(),
        user_agent: s.user_agent.clone(),
        created_at: time(s.created_at),
        last_used_at: s.last_used_at.map(time),
        expires_at: s.expires_at.map(time),
        current: Some(s.id) == current,
    }
}

fn token_info(t: &AccessToken) -> TokenInfo {
    TokenInfo {
        id: t.id.to_string(),
        name: t.name.clone(),
        hint: t.hint.clone(),
        created_at: time(t.created_at),
        last_used_at: t.last_used_at.map(time),
        expires_at: t.expires_at.map(time),
    }
}

fn link_info(p: Pending) -> LinkInfo {
    LinkInfo {
        id: p.link.id.to_string(),
        kind: p.link.kind(),
        name: p.link.name,
        user_name: p.user_name,
        created_by_name: p.created_by_name,
        created_at: time(p.link.created_at),
        expires_at: time(p.link.expires_at),
    }
}

fn require_local_accounts(call: &Call) -> Result<(), RpcError> {
    match call.kernel.config.auth {
        AuthMode::Local => Ok(()),
        AuthMode::Oidc => Err(RpcError::unavailable(
            "this server signs in through an identity provider, so sign-in links are off",
        )),
    }
}

/// Makes a sign-in link for whoever calls: `rookery server setup-key` over the socket, or a person.
pub async fn create_link(call: &Call, p: LinkCreate) -> Result<LinkCreated, RpcError> {
    require_local_accounts(call)?;
    let me = match &call.caller {
        Caller::User { .. } => Some(call.user_id()?),
        _ => None,
    };
    let (target, name, email) = match p.kind {
        LinkKind::Setup => {
            if !matches!(call.caller, Caller::Local) {
                return Err(RpcError::forbidden(
                    "a setup link is made on the server's own machine: rookery server setup-key",
                ));
            }
            (None, None, None)
        }
        LinkKind::Invite => {
            call.require_admin()?;
            let name = p.name.as_deref().and_then(clean_name).ok_or_else(|| {
                RpcError::invalid("an invite needs the person's name, 1 to 100 characters")
            })?;
            let email = p.email.as_deref().map(str::trim).filter(|e| !e.is_empty());
            if email.is_some_and(|e| !e.contains('@') || e.len() > 200) {
                return Err(RpcError::invalid("that is not an email address"));
            }
            (None, Some(name), email.map(str::to_owned))
        }
        LinkKind::Device => {
            let target = match (p.user_id.as_deref(), me) {
                (Some(id), Some(me)) if parse_id("user", id)? == me => me,
                (Some(id), _) => {
                    call.require_admin()?;
                    parse_id("user", id)?
                }
                (None, Some(me)) => me,
                (None, None) => return Err(RpcError::invalid("name the user in userId")),
            };
            let user = call
                .store()
                .users()
                .get(target)
                .await?
                .ok_or_else(|| RpcError::not_found("no such user"))?;
            if user.is_disabled() {
                return Err(RpcError::invalid("that account is disabled"));
            }
            (Some(target), None, None)
        }
    };
    let (code, link) = call
        .store()
        .links()
        .create(p.kind, target, name.as_deref(), email.as_deref(), me)
        .await?;
    let resource = target.map(user_uri);
    let details = serde_json::json!({ "kind": kind_name(p.kind), "link": link.id, "name": name });
    call.audit("link.create", resource.as_deref(), details)
        .await;
    let pending = call
        .store()
        .links()
        .pending(None)
        .await?
        .into_iter()
        .find(|l| l.link.id == link.id)
        .ok_or_else(|| RpcError::internal("the link was not kept"))?;
    Ok(LinkCreated {
        url: link_url(&call.kernel, &code),
        info: link_info(pending),
    })
}

pub fn register(methods: &mut Methods) {
    methods
        .add("auth.me", |call: Call, _: Empty| async move {
            Ok(Me {
                user: call.user().cloned(),
                principal: call.principal(),
                admin: call.is_admin(),
                auth: call.kernel.config.auth,
                version: VERSION.into(),
                api_version: rookery_proto::API_VERSION,
            })
        })
        .summary("Who the caller is, and how this server signs people in.")
        .level(Level::Own);
    methods
        .add("auth.listDevices", |call: Call, _: Empty| async move {
            let user = call.user_id()?;
            let current = match &call.caller {
                Caller::User { session, .. } => *session,
                _ => None,
            };
            let sessions = call.store().auth().list_sessions(user).await?;
            Ok(DeviceList {
                devices: sessions.iter().map(|s| device_info(s, current)).collect(),
            })
        })
        .summary("The caller's device sessions.")
        .level(Level::Own);
    methods
        .add("auth.revokeDevice", |call: Call, p: IdRef| async move {
            let user = call.user_id()?;
            let id = parse_id("device", &p.id)?;
            call.store().auth().revoke_session(user, id).await?;
            // Its connections end now, instead of when they next close.
            call.kernel.conns.revoke_session(id);
            call.audit(
                "device.revoke",
                Some(&user_uri(user)),
                serde_json::json!({ "device": id }),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Ends one of the caller's device sessions, and its connections.")
        .level(Level::Own);
    methods
        .add(
            "auth.createToken",
            |call: Call, p: TokenCreate| async move {
                let user = call.user_id()?;
                let name = p.name.trim();
                if name.is_empty() || name.chars().count() > 100 {
                    return Err(RpcError::invalid(
                        "a token needs a name of 1 to 100 characters",
                    ));
                }
                if p.expires_in_days.is_some_and(|d| !(1..=3650).contains(&d)) {
                    return Err(RpcError::invalid("expiresInDays is 1 to 3650"));
                }
                let expires = p
                    .expires_in_days
                    .map(|d| Utc::now() + chrono::Duration::days(d));
                let (token, record) = call
                    .store()
                    .auth()
                    .create_token(user, name, expires)
                    .await?;
                call.audit(
                    "token.create",
                    Some(&user_uri(user)),
                    serde_json::json!({ "token": record.id, "name": name }),
                )
                .await;
                Ok(TokenCreated {
                    token,
                    info: token_info(&record),
                })
            },
        )
        .summary("Makes an access token for scripts: rookery_pat_…, shown once.")
        .level(Level::Own);
    methods
        .add("auth.listTokens", |call: Call, _: Empty| async move {
            let user = call.user_id()?;
            let tokens = call.store().auth().list_tokens(user).await?;
            Ok(TokenList {
                tokens: tokens.iter().map(token_info).collect(),
            })
        })
        .summary("The caller's access tokens, without the tokens.")
        .level(Level::Own);
    methods
        .add("auth.revokeToken", |call: Call, p: IdRef| async move {
            let user = call.user_id()?;
            let id = parse_id("token", &p.id)?;
            call.store().auth().revoke_token(user, id).await?;
            call.kernel.conns.revoke_token(id);
            call.audit(
                "token.revoke",
                Some(&user_uri(user)),
                serde_json::json!({ "token": id }),
            )
            .await;
            Ok(Empty {})
        })
        .summary("Ends one of the caller's access tokens, and its connections.")
        .level(Level::Own);
    methods
        .add("auth.createLink", |call: Call, p: LinkCreate| async move { create_link(&call, p).await })
        .summary("Makes a one-time sign-in link: a device link for oneself, an invite or a device link for another as admin, the first admin's setup link on the server's own machine.")
        .level(Level::Own);
    methods
        .add("auth.listLinks", |call: Call, _: Empty| async move {
            require_local_accounts(&call)?;
            // Admins see every link that still works; others only their own.
            let by = if call.is_admin() {
                None
            } else {
                Some(call.user_id()?)
            };
            let links = call.store().links().pending(by).await?;
            Ok(LinkList {
                links: links.into_iter().map(link_info).collect(),
            })
        })
        .summary(
            "The sign-in links that still work: all of them for an admin, one's own for others.",
        )
        .level(Level::Own);
    methods
        .add("auth.revokeLink", |call: Call, p: IdRef| async move {
            require_local_accounts(&call)?;
            let id = parse_id("link", &p.id)?;
            let links = call.store().links();
            let mine = match &call.caller {
                Caller::User { .. } => Some(call.user_id()?),
                _ => None,
            };
            match links.get(id).await? {
                Some(link) if call.is_admin() || (mine.is_some() && link.created_by == mine) => {
                    let link = links.revoke(id).await?;
                    let details = serde_json::json!({ "kind": link.kind, "link": link.id });
                    call.audit(
                        "link.revoke",
                        link.user_id.map(user_uri).as_deref(),
                        details,
                    )
                    .await;
                    Ok(Empty {})
                }
                _ => Err(RpcError::not_found("no such link")),
            }
        })
        .summary("Ends a sign-in link that was not used: any as admin, one's own otherwise.")
        .level(Level::Own);
}
