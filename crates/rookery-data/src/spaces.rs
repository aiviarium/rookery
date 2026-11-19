//! `spaces.*`: every person's personal space, spaces of the kinds plugins contribute, and the current
//! space of each connection. Membership is a grant on `space://<id>`, which `access.grant`,
//! `access.revoke` and `access.grants` manage.
//!
//! A space is named by its id, its slug, or `~` for the caller's own personal space. A space the caller
//! may not see is `NOT_FOUND` when named by slug and `FORBIDDEN` when named by id, so slugs do not leak.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

use rookery_kernel::{Call, Empty, Grant, Kernel, Level, Methods, RpcError};
use rookery_proto::data::{
    SpaceCreate, SpaceInfo, SpaceKindInfo, SpaceKindList, SpaceList, SpaceRef, SpaceRename,
};
use rookery_store::spaces::{NewSpace, Space};
use uuid::Uuid;

use crate::{rpc, store_caller};

/// The slug that always means the caller's own personal space.
pub const PERSONAL: &str = "~";

/// The space kinds plugins contribute (`contributes.spaceKinds`), by plugin. The plugin system sets a
/// plugin's with [`set_kinds`] when it loads.
#[derive(Default)]
struct Kinds(RwLock<HashMap<String, Vec<SpaceKindInfo>>>);

/// Sets the space kinds a plugin contributes, replacing those it had; none removes them.
pub fn set_kinds(kernel: &Kernel, plugin: &str, kinds: Vec<SpaceKindInfo>) {
    let all = kernel.facility::<Kinds>();
    let mut all = all.0.write().unwrap();
    if kinds.is_empty() {
        all.remove(plugin);
    } else {
        all.insert(plugin.to_owned(), kinds);
    }
}

/// Every space kind the plugins contribute.
pub fn kinds(kernel: &Kernel) -> Vec<SpaceKindInfo> {
    let all = kernel.facility::<Kinds>();
    let mut out: Vec<SpaceKindInfo> = all.0.read().unwrap().values().flatten().cloned().collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The space each connection entered, by connection.
#[derive(Default)]
struct Current(Mutex<HashMap<u64, Uuid>>);

/// The space the call's connection entered, if it entered one.
pub fn current(call: &Call) -> Option<Uuid> {
    let conn = call.conn.as_ref()?;
    call.kernel
        .facility::<Current>()
        .0
        .lock()
        .unwrap()
        .get(&conn.id())
        .copied()
}

pub fn register(methods: &mut Methods) {
    methods
        .add("spaces.list", list)
        .summary("The spaces the caller may view: their personal space first.")
        .level(Level::Own);
    methods
        .add("spaces.kinds", |call: Call, _: Empty| async move {
            Ok(SpaceKindList {
                kinds: kinds(&call.kernel),
            })
        })
        .summary("The kinds of space plugins contribute.")
        .level(Level::Own);
    methods
        .add("spaces.get", |call: Call, p: SpaceRef| async move {
            let s = resolve(&call, &p.space).await?;
            info(&call, &s).await
        })
        .summary("A space by id, slug or ~, with the caller's level on it.")
        .level(Level::Own);
    methods
        .add("spaces.enter", |call: Call, p: SpaceRef| async move {
            let s = resolve(&call, &p.space).await?;
            let conn = call.conn()?.id();
            call.kernel
                .facility::<Current>()
                .0
                .lock()
                .unwrap()
                .insert(conn, s.id);
            info(&call, &s).await
        })
        .summary("Makes a space the connection's current one, which calls that name none are in.")
        .level(Level::Own);
    methods
        .add("spaces.create", create)
        .summary("Makes a space of a kind a plugin contributes, owned by the caller.")
        .level(Level::Own);
    methods
        .add("spaces.rename", |call: Call, p: SpaceRename| async move {
            let s = resolve(&call, &p.space).await?;
            let name = check_name(&p.name)?;
            let caller = store_caller(&call, None)?;
            let s = call
                .store()
                .spaces()
                .rename(&caller, s.id, &name)
                .await
                .map_err(rpc)?;
            info(&call, &s).await
        })
        .summary("Renames a space. Needs manage on it.")
        .level(Level::Own);
    methods.on_connection_closed(|kernel, conn| {
        kernel
            .facility::<Current>()
            .0
            .lock()
            .unwrap()
            .remove(&conn.id());
    });
}

fn check_name(name: &str) -> Result<String, RpcError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(RpcError::invalid("a space's name is 1 to 100 characters"));
    }
    Ok(name.to_owned())
}

async fn list(call: Call, _: Empty) -> Result<SpaceList, RpcError> {
    call.user_id()?;
    let caller = store_caller(&call, None)?;
    let spaces = call.store().spaces().list_for(&caller).await.map_err(rpc)?;
    let mut out = Vec::with_capacity(spaces.len());
    for s in &spaces {
        out.push(info(&call, s).await?);
    }
    Ok(SpaceList { spaces: out })
}

async fn create(call: Call, p: SpaceCreate) -> Result<SpaceInfo, RpcError> {
    call.user_id()?;
    let known = kinds(&call.kernel);
    if !known.iter().any(|k| k.id == p.kind) {
        return Err(RpcError::invalid(format!(
            "no plugin contributes the space kind {}",
            p.kind
        )));
    }
    let caller = store_caller(&call, None)?;
    let s = call
        .store()
        .spaces()
        .create(
            &caller,
            &NewSpace {
                kind: p.kind,
                name: check_name(&p.name)?,
                slug: p.slug,
                data: None,
            },
        )
        .await
        .map_err(rpc)?;
    info(&call, &s).await
}

/// A space by id, slug or `~`, if the caller may view it.
pub async fn resolve(call: &Call, space: &str) -> Result<Space, RpcError> {
    let store = call.store();
    let caller = store_caller(call, None)?;
    let by_slug = Uuid::parse_str(space).is_err();
    let s = if space == PERSONAL {
        Some(
            store
                .spaces()
                .personal(call.user_id()?)
                .await
                .map_err(rpc)?,
        )
    } else if by_slug {
        store.spaces().by_slug(space).await.map_err(rpc)?
    } else {
        store
            .spaces()
            .get(Uuid::parse_str(space).unwrap())
            .await
            .map_err(rpc)?
    };
    let Some(s) = s else {
        return Err(RpcError::not_found(format!("no space {space}")));
    };
    let level = store
        .access()
        .level_of(&caller, &s.uri())
        .await
        .map_err(rpc)?;
    if level.is_none() || (s.personal && s.owner_id != caller.user && !caller.admin) {
        return Err(if by_slug {
            RpcError::not_found(format!("no space {space}"))
        } else {
            RpcError::forbidden(format!("this needs view on space://{space}"))
        });
    }
    Ok(s)
}

async fn info(call: &Call, s: &Space) -> Result<SpaceInfo, RpcError> {
    let caller = store_caller(call, None)?;
    let level = call
        .store()
        .access()
        .level_of(&caller, &s.uri())
        .await
        .map_err(rpc)?;
    let own_personal = s.personal && s.owner_id == caller.user;
    Ok(SpaceInfo {
        id: s.id.to_string(),
        kind: s.kind.clone(),
        name: s.name.clone(),
        slug: if own_personal {
            PERSONAL.to_string()
        } else {
            s.slug.clone()
        },
        personal: s.personal,
        level: level.and_then(|l| Grant::from_number(l.as_i16())),
        owner: s.owner_id.map(|o| format!("user:{o}")),
    })
}
