//! Collaborative documents: the stored side. The kernel keeps the live Yjs replica and decides what goes
//! here; the store keeps each document's row, its entity `document://<id>`, its updates (one row each)
//! and the snapshot they are compacted into, its saved versions, and what its content puts into the
//! indexes: the entity's title and search text, and its mentions as references.
//!
//! Mentions in a document are references from the document's *source*: its parent entity, so that a
//! document attached to a note or an issue gives that note or issue its backlinks exactly as a `richtext`
//! field would, or the document itself when it has no parent or its parent is a space or a plugin.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Store;
use crate::caller::{Caller, Level};
use crate::entities::{EntityRecord, MAX_SEARCH_TEXT};
use crate::error::{Error, Result};
use crate::refs::RefSource;
use crate::uri::Uri;
use crate::util::truncate_str;

/// The entity scheme of documents.
pub const SCHEME: &str = "document";

pub fn uri(id: Uuid) -> String {
    format!("{SCHEME}://{id}")
}

/// The URI mentions in a document are references from (see the module docs).
pub fn source_of(id: Uuid, parent: Option<&str>) -> String {
    match parent {
        Some(p) if !p.starts_with("space://") && !p.starts_with("plugin://") => p.to_string(),
        _ => uri(id),
    }
}

/// The reference field a document's mentions are stored under, on its source.
pub fn ref_field(id: Uuid) -> String {
    format!("document:{id}")
}

/// SQL: entity `a` is a document attached to another entity. Search finds such a document as its parent, so it
/// is left out of results itself.
pub(crate) fn attached_sql(a: &str) -> String {
    format!(
        "({a}.scheme = 'document' AND {a}.parent IS NOT NULL \
          AND {a}.parent NOT LIKE 'space://%' AND {a}.parent NOT LIKE 'plugin://%')"
    )
}

/// SQL: entity `a` matches the tsquery `q` by its own search text or by that of a document attached to it.
pub(crate) fn match_sql(a: &str, q: &str) -> String {
    format!(
        "({a}.search @@ {q} OR EXISTS (SELECT 1 FROM rookery.entities d WHERE d.parent = {a}.uri \
          AND d.scheme = 'document' AND d.deleted_at IS NULL AND d.search @@ {q}))"
    )
}

/// SQL: the rank of entity `a` for `q`, counting the documents attached to it.
pub(crate) fn rank_sql(a: &str, q: &str) -> String {
    format!(
        "greatest(ts_rank({a}.search, {q}), coalesce((SELECT max(ts_rank(d.search, {q})) FROM rookery.entities d \
          WHERE d.parent = {a}.uri AND d.scheme = 'document' AND d.deleted_at IS NULL), 0))"
    )
}

pub struct NewDocument {
    pub id: Uuid,
    pub parent: Option<String>,
    /// Defaults to the parent's space.
    pub space_id: Option<Uuid>,
    pub plugin: Option<String>,
    pub title: String,
    /// The initial Yjs state, if the document starts with content.
    pub initial: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub id: Uuid,
    pub plugin: Option<String>,
    pub parent: Option<String>,
    pub space_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<String>,
    /// The title given at creation.
    pub given_title: String,
    /// The entity index's title.
    pub title: String,
}

impl Document {
    pub fn uri(&self) -> String {
        uri(self.id)
    }
    pub fn source(&self) -> String {
        source_of(self.id, self.parent.as_deref())
    }
}

/// A document's stored state: the snapshot and the updates after it, in order.
pub struct StoredState {
    pub snapshot: Option<Vec<u8>>,
    pub snapshot_seq: i64,
    pub updates: Vec<(i64, Vec<u8>)>,
}

impl StoredState {
    /// The sequence number the next update gets.
    pub fn next_seq(&self) -> i64 {
        self.updates
            .last()
            .map(|(s, _)| *s)
            .unwrap_or(self.snapshot_seq)
            + 1
    }
}

/// What a document's content puts into the indexes.
pub struct DocumentIndex {
    pub title: String,
    pub text: String,
    /// Entity URIs mentioned, in order, without duplicates.
    pub mentions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub id: Uuid,
    pub document_id: Uuid,
    pub kind: String,
    pub name: Option<String>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub size: i64,
}

const DOC_COLUMNS: &str = "d.id, d.plugin, d.parent, d.space_id, d.created_by, d.created_at, d.updated_at, \
    d.updated_by, d.title AS given_title, coalesce(e.title, d.title) AS title";

pub struct Documents<'a> {
    pub(crate) store: &'a Store,
}

fn owns_plugin(caller: &Caller, plugin: Option<&str>) -> bool {
    caller.user.is_none()
        && plugin.is_some_and(|p| caller.principal.strip_prefix("plugin:") == Some(p))
}

