//! `data.*`: reads and writes of collections, batches, imports, history and references.
//!
//! People mentioned by a write (in rich text) get a `mention` notification. A batch's jobs wake the
//! worker once it commits, and its topic messages their streams.

use rookery_kernel::{Call, Caller, Level, Methods, RpcError};
use rookery_proto::data::{
    Backlink, BacklinkList, BatchOp, DataAggregate, DataAggregateResult, DataBatch,
    DataBatchResult, DataCount, DataFind, DataImport, DataImportResult, DataInsert, DataPage,
    DataQuery, DataRow, DataRowRef, DataSearch, DataTail, DataTailResult, DataTarget, DataUpdate,
    DataUpsert, HistoryEntry, HistoryList, ImportFailure, Reference, ReferenceList, RefsOf,
};
use rookery_store::Caller as StoreCaller;
use rookery_store::collections::{
    AggregateQuery, BatchJob, BatchOp as StoreBatchOp, ImportOptions, Mention, Query, TailQuery,
};
use serde_json::Value;

use crate::{parse_id, rpc, target, time, write_target};

pub fn register(methods: &mut Methods) {
    methods
        .add("data.find", find)
        .summary("A page of the rows of a collection the caller may read: where, orderBy, search, select, include, nearest.")
        .level(Level::Own);
    methods
        .add("data.scan", scan)
        .summary("Every matching row of a collection in id order, a page at a time.")
        .level(Level::Own);
    methods
        .add("data.count", count)
        .summary("The number of rows of a collection that match where and search.")
        .level(Level::Own);
    methods
        .add("data.aggregate", aggregate)
        .summary("Grouped counts, sums, averages and extremes over the rows the caller may read.")
        .level(Level::Own);
    methods
        .add("data.search", search)
        .summary("Rows of a collection that match words of its search fields, best first.")
        .level(Level::Own);
    methods
        .add("data.get", get)
        .summary("One row, or null when there is none the caller may read.")
        .level(Level::Own);
    methods
        .add("data.tail", tail)
        .summary("The rows of a log collection after a seq, oldest first.")
        .level(Level::Own);
    methods
        .add("data.insert", insert)
        .summary("Inserts a row.")
        .level(Level::Own);
    methods
        .add("data.upsert", upsert)
        .summary("Inserts a row, or updates the one whose fields in on match.")
        .level(Level::Own);
    methods
        .add("data.update", update)
        .summary("Changes fields of a row. With rev, only at that revision; CONFLICT otherwise.")
        .level(Level::Own);
    methods
        .add("data.delete", delete)
        .summary("Deletes a row: to the trash when the collection has one.")
        .level(Level::Own);
    methods
        .add("data.restore", restore)
        .summary("Takes a row out of the trash.")
        .level(Level::Own);
    methods
        .add("data.purge", purge)
        .summary("Deletes a row in the trash for good. Needs manage on it.")
        .level(Level::Own);
    methods
        .add("data.batch", batch)
        .summary("Writes, topic messages and jobs in one transaction: all of them or none.")
        .level(Level::Own);
    methods
        .add("data.import", import)
        .summary("Up to 500 rows in one transaction; the rows that fail are reported by index.")
        .level(Level::Own);
    methods
        .add("data.history", history)
        .summary("The field by field history of a row, newest first.")
        .level(Level::Own);
    methods
        .add("data.refsTo", refs_to)
        .summary("What refers to an entity, across plugins, as far as the caller may see.")
        .level(Level::Own);
    methods
        .add("data.refsFrom", refs_from)
        .summary("What an entity refers to, with those that are gone marked dangling.")
        .level(Level::Own);
}

pub(crate) fn query(q: &DataQuery) -> Result<Query, RpcError> {
    serde_json::to_value(q)
        .and_then(serde_json::from_value)
        .map_err(|e| RpcError::invalid(format!("query: {e}")))
}

fn ops(
    ops: &Option<std::collections::BTreeMap<String, rookery_proto::data::FieldOp>>,
) -> Result<rookery_store::collections::operations::FieldOps, RpcError> {
    serde_json::to_value(ops.as_ref().cloned().unwrap_or_default())
        .and_then(serde_json::from_value)
        .map_err(|e| RpcError::invalid(format!("ops: {e}")))
}

async fn find(call: Call, p: DataFind) -> Result<DataPage, RpcError> {
    let (plugin, caller) = target(&call, &p.target).await?;
    let page = call
        .store()
        .data(&plugin)
        .find(&caller, &p.target.collection, &query(&p.query)?)
        .await
        .map_err(rpc)?;
    Ok(DataPage {
        rows: page.rows,
        next: page.next,
        included: page.included,
    })
}

