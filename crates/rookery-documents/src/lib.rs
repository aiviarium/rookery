//! Collaborative documents.
//!
//! A document is a Yjs document with an entity `document://<id>` whose parent is the entity it is
//! attached to, so permissions inherit from it: `view` reads, `use` edits. Clients sync with the
//! y-protocols messages over a stream (`documents.open`); host halves read and edit with RPC
//! (`documents.read`, `documents.edit`). While a document is in use the kernel keeps a replica
//! ([`live`]) that stores every update, compacts them into a snapshot, writes the indexes on a debounce
//! (title, search text, mentions as references, mention notifications), and saves periodic versions.
//!
//! - `schema`: ProseMirror JSON in and out of the Yjs fragment, and what the indexes take from it.
//! - `markdown`: Markdown in and out.
//! - `edit`: server-side edit operations.

pub mod edit;
pub mod live;
pub mod markdown;
pub mod schema;

use rookery_proto::documents::{
    DocumentContent, DocumentConvert, DocumentConverted, DocumentCreate, DocumentEdit,
    DocumentEdited, DocumentFormat, DocumentInfo, DocumentList, DocumentListQuery, DocumentOpen,
    DocumentOpened, DocumentRead, DocumentRef, DocumentRestore, DocumentSaveVersion,
    DocumentVersion, DocumentVersions,
};
use rookery_proto::{ErrorBody, ErrorCode, RpcError};
use rookery_store::documents::{Document, NewDocument, Version};
use rookery_store::{Caller as StoreCaller, Level};
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{ReadTxn, StateVector, Transact, Update};

use live::{Cmd, Who};
use rookery_kernel::streams::{Inbound, Initiator};
use rookery_kernel::{Call, Caller, Kernel, Level as MethodLevel, Methods};

/// The largest y-protocols message a client may send (a paste of a large document fits).
const MAX_MESSAGE: usize = 32 << 20;

