//! `blobs.*`: files kept under `ROOKERY_BLOBS`. Whoever put one, and admins, may read it.

use base64::Engine;
use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Kernel, Level, Methods, RpcError};
use rookery_proto::IdRef;
use rookery_proto::facilities::{BlobInfo, BlobPut, BlobUrl, BlobUrlInfo};
use rookery_proto::stream::StreamOpened;
use rookery_store::blobs::Blob;
use uuid::Uuid;

use crate::{parse_id, time};

/// The most `blobs.put` takes in one call.
const MAX_PUT: usize = 8 * 1024 * 1024;
/// The longest a signed blob address is good for.
const MAX_URL: i64 = 3600;
/// The default, for an address a page loads at once.
const DEFAULT_URL: i64 = 300;

/// What a signed address covers, so one blob's signature opens no other and expires.
fn signing(id: Uuid, expires: i64) -> String {
    format!("blob\n{id}\n{expires}")
}

/// The address of a blob on the server's origin, good until `expires` (Unix seconds).
pub fn url(kernel: &Kernel, id: Uuid, expires: i64) -> Result<String, RpcError> {
    let sig = kernel.store.key()?.sign(signing(id, expires).as_bytes());
    Ok(format!(
        "{}/blobs/{id}?expires={expires}&sig={sig}",
        kernel.config.public_url
    ))
}

/// The blob a valid signature names, without a caller's check: the address is the grant.
pub async fn signed(kernel: &Kernel, id: &str) -> Option<Blob> {
    let id = Uuid::parse_str(id).ok()?;
    kernel.store.blobs().get(id).await.ok().flatten()
}

/// Whether a signature from `/blobs/<id>` reads that blob until `expires`, now being `now`.
pub fn verify(kernel: &Kernel, id: &str, expires: i64, sig: &str, now: i64) -> bool {
    let Ok(id) = Uuid::parse_str(id) else {
        return false;
    };
    if expires < now {
        return false;
    }
    let Ok(key) = kernel.store.key() else {
        return false;
    };
    let want = key.sign(signing(id, expires).as_bytes());
    want.len() == sig.len()
        && want
            .bytes()
            .zip(sig.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn info(b: Blob) -> BlobInfo {
    BlobInfo {
        id: b.id.to_string(),
        sha256: b.sha256,
        size: b.size,
        content_type: b.content_type,
        name: b.name,
        created_at: time(b.created_at),
    }
}

/// A blob the caller may read: whoever put it, and admins.
pub async fn readable(call: &Call, id: &str) -> Result<Blob, RpcError> {
    let found = call.store().blobs().get(parse_id("blob", id)?).await?;
    let owner = call.principal().uri();
    match found {
        Some(blob) if call.is_admin() || blob.owner == owner || blob.owner == call.audit_name() => {
            Ok(blob)
        }
        _ => Err(RpcError::not_found("no such blob")),
    }
}

pub fn register(methods: &mut Methods) {
    methods
        .add("blobs.put", |call: Call, p: BlobPut| async move {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&p.data)
                .map_err(|_| RpcError::invalid("data is base64"))?;
            if bytes.len() > MAX_PUT {
                return Err(RpcError::invalid("blobs.put takes at most 8 MiB"));
            }
            let blob = call
                .store()
                .blobs()
                .put(
                    &bytes,
                    &p.content_type,
                    p.name.as_deref(),
                    &call.principal().uri(),
                )
                .await?;
            Ok(info(blob))
        })
        .summary("Keeps a file of up to 8 MiB, given in base64.")
        .level(Level::Own);
    methods
        .add("blobs.get", |call: Call, p: IdRef| async move {
            Ok(info(readable(&call, &p.id).await?))
        })
        .summary("What a blob is: its size, type and hash.")
        .level(Level::Own);
    methods
        .add("blobs.url", |call: Call, p: BlobUrl| async move {
            let blob = readable(&call, &p.id).await?;
            let secs = p.expires_secs.unwrap_or(DEFAULT_URL).clamp(1, MAX_URL);
            let expires = chrono::Utc::now().timestamp() + secs;
            Ok(BlobUrlInfo {
                url: url(&call.kernel, blob.id, expires)?,
                expires_at: time(chrono::DateTime::from_timestamp(expires, 0).unwrap_or_default()),
            })
        })
        .summary("A short-lived address of a blob on the server's origin, up to an hour.")
        .level(Level::Own);
    methods
        .add("blobs.delete", |call: Call, p: IdRef| async move {
            let blob = readable(&call, &p.id).await?;
            call.store().blobs().delete(blob.id).await?;
            call.audit("blob.delete", None, serde_json::json!({ "blob": blob.id }))
                .await;
            Ok(rookery_proto::Empty {})
        })
        .summary("Deletes a blob the caller put.")
        .level(Level::Own);
    methods
        .add("blobs.read", |call: Call, p: IdRef| async move {
            let blob = readable(&call, &p.id).await?;
            let bytes = call.store().blobs().read(&blob).await?;
            let stream = call.conn()?.streams().open(Initiator::Peer)?;
            let id = stream.id();
            tokio::spawn(async move {
                if stream.write(&bytes).await.is_ok() {
                    stream.eof();
                    stream.close(None);
                }
            });
            Ok(StreamOpened {
                stream: id,
                offset: None,
            })
        })
        .summary("A blob's bytes, on a stream.")
        .stream()
        .level(Level::Own);
}