async fn scan(call: Call, p: DataFind) -> Result<DataPage, RpcError> {
    let (plugin, caller) = target(&call, &p.target).await?;
    let page = call
        .store()
        .data(&plugin)
        .scan(&caller, &p.target.collection, &query(&p.query)?)
        .await
        .map_err(rpc)?;
    Ok(DataPage {
        rows: page.rows,
        next: page.next,
        included: None,
    })
}

async fn count(call: Call, p: DataFind) -> Result<DataCount, RpcError> {
    let (plugin, caller) = target(&call, &p.target).await?;
    let count = call
        .store()
        .data(&plugin)
        .count(&caller, &p.target.collection, &query(&p.query)?)
        .await
        .map_err(rpc)?;
    Ok(DataCount { count })
}

async fn aggregate(call: Call, p: DataAggregate) -> Result<DataAggregateResult, RpcError> {
    let (plugin, caller) = target(&call, &p.target).await?;
    let mut v = serde_json::to_value(&p).map_err(|e| RpcError::invalid(e.to_string()))?;
    // The target is the call's, not part of the aggregate.
    for k in ["plugin", "collection", "space"] {
        v.as_object_mut().map(|m| m.remove(k));
    }
    let q: AggregateQuery =
        serde_json::from_value(v).map_err(|e| RpcError::invalid(format!("aggregate: {e}")))?;
    let rows = call
        .store()
        .data(&plugin)
        .aggregate(&caller, &p.target.collection, &q)
        .await
        .map_err(rpc)?;
    Ok(DataAggregateResult { rows })
}

async fn search(call: Call, p: DataSearch) -> Result<DataPage, RpcError> {
    let (plugin, caller) = target(&call, &p.target).await?;
    let page = call
        .store()
        .data(&plugin)
        .search(
            &caller,
            &p.target.collection,
            &p.text,
            p.filter,
            p.limit.unwrap_or(20),
        )
        .await
        .map_err(rpc)?;
    Ok(DataPage {
        rows: page.rows,
        next: page.next,
        included: page.included,
    })
}

async fn get(call: Call, p: DataRowRef) -> Result<DataRow, RpcError> {
    let (plugin, caller) = target(&call, &p.target).await?;
    let id = parse_id("id", &p.id)?;
    let data = call.store().data(&plugin);
    let row = if p.trashed {
        data.get_trashed(&caller, &p.target.collection, id).await
    } else {
        data.get(&caller, &p.target.collection, id).await
    }
    .map_err(rpc)?;
    Ok(DataRow { row })
}

async fn tail(call: Call, p: DataTail) -> Result<DataTailResult, RpcError> {
    let (plugin, caller) = target(&call, &p.target).await?;
    let q: TailQuery = serde_json::to_value(&p)
        .and_then(serde_json::from_value)
        .map_err(|e| RpcError::invalid(format!("tail: {e}")))?;
    let out = call
        .store()
        .data(&plugin)
        .tail(&caller, &p.target.collection, &q)
        .await
        .map_err(rpc)?;
    Ok(DataTailResult {
        rows: out.rows,
        seq: out.seq,
    })
}

async fn insert(call: Call, p: DataInsert) -> Result<DataRow, RpcError> {
    let (plugin, caller) = write_target(&call, &p.target).await?;
    let out = call
        .store()
        .data(&plugin)
        .insert(&caller, &p.target.collection, p.values)
        .await
        .map_err(rpc)?;
    mentioned(&call, &caller, &plugin, &out.mentions, out.row.as_ref()).await;
    Ok(DataRow { row: out.row })
}

async fn upsert(call: Call, p: DataUpsert) -> Result<DataRow, RpcError> {
    let (plugin, caller) = write_target(&call, &p.target).await?;
    let out = call
        .store()
        .data(&plugin)
        .upsert(&caller, &p.target.collection, p.values, &p.on)
        .await
        .map_err(rpc)?;
    mentioned(&call, &caller, &plugin, &out.mentions, out.row.as_ref()).await;
    Ok(DataRow { row: out.row })
}

async fn update(call: Call, p: DataUpdate) -> Result<DataRow, RpcError> {
    let (plugin, caller) = write_target(&call, &p.target).await?;
    let id = parse_id("id", &p.id)?;
    let out = call
        .store()
        .data(&plugin)
        .update_ops(
            &caller,
            &p.target.collection,
            id,
            p.patch,
            p.rev,
            &ops(&p.ops)?,
        )
        .await
        .map_err(rpc)?;
    mentioned(&call, &caller, &plugin, &out.mentions, out.row.as_ref()).await;
    Ok(DataRow { row: out.row })
}