pub fn register(methods: &mut Methods) {
    methods
        .add("documents.create", |call: Call, p: DocumentCreate| async move {
            let caller = rookery_data::store_caller(&call, None)?;
            let blocks = match (&p.content, &p.markdown) {
                (None, None) => None,
                (c, m) => Some(edit::content_blocks(c, m)?),
            };
            // An empty document starts with one empty paragraph, so editors that open it at the same time
            // do not each add a first paragraph of their own.
            let start = blocks
                .clone()
                .unwrap_or_else(|| vec![serde_json::json!({ "type": "paragraph" })]);
            let initial = {
                let doc = live::new_doc();
                let frag = doc.get_or_insert_xml_fragment(schema::FRAGMENT);
                {
                    let mut txn = doc.transact_mut();
                    schema::insert_blocks(&mut txn, &frag, 0, &start);
                }
                Some(doc.transact().encode_state_as_update_v1(&StateVector::default()))
            };
            let space_id = match &p.space {
                Some(s) => Some(parse_id("space", s)?),
                None if p.parent.is_none() => caller.space,
                None => None,
            };
            let id = Uuid::now_v7();
            call.store()
                .documents()
                .create(
                    &caller,
                    &NewDocument {
                        id,
                        parent: p.parent.clone(),
                        space_id,
                        plugin: crate::plugin_of(&call).map(str::to_string),
                        title: p.title.clone().unwrap_or_default(),
                        initial,
                    },
                )
                .await
                .rpc()?;
            if let Some(b) = blocks {
                let content = serde_json::json!({ "type": "doc", "content": b });
                live::index_content(&call.kernel, id, &content, Some(&who(&call)?)).await?;
            }
            info(&call, &caller, id).await.map(|(_, i)| i)
        })
        .summary("Makes a document attached to an entity, whose access and collection rules it inherits.")
        .level(MethodLevel::Own);
    methods
        .add("documents.get", |call: Call, p: DocumentRef| async move {
            let caller = rookery_data::store_caller(&call, None)?;
            let id = parse_id("document", &p.id)?;
            info(&call, &caller, id).await.map(|(_, i)| i)
        })
        .summary("A document's record and the caller's level on it.")
        .level(MethodLevel::Own);
    methods
        .add(
            "documents.list",
            |call: Call, p: DocumentListQuery| async move {
                let caller = rookery_data::store_caller(&call, None)?;
                let docs = call
                    .store()
                    .documents()
                    .list(&caller, &p.parent)
                    .await
                    .rpc()?;
                let mut out = Vec::with_capacity(docs.len());
                for d in docs {
                    if let Some(level) = call.store().documents().level(&caller, &d).await.rpc()? {
                        out.push(describe(&d, level));
                    }
                }
                Ok(DocumentList { documents: out })
            },
        )
        .summary("The documents attached to an entity.")
        .level(MethodLevel::Own);
    methods
        .add(
            "documents.delete",
            |call: Call, p: DocumentRef| async move {
                let caller = rookery_data::store_caller(&call, None)?;
                let id = parse_id("document", &p.id)?;
                require(&call, &caller, id, Level::Use).await?;
                call.store().documents().delete(id).await.rpc()?;
                live::send_if_live(&call.kernel, id, Cmd::Close);
                Ok(())
            },
        )
        .summary("Deletes a document. Needs write access on it.")
        .level(MethodLevel::Own);
    methods
        .add("documents.open", |call: Call, p: DocumentOpen| async move {
            let conn = call.conn()?.clone();
            let caller = rookery_data::store_caller(&call, None)?;
            let id = parse_id("document", &p.id)?;
            let (lvl, document) = info(&call, &caller, id).await?;
            let write = lvl >= Level::Use;
            let mut stream = conn.streams().open(Initiator::Peer)?;
            let sid = stream.id();
            let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let sub = live::next_sub();
            let kernel = call.kernel.clone();
            live::send(
                &kernel,
                id,
                Cmd::Join {
                    sub,
                    tx,
                    write,
                    who: who(&call)?,
                },
            );
            tokio::spawn(async move {
                let mut buf: Vec<u8> = Vec::new();
                loop {
                    tokio::select! {
                        item = stream.read() => match item {
                            Some(Inbound::Data(d)) => {
                                buf.extend_from_slice(&d);
                                loop {
                                    match take_frame(&mut buf) {
                                        Ok(Some(msg)) => live::send(&kernel, id, Cmd::Message { sub, data: msg }),
                                        Ok(None) => break,
                                        Err(e) => {
                                            stream.close(Some(ErrorBody { code: ErrorCode::Invalid, message: e, details: None }));
                                            live::send_if_live(&kernel, id, Cmd::Leave { sub });
                                            return;
                                        }
                                    }
                                }
                            }
                            _ => break,
                        },
                        out = rx.recv() => match out {
                            Some(msg) => {
                                if stream.write(&frame(&msg)).await.is_err() {
                                    break;
                                }
                            }
                            None => {
                                stream.close(None);
                                break;
                            }
                        },
                    }
                }
                live::send_if_live(&kernel, id, Cmd::Leave { sub });
            });
            Ok(DocumentOpened {
                stream: sid,
                document,
                read_only: !write,
            })
        })
        .summary("Opens a document for live editing: Yjs sync messages on a stream.")
        .level(MethodLevel::Own);
    methods
        .add("documents.read", |call: Call, p: DocumentRead| async move {
            let caller = rookery_data::store_caller(&call, None)?;
            let id = parse_id("document", &p.id)?;
            let (_, d) = info(&call, &caller, id).await?;
            let content = match &p.version {
                Some(v) => {
                    let v = parse_id("version", v)?;
                    let (_, data) = call.store().documents().version(id, v).await.rpc()?;
                    content_of_state(&data)?
                }
                None => live::ask(&call.kernel, id, |reply| Cmd::Read { reply }).await?,
            };
            Ok(render(
                &call.kernel,
                &d,
                content,
                p.format.unwrap_or_default(),
            ))
        })
        .summary("A document's content as ProseMirror JSON, Markdown or text.")
        .level(MethodLevel::Own);
    methods
        .add(
            "documents.convert",
            |call: Call, p: DocumentConvert| async move {
                rookery_data::store_caller(&call, None)?;
                let templates = live::BlockTemplates::of(&call.kernel);
                match (p.markdown, p.content) {
                    (Some(md), None) => {
                        if md.len() > 1024 * 1024 {
                            return Err(RpcError::invalid("at most 1 MiB of Markdown"));
                        }
                        let blocks = schema::normalize_blocks(&markdown::from_markdown(&md));
                        Ok(DocumentConverted {
                            markdown: None,
                            content: Some(serde_json::json!({ "type": "doc", "content": blocks })),
                        })
                    }
                    (None, Some(content)) => {
                        let blocks = schema::normalize_content(&content);
                        Ok(DocumentConverted {
                            markdown: Some(markdown::to_markdown(&blocks, &|n| {
                                templates.markdown(n)
                            })),
                            content: None,
                        })
                    }
                    _ => Err(RpcError::invalid("give either markdown or content")),
                }
            },
        )
        .summary("Converts between Markdown and ProseMirror JSON, with no document.")
        .level(MethodLevel::Own);
    methods
        .add("documents.edit", |call: Call, p: DocumentEdit| async move {
            let caller = rookery_data::store_caller(&call, None)?;
            let id = parse_id("document", &p.id)?;
            require(&call, &caller, id, Level::Use).await?;
            let who = who(&call)?;
            let blocks = live::ask(&call.kernel, id, |reply| Cmd::Edit {
                ops: p.ops,
                author: p.author,
                who,
                reply,
            })
            .await?;
            Ok(DocumentEdited { blocks })
        })
        .summary("Edits a document on the server: replace, append, insert or set a section.")
        .level(MethodLevel::Own);
    methods
        .add(
            "documents.history",
            |call: Call, p: DocumentRef| async move {
                let caller = rookery_data::store_caller(&call, None)?;
                let id = parse_id("document", &p.id)?;
                require(&call, &caller, id, Level::View).await?;
                let versions = call.store().documents().versions(id, 500).await.rpc()?;
                Ok(DocumentVersions {
                    versions: versions.iter().map(version_info).collect(),
                })
            },
        )
        .summary("A document's saved versions, newest first.")
        .level(MethodLevel::Own);
    methods
        .add(
            "documents.saveVersion",
            |call: Call, p: DocumentSaveVersion| async move {
                let caller = rookery_data::store_caller(&call, None)?;
                let id = parse_id("document", &p.id)?;
                require(&call, &caller, id, Level::Use).await?;
                let who = who(&call)?;
                let v = live::ask(&call.kernel, id, |reply| Cmd::SaveVersion {
                    kind: "named",
                    name: p.name.filter(|n| !n.trim().is_empty()),
                    who,
                    reply,
                })
                .await?;
                Ok(version_info(&v))
            },
        )
        .summary("Saves the document's state now as a named version.")
        .level(MethodLevel::Own);
    methods
        .add("documents.restore", |call: Call, p: DocumentRestore| async move {
            let caller = rookery_data::store_caller(&call, None)?;
            let id = parse_id("document", &p.id)?;
            require(&call, &caller, id, Level::Use).await?;
            let version = parse_id("version", &p.version)?;
            let who = who(&call)?;
            let v = live::ask(&call.kernel, id, |reply| Cmd::Restore { version, who, reply }).await?;
            Ok(version_info(&v))
        })
        .summary("Makes a saved version the document's state again, keeping the state before it as a version.")
        .level(MethodLevel::Own);
    // Writes the indexes and compacts now, instead of after the debounce. For tests and for host halves
    // that read the indexes right after an edit.
    methods
        .add("documents.flush", |call: Call, p: DocumentRef| async move {
            let caller = rookery_data::store_caller(&call, None)?;
            let id = parse_id("document", &p.id)?;
            require(&call, &caller, id, Level::View).await?;
            live::ask(&call.kernel, id, |reply| Cmd::Flush { reply }).await
        })
        .summary("Writes a live document's state and indexes now.")
        .level(MethodLevel::Own);
}

