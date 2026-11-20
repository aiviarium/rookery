//! The edge: what the server answers over TCP and on the Unix socket.
//!
//! `/healthz` is answered always: 200 when this server leads, 503 while it is a standby. Everything else
//! goes to the kernel's routes while it leads, and is 503 before and after.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::extract::{
    DefaultBodyLimit, Extension, Path as UrlPath, Query, Request, State, WebSocketUpgrade,
};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use rookery_kernel::auth::{authenticate, bearer, cookie};
use rookery_kernel::{Caller, Kernel};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// How a request reached the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Tcp,
    /// The Unix socket: the server's own machine, an admin with no token.
    Socket,
}

/// The kernel of a server that leads, and its routes.
struct Leading {
    kernel: Kernel,
    tcp: Router,
    socket: Router,
    /// The hosts' ports, each on an origin of its own.
    ports: Router,
}

pub struct Edge {
    leading: RwLock<Option<Arc<Leading>>>,
    shell: Option<PathBuf>,
}

impl Edge {
    pub fn new(shell: Option<PathBuf>) -> Edge {
        Edge {
            leading: RwLock::new(None),
            shell,
        }
    }

    /// The kernel while the server leads.
    pub fn kernel(&self) -> Option<Kernel> {
        self.leading().map(|l| l.kernel.clone())
    }

    fn leading(&self) -> Option<Arc<Leading>> {
        self.leading.read().unwrap().clone()
    }

    /// The server leads: from now on requests reach the kernel.
    pub fn set_kernel(&self, kernel: Kernel) {
        let routes = kernel_routes(self.shell.clone());
        let tcp = routes
            .clone()
            .layer(Extension(Via::Tcp))
            .with_state(kernel.clone());
        let socket = routes
            .layer(Extension(Via::Socket))
            .with_state(kernel.clone());
        let ports = rookery_hosts::proxy::port_routes().with_state(kernel.clone());
        *self.leading.write().unwrap() = Some(Arc::new(Leading {
            kernel,
            tcp,
            socket,
            ports,
        }));
    }

    /// The server stopped leading: from now on every request is answered 503, and the kernel it
    /// returns is no longer reached.
    pub fn clear_kernel(&self) -> Option<Kernel> {
        self.leading
            .write()
            .unwrap()
            .take()
            .map(|l| l.kernel.clone())
    }
}

// An 8 MiB blob expands to just under 11 MiB in base64, with room for the RPC envelope.
const MAX_RPC_MESSAGE: usize = 12 * 1024 * 1024;

fn kernel_routes(shell: Option<PathBuf>) -> Router<Kernel> {
    let shell = Arc::new(shell);
    Router::new()
        .route("/ws", get(ws))
        .route(
            "/rpc",
            post(crate::rpc::handle).layer(DefaultBodyLimit::max(MAX_RPC_MESSAGE)),
        )
        .route("/blobs/{id}", get(blob))
        .merge(rookery_auth::routes())
        .merge(rookery_plugins::routes())
        .merge(rookery_hosts::proxy::routes())
        .merge(rookery_data::webhooks::routes())
        .fallback(move |State(kernel): State<Kernel>, request: Request| {
            crate::shell::serve(shell.clone(), kernel, request)
        })
}

async fn healthz(State(edge): State<Arc<Edge>>, method: Method) -> Response {
    match edge.leading() {
        Some(_) if method == Method::GET || method == Method::HEAD => {
            (StatusCode::OK, "leader\n").into_response()
        }
        Some(_) => (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET,HEAD")],
        )
            .into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "standby\n").into_response(),
    }
}

fn standby() -> Response {
    let body = serde_json::json!({ "error": rookery_kernel::RpcError::unavailable("this server is a standby; another one leads").body() });
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "2")],
        axum::Json(body),
    )
        .into_response()
}