async fn delete(call: Call, p: DataRowRef) -> Result<DataRow, RpcError> {
    let (plugin, caller) = write_target(&call, &p.target).await?;
    let out = call
        .store()
        .data(&plugin)
        .delete(&caller, &p.target.collection, parse_id("id", &p.id)?)
        .await
        .map_err(rpc)?;
    Ok(DataRow { row: out.row })
}

async fn restore(call: Call, p: DataRowRef) -> Result<DataRow, RpcError> {
    let (plugin, caller) = write_target(&call, &p.target).await?;
    let out = call
        .store()
        .data(&plugin)
        .restore(&caller, &p.target.collection, parse_id("id", &p.id)?)
        .await
        .map_err(rpc)?;
    Ok(DataRow { row: out.row })
}

async fn purge(call: Call, p: DataRowRef) -> Result<DataRow, RpcError> {
    let (plugin, caller) = write_target(&call, &p.target).await?;
    call.store()
        .data(&plugin)
        .purge(&caller, &p.target.collection, parse_id("id", &p.id)?)
        .await
        .map_err(rpc)?;
    Ok(DataRow { row: None })
}

fn object(v: &Value) -> Result<serde_json::Map<String, Value>, RpcError> {
    match v {
        Value::Object(m) => Ok(m.clone()),
        _ => Err(RpcError::invalid("values are an object")),
    }
}

async fn batch(call: Call, p: DataBatch) -> Result<DataBatchResult, RpcError> {
    let t = DataTarget {
        plugin: p.plugin.clone(),
        collection: String::new(),
        space: p.space.clone(),
    };
    let (plugin, caller) = write_target(&call, &t).await?;
    let mut ops_ = Vec::with_capacity(p.ops.len());
    let mut topics = Vec::new();
    for op in &p.ops {
        ops_.push(match op {
            BatchOp::Insert { collection, values } => StoreBatchOp::Insert {
                collection: collection.clone(),
                values: object(values)?,
            },
            BatchOp::Upsert {
                collection,
                values,
                on,
            } => StoreBatchOp::Upsert {
                collection: collection.clone(),
                values: object(values)?,
                on: on.clone(),
            },
            BatchOp::Update {
                collection,
                id,
                patch,
                rev,
                ops: field_ops,
            } => StoreBatchOp::Update {
                collection: collection.clone(),
                id: parse_id("id", id)?,
                patch: object(patch)?,
                rev: *rev,
                ops: ops(field_ops)?,
            },
            BatchOp::Delete { collection, id } => StoreBatchOp::Delete {
                collection: collection.clone(),
                id: parse_id("id", id)?,
            },
            BatchOp::Restore { collection, id } => StoreBatchOp::Restore {
                collection: collection.clone(),
                id: parse_id("id", id)?,
            },
            // A plugin's topics and jobs are its own: `<plugin>/<name>`.
            BatchOp::Publish { topic, payload } => {
                if !matches!(&call.caller, Caller::Plugin(_)) {
                    return Err(RpcError::forbidden("only a plugin publishes in a batch"));
                }
                let topic = scoped(&plugin, topic)?;
                topics.push(topic.clone());
                StoreBatchOp::Publish {
                    topic,
                    payload: payload.clone(),
                }
            }
            BatchOp::Enqueue { job, params } => {
                if !matches!(&call.caller, Caller::Plugin(_)) {
                    return Err(RpcError::forbidden("only a plugin enqueues in a batch"));
                }
                StoreBatchOp::Enqueue {
                    job: BatchJob {
                        kind: scoped(&plugin, job)?,
                        params: params.clone(),
                        subject: None,
                        max_attempts: 1,
                    },
                }
            }
        });
    }
    let condition = p
        .condition
        .map(|c| serde_json::to_value(c).and_then(serde_json::from_value))
        .transpose()
        .map_err(|e| RpcError::invalid(e.to_string()))?;
    let out = call
        .store()
        .data(&plugin)
        .batch_if(&caller, &ops_, condition.as_ref(), &call.audit_name())
        .await
        .map_err(rpc)?;
    if !out.jobs.is_empty() {
        rookery_jobs::wake(&call.kernel);
    }
    for topic in &topics {
        rookery_jobs::topics::wake_topic(&call.kernel, topic);
    }
    mentioned(&call, &caller, &plugin, &out.mentions, None).await;
    Ok(DataBatchResult { rows: out.rows })
}

/// A name of the plugin's own: `<plugin>/<name>`, given with or without the prefix.
fn scoped(plugin: &str, name: &str) -> Result<String, RpcError> {
    let name = name.strip_prefix(&format!("{plugin}/")).unwrap_or(name);
    if name.is_empty() || name.len() > 200 || name.contains('/') {
        return Err(RpcError::invalid(format!(
            "{name:?} is no name of the plugin's own"
        )));
    }
    Ok(format!("{plugin}/{name}"))
}