/// Whether the caller satisfies the attached collection's read and write rules on its row.
async fn attachment_access(
    conn: &mut PgConnection,
    caller: &Caller,
    parent: &str,
) -> Result<(bool, bool)> {
    if caller.admin {
        return Ok((true, true));
    }
    let rules: Option<(String, String, i16, i16)> = sqlx::query_as(
        "SELECT c.uri, c.plugin, rookery.collection_need(c.plugin, c.collection),
                 rookery.collection_need(c.plugin, c.collection, true)
         FROM rookery.entity_collection(coalesce(
             (SELECT target FROM rookery.entity_aliases WHERE uri = $1), $1), 15) c",
    )
    .bind(parent)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((row, plugin, read, write)) = rules else {
        return Ok((true, true));
    };
    if caller.admin_on(&plugin) || owns_plugin(caller, Some(&plugin)) {
        return Ok((true, true));
    }
    let held = crate::access::level(conn, caller, &row)
        .await?
        .map_or(0, Level::as_i16);
    Ok((held >= read, held >= write))
}

impl<'a> Documents<'a> {
    /// Creates a document and its entity. The caller needs `use` on the parent; a document without a
    /// parent needs a user, who owns it.
    pub async fn create(&self, caller: &Caller, new: &NewDocument) -> Result<Document> {
        let mut tx = self.store.pool().begin().await?;
        let mut space_id = new.space_id;
        let parent: Option<String> = match &new.parent {
            Some(parent) => Some(sqlx::query_scalar(
                "SELECT coalesce((SELECT target FROM rookery.entity_aliases WHERE uri = $1), $1)",
            ).bind(parent).fetch_one(&mut *tx).await?),
            None => None,
        };
        if let Some(parent) = &parent {
            Uri::parse(parent)?;
            let (read, write) = attachment_access(&mut tx, caller, parent).await?;
            if !(read && write) {
                return Err(Error::forbidden(
                    "the attached collection's write access is required",
                ));
            }
            let parent_plugin: Option<String> =
                sqlx::query_scalar("SELECT plugin FROM rookery.entities WHERE uri = $1")
                    .bind(parent)
                    .fetch_optional(&mut *tx)
                    .await?
                    .flatten();
            if !owns_plugin(caller, parent_plugin.as_deref()) {
                crate::access::require(&mut tx, caller, parent, Level::Use).await?;
            }
            if space_id.is_none() {
                space_id = if let Some(s) = parent.strip_prefix("space://") {
                    Uuid::parse_str(s).ok()
                } else {
                    sqlx::query_scalar("SELECT space_id FROM rookery.entities WHERE uri = $1")
                        .bind(parent)
                        .fetch_optional(&mut *tx)
                        .await?
                        .flatten()
                };
            }
        } else if caller.user.is_none() {
            return Err(Error::invalid(
                "a document without a parent needs a user to own it",
            ));
        }
        let title = truncate_str(new.title.trim(), 4096).to_string();
        sqlx::query(
            "INSERT INTO rookery.documents (id, plugin, parent, space_id, created_by, updated_by, title, snapshot) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(new.id)
        .bind(&new.plugin)
        .bind(&parent)
        .bind(space_id)
        .bind(caller.user)
        .bind(caller.actor())
        .bind(&title)
        .bind(&new.initial)
        .execute(&mut *tx)
        .await?;
        let rec = EntityRecord {
            uri: uri(new.id),
            space_id,
            parent,
            title: if title.is_empty() {
                "Untitled".into()
            } else {
                title.clone()
            },
            icon: Some("file".into()),
            owner_id: caller.user,
            plugin: new.plugin.clone(),
            ..Default::default()
        };
        crate::entities::upsert(&mut tx, &rec, None).await?;
        let doc = fetch(&mut tx, new.id).await?.expect("just inserted");
        tx.commit().await?;
        Ok(doc)
    }

    /// The document, without an access check (the kernel checks `document://<id>`).
    pub async fn get(&self, id: Uuid) -> Result<Option<Document>> {
        let mut conn = self.store.pool().acquire().await?;
        fetch(&mut conn, id).await
    }

    /// The document's usable level after applying its attachment's collection rules.
    pub async fn level(&self, caller: &Caller, doc: &Document) -> Result<Option<Level>> {
        let mut conn = self.store.pool().acquire().await?;
        let held = if owns_plugin(caller, doc.plugin.as_deref()) {
            Some(Level::Manage)
        } else {
            crate::access::level(&mut conn, caller, &doc.uri()).await?
        };
        let Some(held) = held else { return Ok(None) };
        let Some(parent) = &doc.parent else {
            return Ok(Some(held));
        };
        let (read, write) = attachment_access(&mut conn, caller, parent).await?;
        Ok(read.then_some(if write { held } else { Level::View }))
    }

    /// The documents attached to `parent` that the caller may view, oldest first.
    pub async fn list(&self, caller: &Caller, parent: &str) -> Result<Vec<Document>> {
        let sql = format!(
            "SELECT {DOC_COLUMNS} FROM rookery.documents d JOIN rookery.entities e ON e.scheme = 'document' AND e.id = d.id::text \
             WHERE d.parent = $1 AND {} ORDER BY d.created_at LIMIT 500",
            crate::entities::visible_sql("$2", "$3", "$4")
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
            .bind(parent)
            .bind(caller.user)
            .bind(caller.actor())
            .bind(caller.admin)
            .fetch_all(self.store.pool())
            .await?)
    }

    /// The snapshot and the updates after it.
    pub async fn load(&self, id: Uuid) -> Result<StoredState> {
        let row: Option<(Option<Vec<u8>>, i64)> =
            sqlx::query_as("SELECT snapshot, snapshot_seq FROM rookery.documents WHERE id = $1")
                .bind(id)
                .fetch_optional(self.store.pool())
                .await?;
        let (snapshot, snapshot_seq) = row.ok_or_else(|| Error::not_found("no such document"))?;
        let updates: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT seq, data FROM rookery.document_updates WHERE document_id = $1 AND seq > $2 ORDER BY seq",
        )
        .bind(id)
        .bind(snapshot_seq)
        .fetch_all(self.store.pool())
        .await?;
        Ok(StoredState {
            snapshot,
            snapshot_seq,
            updates,
        })
    }

