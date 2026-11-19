//! Blobs: files under `ROOKERY_BLOBS`, named by their SHA-256 (`ab/abcd…`), and a row for each that
//! says what it is and whose. Two blobs with the same bytes share a file.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::Store;
use crate::error::{Error, Result};
use crate::util::new_id;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Blob {
    pub id: Uuid,
    pub sha256: String,
    pub size: i64,
    pub content_type: String,
    pub name: Option<String>,
    pub owner: String,
    pub created_at: DateTime<Utc>,
}

const COLS: &str = "id, sha256, size, content_type, name, owner, created_at";

pub struct Blobs<'a> {
    pub(crate) store: &'a Store,
}

impl Blobs<'_> {
    fn file(&self, sha256: &str) -> Result<PathBuf> {
        Ok(self.store.blob_dir()?.join(&sha256[..2]).join(sha256))
    }

    pub async fn put(
        &self,
        bytes: &[u8],
        content_type: &str,
        name: Option<&str>,
        owner: &str,
    ) -> Result<Blob> {
        let sha256 = hex::encode(Sha256::digest(bytes));
        let file = self.file(&sha256)?;
        if !file.exists() {
            let dir = file.parent().expect("a blob's file is in a folder");
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| Error::Unavailable(format!("the blob folder: {e}")))?;
            // Written aside and moved in place, so a reader never sees half a file.
            let partial = dir.join(format!(".{sha256}.{}", new_id().simple()));
            tokio::fs::write(&partial, bytes)
                .await
                .map_err(|e| Error::Unavailable(format!("writing a blob: {e}")))?;
            tokio::fs::rename(&partial, &file)
                .await
                .map_err(|e| Error::Unavailable(format!("writing a blob: {e}")))?;
        }
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO rookery.blobs (id, sha256, size, content_type, name, owner) VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLS}"
        )))
        .bind(new_id())
        .bind(&sha256)
        .bind(bytes.len() as i64)
        .bind(content_type)
        .bind(name)
        .bind(owner)
        .fetch_one(self.store.pool())
        .await?)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Blob>> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM rookery.blobs WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(self.store.pool())
        .await?)
    }

    /// The bytes of a blob.
    pub async fn read(&self, blob: &Blob) -> Result<Vec<u8>> {
        tokio::fs::read(self.file(&blob.sha256)?)
            .await
            .map_err(|e| Error::Unavailable(format!("reading blob {}: {e}", blob.id)))
    }

    /// Removes a blob, and its file once no other blob has the same bytes.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let gone: Option<String> =
            sqlx::query_scalar("DELETE FROM rookery.blobs WHERE id = $1 RETURNING sha256")
                .bind(id)
                .fetch_optional(self.store.pool())
                .await?;
        let Some(sha256) = gone else { return Ok(false) };
        let others: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM rookery.blobs WHERE sha256 = $1)")
                .bind(&sha256)
                .fetch_one(self.store.pool())
                .await?;
        if !others {
            let _ = tokio::fs::remove_file(self.file(&sha256)?).await;
        }
        Ok(true)
    }

    /// Whether a row of a collection names the blob in a field of type `blob`.
    pub async fn referenced(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM rookery.refs WHERE dst_scheme = 'blob' AND dst_id = $1 AND NOT src_deleted)",
        )
        .bind(id.to_string())
        .fetch_one(self.store.pool())
        .await?)
    }
}