async fn import(call: Call, p: DataImport) -> Result<DataImportResult, RpcError> {
    let (plugin, caller) = write_target(&call, &p.target).await?;
    let options: ImportOptions =
        serde_json::from_value(serde_json::json!({ "mode": p.mode, "on": p.on }))
            .map_err(|e| RpcError::invalid(format!("import: {e}")))?;
    let out = call
        .store()
        .data(&plugin)
        .import(&caller, &p.target.collection, &p.rows, &options)
        .await
        .map_err(rpc)?;
    Ok(DataImportResult {
        inserted: out.inserted,
        updated: out.updated,
        failed: out
            .failed
            .into_iter()
            .map(|f| ImportFailure {
                index: f.index as u32,
                code: f.code,
                message: f.message,
                details: f.details,
            })
            .collect(),
    })
}

async fn history(call: Call, p: DataRowRef) -> Result<HistoryList, RpcError> {
    let (plugin, caller) = target(&call, &p.target).await?;
    let entries = call
        .store()
        .data(&plugin)
        .history(
            &caller,
            &p.target.collection,
            parse_id("id", &p.id)?,
            p.limit.unwrap_or(50),
        )
        .await
        .map_err(rpc)?;
    Ok(HistoryList {
        entries: entries
            .into_iter()
            .map(|e| HistoryEntry {
                op: e.op,
                rev: e.rev,
                principal: e.principal,
                on_behalf_of: e.on_behalf_of.map(|u| format!("user:{u}")),
                at: time(e.at),
                changes: e.changes,
            })
            .collect(),
    })
}

async fn refs_to(call: Call, p: RefsOf) -> Result<BacklinkList, RpcError> {
    let caller = crate::store_caller(&call, None)?;
    let links = call.store().refs().to(&caller, &p.uri).await.map_err(rpc)?;
    Ok(BacklinkList {
        backlinks: links
            .into_iter()
            .map(|b| Backlink {
                source: b.source,
                field: b.field,
                plugin: b.plugin,
                collection: b.collection,
                row_id: b.row_id.map(|r| r.to_string()),
                title: b.title,
                icon: b.icon,
                subtitle: b.subtitle,
                at: time(b.at),
            })
            .collect(),
    })
}

async fn refs_from(call: Call, p: RefsOf) -> Result<ReferenceList, RpcError> {
    let caller = crate::store_caller(&call, None)?;
    let refs = call
        .store()
        .refs()
        .from(&caller, &p.uri)
        .await
        .map_err(rpc)?;
    Ok(ReferenceList {
        references: refs
            .into_iter()
            .map(|r| Reference {
                field: r.field,
                target: r.target,
                dangling: r.dangling,
                title: r.title,
                icon: r.icon,
            })
            .collect(),
    })
}

/// Puts a `mention` notification in the inbox of each person a write mentioned, besides the writer.
/// A failure is logged and does not fail the write.
async fn mentioned(
    call: &Call,
    caller: &StoreCaller,
    plugin: &str,
    mentions: &[Mention],
    row: Option<&Value>,
) {
    if mentions.is_empty() {
        return;
    }
    let who = call
        .user()
        .map(|u| u.name.clone())
        .unwrap_or_else(|| plugin.to_string());
    let title = mentions.first().and_then(|m| {
        let coll = call.store().collections().get(plugin, &m.collection).ok()?;
        let field = coll.def.entity.as_ref()?.title.clone();
        row.and_then(|r| r.get(&field))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let text = match title {
        Some(t) if !t.is_empty() => format!("{who} mentioned you in {t}"),
        _ => format!("{who} mentioned you"),
    };
    for m in mentions {
        if Some(m.user) == caller.user {
            continue;
        }
        let data = serde_json::json!({
            "plugin": m.plugin,
            "collection": m.collection,
            "rowId": m.row_id,
            "field": m.field,
            "entity": m.entity,
        });
        let sent = call
            .store()
            .notify()
            .send(rookery_store::notify::New {
                user: m.user,
                kind: "rookery/mention",
                title: &text,
                body: None,
                data: Some(&data),
                actions: None,
                sent_by: &caller.actor(),
            })
            .await;
        match sent {
            Ok(Some(n)) => call.kernel.conns.notify_user(
                &m.user.to_string(),
                "notify.new",
                rookery_jobs::notify::info(n),
            ),
            Ok(None) => {}
            Err(e) => tracing::warn!(plugin, error = %e, "a mention's notification failed"),
        }
    }
}
