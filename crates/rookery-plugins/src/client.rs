//! Client halves, served at an address of their own for each version of their files:
//! `/plugins/<id>/<hash>/<path>`, where `<hash>` is the SHA-256 of the client half's files and `<path>`
//! is relative to the plugin's target folder. A browser keeps them for good. An address with another
//! hash is gone: `plugins.list` gives the current one.

use std::path::Path;

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rookery_kernel::Kernel;

use crate::loaded::inside;

/// The routes the server mounts beside `/ws` and `/rpc`.
pub fn routes() -> Router<Kernel> {
    Router::new().route("/plugins/{id}/{hash}/{*path}", get(serve))
}

async fn serve(
    State(kernel): State<Kernel>,
    UrlPath((id, hash, path)): UrlPath<(String, String, String)>,
) -> Response {
    let Some(plugins) = kernel.get_facility::<crate::Plugins>() else {
        return not_found();
    };
    let Some(plugin) = plugins.get(&id) else {
        return not_found();
    };
    let Some(client) = &plugin.client else {
        return not_found();
    };
    if client.hash != hash || !client.serves(&path) {
        return not_found();
    }
    let Some(file) = inside(&plugin.dir, &path) else {
        return not_found();
    };
    // A link out of the folder is not served, whatever it points at.
    let real = match (
        tokio::fs::canonicalize(&file).await,
        tokio::fs::canonicalize(&plugin.dir).await,
    ) {
        (Ok(real), Ok(dir)) if real.starts_with(&dir) => real,
        _ => return not_found(),
    };
    match tokio::fs::read(&real).await {
        Ok(data) => (
            [
                (header::CONTENT_TYPE, content_type(&real)),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ),
                (
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                ),
            ],
            data,
        )
            .into_response(),
        Err(_) => not_found(),
    }
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        "no such file of a plugin's client half\n",
    )
        .into_response()
}

fn content_type(path: &Path) -> HeaderValue {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    HeaderValue::from_static(match ext.as_str() {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "html" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    })
}