    /// Stores one update.
    pub async fn append(&self, id: Uuid, seq: i64, data: &[u8], actor: Option<&str>) -> Result<()> {
        let mut tx = self.store.pool().begin().await?;
        sqlx::query(
            "INSERT INTO rookery.document_updates (document_id, seq, data, actor) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(seq)
        .bind(data)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE rookery.documents SET updated_at = now(), updated_by = coalesce($2, updated_by) WHERE id = $1")
            .bind(id)
            .bind(actor)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces the snapshot with one that holds every update up to `upto`, and drops those updates.
    pub async fn compact(&self, id: Uuid, snapshot: &[u8], upto: i64) -> Result<u64> {
        let mut tx = self.store.pool().begin().await?;
        sqlx::query("UPDATE rookery.documents SET snapshot = $2, snapshot_seq = $3 WHERE id = $1")
            .bind(id)
            .bind(snapshot)
            .bind(upto)
            .execute(&mut *tx)
            .await?;
        let n = sqlx::query(
            "DELETE FROM rookery.document_updates WHERE document_id = $1 AND seq <= $2",
        )
        .bind(id)
        .bind(upto)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(n)
    }

    /// How many updates are stored after the snapshot.
    pub async fn pending_updates(&self, id: Uuid) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM rookery.document_updates u JOIN rookery.documents d ON d.id = u.document_id \
             WHERE u.document_id = $1 AND u.seq > d.snapshot_seq",
        )
        .bind(id)
        .fetch_one(self.store.pool())
        .await?)
    }

    /// Puts what the content says into the indexes, in one transaction: the entity's title and search
    /// text, and the mentions as references from the document's source. Returns the users mentioned now
    /// who were not before.
    pub async fn index(&self, id: Uuid, ix: &DocumentIndex) -> Result<Vec<Uuid>> {
        let mut tx = self.store.pool().begin().await?;
        let doc = fetch(&mut tx, id)
            .await?
            .ok_or_else(|| Error::not_found("no such document"))?;
        let source = doc.source();
        let src = Uri::parse(&source)?;
        let field = ref_field(id);
        let before: Vec<String> = sqlx::query_scalar(
            "SELECT dst_id FROM rookery.refs WHERE src_scheme = $1 AND src_id = $2 AND field = $3 AND dst_scheme = 'user'",
        )
        .bind(&src.scheme)
        .bind(&src.id)
        .bind(&field)
        .fetch_all(&mut *tx)
        .await?;
        let (plugin, collection): (Option<String>, Option<String>) = if source == doc.uri() {
            (doc.plugin.clone(), None)
        } else {
            sqlx::query_as("SELECT plugin, collection FROM rookery.entities WHERE uri = $1")
                .bind(&source)
                .fetch_optional(&mut *tx)
                .await?
                .unwrap_or((doc.plugin.clone(), None))
        };
        let targets: Vec<Uri> = ix
            .mentions
            .iter()
            .filter_map(|m| Uri::parse(m).ok())
            .filter(|u| u.to_string() != source)
            .collect();
        let rs = RefSource {
            scheme: src.scheme.clone(),
            id: src.id.clone(),
            access_uri: doc.uri(),
            owner_id: None,
            plugin,
            collection,
        };
        crate::refs::replace(&mut tx, &rs, &field, &targets, false).await?;
        let title = if ix.title.trim().is_empty() {
            if doc.given_title.is_empty() {
                "Untitled".to_string()
            } else {
                doc.given_title.clone()
            }
        } else {
            truncate_str(ix.title.trim(), 4096).to_string()
        };
        let text = truncate_str(&ix.text, MAX_SEARCH_TEXT);
        sqlx::query(
            "UPDATE rookery.entities SET title = $2, \
                search = setweight(to_tsvector('simple', $2), 'A') || setweight(to_tsvector('simple', $3), 'B'), \
                updated_at = now() \
             WHERE scheme = 'document' AND id = $1",
        )
        .bind(id.to_string())
        .bind(&title)
        .bind(text)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE rookery.documents SET indexed_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let new = targets
            .iter()
            .filter(|u| u.scheme == "user" && !before.contains(&u.id))
            .filter_map(|u| Uuid::parse_str(&u.id).ok())
            .collect();
        Ok(new)
    }

    /// Deletes a document with its updates, versions, entity and the references its mentions made.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let mut tx = self.store.pool().begin().await?;
        let Some(doc) = fetch(&mut tx, id).await? else {
            return Ok(false);
        };
        let src = Uri::parse(&doc.source())?;
        sqlx::query(
            "DELETE FROM rookery.refs WHERE src_scheme = $1 AND src_id = $2 AND field = $3",
        )
        .bind(&src.scheme)
        .bind(&src.id)
        .bind(ref_field(id))
        .execute(&mut *tx)
        .await?;
        crate::entities::delete(&mut tx, &doc.uri()).await?;
        sqlx::query("DELETE FROM rookery.documents WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Saves a version of the document.
    pub async fn add_version(
        &self,
        id: Uuid,
        kind: &str,
        name: Option<&str>,
        data: &[u8],
        created_by: Option<&str>,
    ) -> Result<Version> {
        Ok(sqlx::query_as(
            "INSERT INTO rookery.document_versions (id, document_id, kind, name, data, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id, document_id, kind, name, created_by, created_at, length(data)::bigint AS size",
        )
        .bind(crate::util::new_id())
        .bind(id)
        .bind(kind)
        .bind(name.map(|n| truncate_str(n.trim(), 200)))
        .bind(data)
        .bind(created_by)
        .fetch_one(self.store.pool())
        .await?)
    }

    /// Versions, newest first.
    pub async fn versions(&self, id: Uuid, limit: i64) -> Result<Vec<Version>> {
        Ok(sqlx::query_as(
            "SELECT id, document_id, kind, name, created_by, created_at, length(data)::bigint AS size \
             FROM rookery.document_versions WHERE document_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2",
        )
        .bind(id)
        .bind(limit.clamp(1, 1000))
        .fetch_all(self.store.pool())
        .await?)
    }

    /// A version and its stored state.
    pub async fn version(&self, id: Uuid, version: Uuid) -> Result<(Version, Vec<u8>)> {
        #[derive(sqlx::FromRow)]
        struct Row {
            #[sqlx(flatten)]
            v: Version,
            data: Vec<u8>,
        }
        let r: Option<Row> = sqlx::query_as(
            "SELECT id, document_id, kind, name, created_by, created_at, length(data)::bigint AS size, data \
             FROM rookery.document_versions WHERE document_id = $1 AND id = $2",
        )
        .bind(id)
        .bind(version)
        .fetch_optional(self.store.pool())
        .await?;
        let r = r.ok_or_else(|| Error::not_found("no such version"))?;
        Ok((r.v, r.data))
    }

    /// When the newest version was saved.
    pub async fn last_version_at(&self, id: Uuid) -> Result<Option<DateTime<Utc>>> {
        Ok(sqlx::query_scalar(
            "SELECT max(created_at) FROM rookery.document_versions WHERE document_id = $1",
        )
        .bind(id)
        .fetch_one(self.store.pool())
        .await?)
    }
}

async fn fetch(conn: &mut PgConnection, id: Uuid) -> Result<Option<Document>> {
    let sql = format!(
        "SELECT {DOC_COLUMNS} FROM rookery.documents d \
         LEFT JOIN rookery.entities e ON e.scheme = 'document' AND e.id = d.id::text WHERE d.id = $1"
    );
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(id)
        .fetch_optional(conn)
        .await?)
}
