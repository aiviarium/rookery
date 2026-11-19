//! The routes of the server's key ring: its keys with their public halves,
//! making or taking in a key, removing one, and rotating one as a job. A
//! private half comes in, as the bytes of a request, and never goes out.
//! All of them need the role `admin`.

use super::Serve;
use super::api::{Acted, Answer, Bytes, Fail, Json, internal};
use rookery_api::{Done, code};
use rookery_core::{
    keys::{self, contact, ring},
    runner::{Request, Subject, Work},
    store,
};
use std::sync::Arc;

pub use rookery_proto::hosts::{KeyInfo, KeyList};

fn info(id: String, key: ring::Public, hosts: Vec<String>) -> KeyInfo {
    KeyInfo {
        id,
        name: key.name,
        public: key.public,
        fingerprint: key.fingerprint,
        pending: key.pending.map(|next| {
            Box::new(KeyInfo {
                id: String::new(),
                name: next.name,
                public: next.public,
                fingerprint: next.fingerprint,
                pending: None,
                hosts: vec![],
            })
        }),
        hosts,
    }
}

/// The ring with the hosts that use each key.
pub fn listing(paths: &rookery_core::paths::Paths) -> rookery_core::Result<KeyList> {
    let store = store::shared(paths)?;
    let mut keys = vec![];
    for key in ring::list(paths, &store)? {
        let hosts = keys::users(&store, &key.name)?;
        let id = store
            .id_of(store::kind::KEY, store::OWN, &key.name)?
            .unwrap_or_default();
        keys.push(info(id, key, hosts));
    }
    Ok(KeyList { keys })
}

fn refused(error: Box<dyn std::error::Error>) -> Fail {
    let text = error.to_string();
    let kind = if text.starts_with("No key") {
        code::NOT_FOUND
    } else if text.contains("already")
        || text.contains("log in with")
        || text.contains("stays")
        || text.contains("being rotated")
    {
        code::CONFLICT
    } else {
        code::BAD_REQUEST
    };
    Fail::new(kind, text)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> rookery_core::Result<T> + Send + 'static,
) -> Answer<T> {
    // A key's halves come from the store, which waits, and /dev/urandom.
    let done = tokio::task::spawn_blocking(move || work().map_err(|error| error.to_string()))
        .await
        .map_err(internal)?;
    done.map_err(|error| refused(error.into()))
}

pub async fn list(state: Arc<Serve>) -> Answer<Json<KeyList>> {
    let paths = state.paths.clone();
    blocking(move || listing(&paths)).await.map(Json)
}

pub async fn add(state: Arc<Serve>, name: String, body: Bytes) -> Answer<Json<KeyInfo>> {
    let paths = state.paths.clone();
    blocking(move || {
        let store = store::shared(&paths)?;
        let private = (!body.is_empty()).then_some(&body[..]);
        let key = ring::add(&paths, &store, &name, private)?;
        let id = store
            .id_of(store::kind::KEY, store::OWN, &key.name)?
            .unwrap_or_default();
        Ok(info(id, key, vec![]))
    })
    .await
    .map(Json)
}

pub async fn remove(state: Arc<Serve>, name: String) -> Answer<Json<Done>> {
    let paths = state.paths.clone();
    blocking(move || {
        let store = store::shared(&paths)?;
        ring::remove(&paths, &store, &name)
    })
    .await?;
    Ok(Json(Done { ok: true }))
}

pub async fn rotate(state: Arc<Serve>, name: String) -> Answer<Json<Acted>> {
    start_rotation(&state, name, None).await.map(Json)
}

/// Start the rotation of a key as a job: for this route and for the
/// installation's action `rotate-key`.
pub(crate) async fn start_rotation(
    state: &std::sync::Arc<super::Serve>,
    name: String,
    token: Option<String>,
) -> Answer<Acted> {
    let store = store::shared(&state.paths).map_err(internal)?;
    if ring::read(&store, &name, false)
        .map_err(internal)?
        .is_none()
        && name != rookery_api::profile::ROOKERY_KEY
    {
        return Err(Fail::new(code::NOT_FOUND, format!("No key {name}")));
    }
    let paths = state.paths.clone();
    let mut request = Request::new(Subject::Installation, &format!("rotate-key:{name}"));
    request.changes = true;
    request.token = token;
    let work = Work::Function(Box::new(move |out| {
        match contact::rotate(&paths, &name, out) {
            Ok(rotation) => rotation.finished,
            Err(error) => {
                use std::io::Write;
                let _ = writeln!(out, "✗ {error}");
                false
            }
        }
    }));
    let job = state
        .runner
        .start(request, work)
        .await
        .map_err(|error| Fail::new(code::CONFLICT, error.to_string()))?;
    Ok(Acted {
        job: Some(job),
        ..Acted::default()
    })
}