async fn forward(edge: Arc<Edge>, via: Via, request: Request) -> Response {
    let Some(leading) = edge.leading() else {
        return standby();
    };
    let router = if via == Via::Socket {
        &leading.socket
    } else if rookery_hosts::proxy::is_port_request(&leading.kernel, request.headers()) {
        // A port's own origin is the port's, with none of the server's routes.
        &leading.ports
    } else {
        &leading.tcp
    };
    match router.clone().oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

fn edge_router(edge: Arc<Edge>, via: Via) -> Router {
    Router::new()
        .route("/healthz", any(healthz))
        .fallback(move |State(edge): State<Arc<Edge>>, request: Request| {
            forward(edge, via, request)
        })
        .with_state(edge)
}

/// `GET /blobs/<id>`: a blob's bytes, for whoever holds the signed address the server handed out, or
/// a signed-in person who put it. A signed address is the grant, and lasts only its few minutes.
async fn blob(
    State(kernel): State<Kernel>,
    Extension(via): Extension<Via>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<BlobQuery>,
    headers: HeaderMap,
) -> Response {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0);
    let signed = match (&query.sig, query.expires) {
        (Some(sig), Some(expires)) => {
            rookery_facilities::blobs::verify(&kernel, &id, expires, sig, now)
        }
        _ => false,
    };
    let readable = if signed {
        rookery_facilities::blobs::signed(&kernel, &id).await
    } else {
        let Some(caller) = caller_of(&kernel, via, &headers, true).await else {
            return (
                StatusCode::UNAUTHORIZED,
                "sign in first: a device session or an access token",
            )
                .into_response();
        };
        let call = rookery_kernel::Call::new(kernel.clone(), caller, None);
        rookery_facilities::blobs::readable(&call, &id).await.ok()
    };
    let Some(blob) = readable else {
        return (StatusCode::NOT_FOUND, "no such blob").into_response();
    };
    let Ok(bytes) = kernel.store.blobs().read(&blob).await else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the blob could not be read",
        )
            .into_response();
    };
    // A blob's content type is the uploader's. From the server's own origin, only a type that cannot
    // run script is shown in place; everything else downloads, and every answer is sandboxed.
    let mut response_headers = vec![
        (header::CONTENT_TYPE, blob.content_type.clone()),
        (header::CACHE_CONTROL, "private, max-age=60".to_owned()),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        (
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'".to_owned(),
        ),
        (
            header::HeaderName::from_static("cross-origin-resource-policy"),
            "same-site".to_owned(),
        ),
    ];
    // A blob's content type is the uploader's, and this is the server's own origin: only a type that
    // cannot run script is shown in place, and everything else is an attachment.
    if !inline_type(&blob.content_type) {
        response_headers.push((
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                filename(&blob.name, &blob.id.to_string())
            ),
        ));
    }
    let mut response = bytes.into_response();
    for (name, value) in response_headers {
        if let Ok(value) = axum::http::HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

/// Types a blob may be shown as in place: they cannot run script, and svg is not one of them.
fn inline_type(content_type: &str) -> bool {
    const INLINE: &[&str] = &[
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/avif",
        "application/pdf",
        "text/plain",
    ];
    let base = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    INLINE.contains(&base.as_str()) || base.starts_with("audio/") || base.starts_with("video/")
}

/// A name safe to put in `Content-Disposition`: no quotes, separators or control characters.
fn filename(name: &Option<String>, id: &str) -> String {
    let chosen = name.as_deref().unwrap_or(id);
    let clean: String = chosen
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '\\' | '/' | '\r' | '\n'))
        .take(120)
        .collect();
    if clean.is_empty() {
        "blob".to_owned()
    } else {
        clean
    }
}

#[derive(serde::Deserialize)]
struct BlobQuery {
    expires: Option<i64>,
    sig: Option<String>,
}

/// The caller of a request: the socket's admin, or whoever its token or cookie signs in as. The cookie
/// counts only for a page of this server, so another site cannot use a browser's session.
pub async fn caller_of(
    kernel: &Kernel,
    via: Via,
    headers: &HeaderMap,
    cookie_ok: bool,
) -> Option<Caller> {
    if via == Via::Socket {
        return Some(Caller::Local);
    }
    let token = credential(kernel, headers, cookie_ok)?;
    authenticate(kernel, &token).await
}

fn credential(kernel: &Kernel, headers: &HeaderMap, cookie_ok: bool) -> Option<String> {
    bearer(headers).or_else(|| {
        if cookie_ok && rookery_auth::session::same_origin(kernel, headers) {
            cookie(headers)
        } else {
            None
        }
    })
}

async fn ws(
    State(kernel): State<Kernel>,
    Extension(via): Extension<Via>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let token = if via == Via::Socket {
        None
    } else {
        credential(&kernel, &headers, true)
    };
    let caller = match (via, token.as_deref()) {
        (Via::Socket, _) => Some(Caller::Local),
        (_, Some(token)) => authenticate(&kernel, token).await,
        (_, None) => None,
    };
    let Some(caller) = caller else {
        return (
            StatusCode::UNAUTHORIZED,
            "sign in first: a device session or an access token",
        )
            .into_response();
    };
    upgrade
        .max_message_size(MAX_RPC_MESSAGE)
        .max_frame_size(MAX_RPC_MESSAGE)
        .on_upgrade(move |socket| rookery_kernel::conn::serve(kernel, socket, caller, token))
}

pub async fn serve_tcp(
    edge: Arc<Edge>,
    listener: tokio::net::TcpListener,
    quit: CancellationToken,
) {
    let router = edge_router(edge, Via::Tcp);
    if let Err(e) = axum::serve(listener, router)
        .with_graceful_shutdown(quit.cancelled_owned())
        .await
    {
        tracing::error!(error = %e, "serving over TCP failed");
    }
}

pub async fn serve_socket(
    edge: Arc<Edge>,
    path: PathBuf,
    listener: tokio::net::UnixListener,
    quit: CancellationToken,
) {
    let router = edge_router(edge, Via::Socket);
    if let Err(e) = axum::serve(listener, router)
        .with_graceful_shutdown(quit.cancelled_owned())
        .await
    {
        tracing::error!(error = %e, "serving on the socket failed");
    }
    let _ = std::fs::remove_file(&path);
}

/// Binds the Unix socket, readable by this user alone. A socket left by a server that ended is
/// replaced; one that answers belongs to a server that runs, which is an error.
pub fn bind_socket(path: &Path) -> Result<tokio::net::UnixListener, String> {
    if let Some(dir) = path.parent()
        && !dir.exists()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(format!(
                "another rookery serve listens on {}",
                path.display()
            ));
        }
        std::fs::remove_file(path)
            .map_err(|e| format!("removing the old {}: {e}", path.display()))?;
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|e| format!("listening on {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    Ok(listener)
}