/// The varuint-length framing of messages on a document stream.
pub fn frame(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len() + 5);
    let mut n = msg.len() as u64;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    out.extend_from_slice(msg);
    out
}

/// Takes one framed message off the front of `buf`, if a whole one is there.
pub fn take_frame(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    let mut len: u64 = 0;
    let mut shift = 0;
    let mut i = 0;
    loop {
        let Some(b) = buf.get(i) else {
            return Ok(None);
        };
        len |= u64::from(b & 0x7f) << shift;
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 35 {
            return Err("a message length is malformed".into());
        }
    }
    if len as usize > MAX_MESSAGE {
        return Err(format!("a message is larger than {MAX_MESSAGE} bytes"));
    }
    let end = i + len as usize;
    if buf.len() < end {
        return Ok(None);
    }
    let msg = buf[i..end].to_vec();
    buf.drain(..end);
    Ok(Some(msg))
}

fn who(call: &Call) -> Result<Who, RpcError> {
    let caller = rookery_data::store_caller(call, None)?;
    let name = match (call.user(), crate::plugin_of(call)) {
        (_, Some(p)) => p.to_string(),
        (Some(u), None) => u.name.clone(),
        (None, None) => caller.principal.clone(),
    };
    Ok(Who {
        actor: caller.actor(),
        user: caller.user,
        name,
    })
}

