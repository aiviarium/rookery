//! `blobs.*`: files kept under `ROOKERY_BLOBS`. Whoever put one, and admins, may read it.

use base64::Engine;
use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Level, Methods, RpcError};
use rookery_proto::IdRef;
use rookery_proto::facilities::{BlobInfo, BlobPut};
use rookery_proto::stream::StreamOpened;
use rookery_store::blobs::Blob;

use crate::{parse_id, time};

/// The most `blobs.put` takes in one call.
const MAX_PUT: usize = 8 * 1024 * 1024;

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

async fn readable(call: &Call, id: &str) -> Result<Blob, RpcError> {
    let found = call.store().blobs().get(parse_id("blob", id)?).await?;
    match found {
        Some(blob) if call.is_admin() || blob.owner == call.audit_name() => Ok(blob),
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
                    &call.audit_name(),
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
