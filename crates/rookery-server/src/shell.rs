//! Serving the shell: the built files in `ROOKERY_SHELL_DIR`, with `index.html` for any path that names
//! no file, so that the shell's own routes load it. Without a shell, `/` is a plain page that says so.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
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
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The file a request path names inside `root`, or `None` for one that climbs out.
fn inside(root: &Path, path: &str) -> Option<PathBuf> {
    let mut file = root.to_path_buf();
    for component in Path::new(path.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => file.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(file)
}

const NO_SHELL: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>rookery serve</title>
<style>:root{color-scheme:light dark}body{font:15px/1.5 system-ui,sans-serif;max-width:36rem;margin:15vh auto;padding:0 16px}code{font-size:90%}</style></head>
<body><h1>rookery serve</h1><p>This server runs, and no shell is installed on it. A shell, such as Crow, is the page people use in a browser.</p>
<p>Set <code>ROOKERY_SHELL_DIR</code> to the folder of a built shell, or use the <code>rookery</code> command line.</p></body></html>"#;

pub async fn serve(shell: Arc<Option<PathBuf>>, request: Request) -> Response {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = request.uri().path().to_owned();
    let Some(root) = shell.as_ref() else {
        if path == "/" {
            return (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                NO_SHELL,
            )
                .into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(root) = tokio::fs::canonicalize(root).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(mut file) = inside(&root, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if file.is_dir() {
        file.push("index.html");
    }
    let named_a_file = Path::new(&path).extension().is_some();
    if !file.is_file() {
        // A path of the shell's own, such as /hosts/studio, loads the shell; a missing asset is missing.
        if named_a_file {
            return StatusCode::NOT_FOUND.into_response();
        }
        file = root.join("index.html");
    }
    let Ok(resolved) = tokio::fs::canonicalize(&file).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !resolved.starts_with(&root) || !resolved.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(resolved).await {
        Ok(bytes) => {
            let html = file.extension().is_some_and(|e| e == "html");
            // The pages are asked for again each time; the assets they name carry content hashes.
            let cache = if html {
                "no-cache"
            } else {
                "public, max-age=3600"
            };
            (
                [
                    (header::CONTENT_TYPE, content_type(&file)),
                    (header::CACHE_CONTROL, cache),
                ],
                bytes,
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "the shell has no index.html").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside() {
        let root = Path::new("/srv/shell");
        assert_eq!(
            inside(root, "/assets/app.js"),
            Some(PathBuf::from("/srv/shell/assets/app.js"))
        );
        assert_eq!(inside(root, "/../etc/passwd"), None);
        assert_eq!(inside(root, "/"), Some(PathBuf::from("/srv/shell")));
    }
}