/// The document, if the caller has at least `min` on it. Missing and invisible are both `NOT_FOUND`.
async fn require(
    call: &Call,
    caller: &StoreCaller,
    id: Uuid,
    min: Level,
) -> Result<(Level, Document), RpcError> {
    let documents = call.store().documents();
    let doc = documents
        .get(id)
        .await
        .rpc()?
        .ok_or_else(|| RpcError::not_found("no such document"))?;
    match documents.level(caller, &doc).await.rpc()? {
        Some(held) if held >= min => Ok((held, doc)),
        Some(_) => Err(RpcError::forbidden(format!(
            "editing this document needs {} on it",
            min.as_str()
        ))),
        None => Err(RpcError::not_found("no such document")),
    }
}

async fn info(
    call: &Call,
    caller: &StoreCaller,
    id: Uuid,
) -> Result<(Level, DocumentInfo), RpcError> {
    let (l, d) = require(call, caller, id, Level::View).await?;
    Ok((l, describe(&d, l)))
}

fn describe(d: &Document, level: Level) -> DocumentInfo {
    DocumentInfo {
        id: d.id.to_string(),
        uri: d.uri(),
        parent: d.parent.clone(),
        source: d.source(),
        space_id: d.space_id.map(|s| s.to_string()),
        plugin: d.plugin.clone(),
        title: d.title.clone(),
        created_by: d.created_by.map(|u| u.to_string()),
        created_at: d.created_at.to_rfc3339(),
        updated_at: d.updated_at.to_rfc3339(),
        level: level.as_str().to_string(),
    }
}

fn version_info(v: &Version) -> DocumentVersion {
    DocumentVersion {
        id: v.id.to_string(),
        document_id: v.document_id.to_string(),
        kind: v.kind.clone(),
        name: v.name.clone(),
        created_by: v.created_by.clone(),
        created_at: v.created_at.to_rfc3339(),
        size: v.size,
    }
}

/// The ProseMirror JSON in a stored state.
pub fn content_of_state(data: &[u8]) -> Result<Value, RpcError> {
    let doc = live::new_doc();
    {
        let mut txn = doc.transact_mut();
        let u = Update::decode_v1(data).map_err(|e| RpcError::internal(e.to_string()))?;
        txn.apply_update(u)
            .map_err(|e| RpcError::internal(e.to_string()))?;
    }
    let frag = doc.get_or_insert_xml_fragment(schema::FRAGMENT);
    let txn = doc.transact();
    Ok(schema::to_json(&txn, &frag))
}

fn render(
    kernel: &Kernel,
    d: &DocumentInfo,
    content: Value,
    format: DocumentFormat,
) -> DocumentContent {
    let blocks: Vec<Value> = content
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let templates = live::BlockTemplates::of(kernel);
    let mut out = DocumentContent {
        id: d.id.clone(),
        title: d.title.clone(),
        content: None,
        markdown: None,
        text: None,
        blocks: blocks.len() as u32,
    };
    match format {
        DocumentFormat::Json => out.content = Some(content),
        DocumentFormat::Markdown => {
            out.markdown = Some(markdown::to_markdown(&blocks, &|n| templates.markdown(n)))
        }
        DocumentFormat::Text => {
            out.text = Some(schema::plain_text_of(&content, &|n| templates.text(n)))
        }
    }
    out
}

/// A store's result as a call's.
pub(crate) trait IntoRpc<T> {
    fn rpc(self) -> Result<T, RpcError>;
}

impl<T> IntoRpc<T> for Result<T, rookery_store::Error> {
    fn rpc(self) -> Result<T, RpcError> {
        self.map_err(Into::into)
    }
}

pub(crate) fn parse_id(what: &str, id: &str) -> Result<Uuid, RpcError> {
    Uuid::parse_str(id).map_err(|_| RpcError::invalid(format!("{what}: {id:?} is not an id")))
}

/// The plugin a call is from, when it is from one.
pub(crate) fn plugin_of(call: &Call) -> Option<&str> {
    match &call.caller {
        Caller::Plugin(p) => Some(p),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        let big = vec![7u8; 300];
        buf.extend(frame(b"ab"));
        buf.extend(frame(&big));
        let partial = buf.split_off(buf.len() - 10);
        assert_eq!(take_frame(&mut buf).unwrap(), Some(b"ab".to_vec()));
        assert_eq!(take_frame(&mut buf).unwrap(), None);
        buf.extend(partial);
        assert_eq!(take_frame(&mut buf).unwrap(), Some(big));
        assert!(buf.is_empty());
        let mut bad = vec![0xff; 8];
        assert!(take_frame(&mut bad).is_err());
    }
}
