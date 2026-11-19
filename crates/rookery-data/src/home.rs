//! What a shell's home screen, command palette, search and sidebar read: search over the entity index
//! (`search.query`), what each person opened and ran last (`recents.*`), pins (`pins.*`) and what
//! changed lately in their spaces (`activity.recent`). Everything here is the signed-in person's.

use rookery_kernel::{Call, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::home::{
    ActivityItem, ActivityList, ActivityQuery, PinAdd, PinInfo, PinKind, PinList, PinRemove,
    PinRemoved, PinsChanged, PinsQuery, RecentCommand, RecentEntity, RecentForget, RecentKind,
    RecentTouch, RecentTouched, RecentsList, RecentsQuery, SearchFacet, SearchHit, SearchParams,
    SearchResult,
};
use rookery_store::pins::{NewPin, Pin, PinKind as StoreKind};
use rookery_store::search::SearchQuery;
use uuid::Uuid;

use crate::entities::entity_info;
use crate::{parse_id, rpc, store_caller, time};

fn ids(list: &Option<Vec<String>>) -> Result<Option<Vec<Uuid>>, RpcError> {
    list.as_ref()
        .map(|l| l.iter().map(|s| parse_id("space", s)).collect())
        .transpose()
}

pub fn register(methods: &mut Methods) {
    methods
        .add("search.query", search)
        .summary("Entities the caller may see that match words, best first, with counts per scheme; with no words, what they opened and what changed last.")
        .level(Level::Own);
    methods
        .add("recents.touch", |call: Call, p: RecentTouch| async move {
            call.user_id()?;
            let caller = store_caller(&call, None)?;
            if p.key.is_empty() || p.key.len() > 500 {
                return Err(RpcError::invalid(
                    "a recent item needs a key of 1 to 500 bytes",
                ));
            }
            let recents = call.store().recents();
            let recorded = match p.kind {
                RecentKind::Entity => recents.touch_entity(&caller, &p.key).await.map_err(rpc)?,
                RecentKind::Command => {
                    recents.touch_command(&caller, &p.key).await.map_err(rpc)?;
                    true
                }
            };
            Ok(RecentTouched { recorded })
        })
        .summary("Records that the caller opened an entity or ran a command.")
        .level(Level::Own);
    methods
        .add("recents.list", recents)
        .summary("What the caller opened or ran last, newest first.")
        .level(Level::Own);
    methods
        .add("recents.forget", |call: Call, p: RecentForget| async move {
            call.user_id()?;
            let caller = store_caller(&call, None)?;
            call.store()
                .recents()
                .forget(&caller, p.kind.as_str(), p.key.as_deref())
                .await
                .map_err(rpc)?;
            Ok(Empty {})
        })
        .summary("Forgets one recent item, or all of a kind.")
        .level(Level::Own);
    methods
        .add("pins.list", |call: Call, p: PinsQuery| async move {
            call.user_id()?;
            let caller = store_caller(&call, None)?;
            let space = p
                .space
                .as_deref()
                .map(|s| parse_id("space", s))
                .transpose()?;
            let pins = call
                .store()
                .pins()
                .list(&caller, space)
                .await
                .map_err(rpc)?;
            Ok(PinList {
                pins: pins.iter().map(pin_info).collect(),
            })
        })
        .summary("The caller's own pins, and a space's shared ones.")
        .level(Level::Own);
    methods
        .add("pins.add", add_pin)
        .summary("Pins an entity or a view, for the caller or shared in a space they hold use on.")
        .level(Level::Own);
    methods
        .add("pins.remove", |call: Call, p: PinRemove| async move {
            call.user_id()?;
            let caller = store_caller(&call, None)?;
            let removed = call
                .store()
                .pins()
                .remove(&caller, parse_id("pin", &p.id)?)
                .await
                .map_err(rpc)?;
            if let Some(pin) = &removed {
                changed(&call, pin.space_id).await;
            }
            Ok(PinRemoved {
                removed: removed.is_some(),
            })
        })
        .summary("Removes a pin.")
        .level(Level::Own);
    methods
        .add("activity.recent", activity)
        .summary(
            "What changed lately in the spaces the caller is in, runs of edits taken together.",
        )
        .level(Level::Own);
}

async fn search(call: Call, p: SearchParams) -> Result<SearchResult, RpcError> {
    let caller = store_caller(&call, None)?;
    let found = call
        .store()
        .search()
        .query(
            &caller,
            &SearchQuery {
                text: p.text,
                schemes: p.schemes,
                spaces: ids(&p.spaces)?,
                limit: p.limit,
                offset: p.offset,
                facets: p.facets.unwrap_or(false),
            },
        )
        .await
        .map_err(rpc)?;
    Ok(SearchResult {
        hits: found
            .hits
            .iter()
            .map(|h| SearchHit {
                entity: entity_info(&h.entity),
                rank: h.rank,
                title_match: h.title_match,
                recent: h.recent,
            })
            .collect(),
        facets: found.facets.map(|f| {
            f.into_iter()
                .map(|f| SearchFacet {
                    scheme: f.scheme,
                    count: f.count,
                })
                .collect()
        }),
        more: found.more,
    })
}

async fn recents(call: Call, p: RecentsQuery) -> Result<RecentsList, RpcError> {
    call.user_id()?;
    let caller = store_caller(&call, None)?;
    let limit = p.limit.unwrap_or(20);
    let recents = call.store().recents();
    let mut out = RecentsList {
        entities: Vec::new(),
        commands: Vec::new(),
    };
    match p.kind {
        RecentKind::Entity => {
            let space = p
                .space
                .as_deref()
                .map(|s| parse_id("space", s))
                .transpose()?;
            out.entities = recents
                .entities(&caller, p.schemes.as_deref(), space, limit)
                .await
                .map_err(rpc)?
                .iter()
                .map(|r| RecentEntity {
                    entity: entity_info(&r.entity),
                    opened_at: time(r.opened_at),
                })
                .collect();
        }
        RecentKind::Command => {
            out.commands = recents
                .commands(&caller, limit)
                .await
                .map_err(rpc)?
                .into_iter()
                .map(|c| RecentCommand {
                    id: c.id,
                    opened_at: time(c.opened_at),
                    count: c.count,
                })
                .collect();
        }
    }
    Ok(out)
}

fn pin_info(p: &Pin) -> PinInfo {
    PinInfo {
        id: p.id.to_string(),
        space_id: p.space_id.map(|s| s.to_string()),
        kind: match p.kind {
            StoreKind::Entity => PinKind::Entity,
            StoreKind::View => PinKind::View,
        },
        target: p.target.clone(),
        path: p.path.clone(),
        view_space: p.view_space.map(|s| s.to_string()),
        title: p.title.clone(),
        icon: p.icon.clone(),
        entity: p.entity.as_ref().map(entity_info),
    }
}

async fn add_pin(call: Call, p: PinAdd) -> Result<PinInfo, RpcError> {
    call.user_id()?;
    let caller = store_caller(&call, None)?;
    let uuid = |s: &Option<String>| -> Result<Option<Uuid>, RpcError> {
        s.as_deref().map(|s| parse_id("space", s)).transpose()
    };
    let new = NewPin {
        kind: match p.kind {
            PinKind::Entity => StoreKind::Entity,
            PinKind::View => StoreKind::View,
        },
        target: p.target,
        path: p.path.unwrap_or_default(),
        view_space: uuid(&p.view_space)?,
        shared_in: uuid(&p.shared_in)?,
        title: p.title.unwrap_or_default(),
        icon: p.icon,
    };
    let pin = call.store().pins().add(&caller, &new).await.map_err(rpc)?;
    changed(&call, new.shared_in).await;
    Ok(pin_info(&pin))
}

/// Tells the connections that show the pins: the person's own for a personal pin, those of the space's
/// members for a shared one.
async fn changed(call: &Call, shared_in: Option<Uuid>) {
    let payload = PinsChanged {
        space_id: shared_in.map(|s| s.to_string()),
    };
    match shared_in {
        None => {
            if let Some(user) = call.user() {
                call.kernel
                    .conns
                    .notify_user(&user.id, "pins.changed", &payload);
            }
        }
        Some(space) => notify_space(&call.kernel, space, "pins.changed", &payload).await,
    }
}

/// Sends a notification to the connections of the people who may view a space.
pub async fn notify_space(
    kernel: &Kernel,
    space: Uuid,
    method: &str,
    params: &impl serde::Serialize,
) {
    let uri = rookery_store::uri::space_uri(space);
    let mut seen = std::collections::HashMap::<String, bool>::new();
    for conn in kernel.conns.all() {
        let Some(user) = conn.caller().user() else {
            continue;
        };
        let allowed = match seen.get(&user.id) {
            Some(a) => *a,
            None => {
                let mut caller = match Uuid::parse_str(&user.id) {
                    Ok(id) => rookery_store::Caller::user(id),
                    Err(_) => continue,
                };
                caller.admin = user.admin;
                let a = kernel
                    .store
                    .access()
                    .level_of(&caller, &uri)
                    .await
                    .ok()
                    .flatten()
                    .is_some();
                seen.insert(user.id.clone(), a);
                a
            }
        };
        if allowed {
            conn.notify(method, params);
        }
    }
}

async fn activity(call: Call, p: ActivityQuery) -> Result<ActivityList, RpcError> {
    call.user_id()?;
    let caller = store_caller(&call, None)?;
    let space = p
        .space
        .as_deref()
        .map(|s| parse_id("space", s))
        .transpose()?;
    let items = call
        .store()
        .activity()
        .recent(&caller, space, p.limit.unwrap_or(20))
        .await
        .map_err(rpc)?;
    Ok(ActivityList {
        items: items
            .iter()
            .map(|i| ActivityItem {
                seq: i.seq,
                at: time(i.at),
                op: i.op.clone(),
                actor_id: i.actor_id.map(|a| a.to_string()),
                actor_name: i.actor_name.clone(),
                entity: entity_info(&i.entity),
                fields: i.fields.clone(),
                changes: i.changes,
            })
            .collect(),
    })
}
