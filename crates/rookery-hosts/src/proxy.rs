//! A host's ports and desktop in a browser.
//!
//! **Ports are served on an origin of their own**, never on the server's. Each port of each host is
//! `SCHEME://PORT-HOSTID.BASE` under `ROOKERY_PORTS_URL` (`SCHEME://BASE`), so a page a host serves
//! is another origin than Rookery's: its scripts cannot read Rookery's pages, and Rookery's `/ws` and
//! `/rpc` take the device session's cookie only from Rookery's own origin. The port's origin has a
//! cookie of its own, `rookery_port`, which reaches only that port of that host for that person:
//!
//! 1. Rookery's `/env/NAME/port/PORT/PATH`, or `env.portUrl`, checks `use` on `env://NAME` and makes a
//!    ticket that works once, within a minute, for that person, host and port.
//! 2. The browser takes it to `PORT-HOSTID.BASE/.rookery/claim`, which sets `rookery_port` there and
//!    goes on to `PATH`.
//! 3. Every request and every WebSocket on the port's origin is checked again: the person is not
//!    disabled and still holds `use` on the host. Neither cookie, nor a `Bearer rookery_` token, is
//!    passed on to the port, and cookies the port sets lose their `Domain`.
//!
//! **The desktop** is `/env/NAME/desktop` on Rookery's origin: a WebSocket of RFB bytes for noVNC, for
//! a page of Rookery's own origin with `use` on the host. The server answers the VNC password
//! itself, so it never reaches the browser.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get};
use futures_util::{SinkExt, StreamExt};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rookery_api::client::Frame;
use rookery_core::host_type::Capability;
use rookery_kernel::{Call, Caller, Grant, Kernel, RpcError};
use rookery_proto::env::PortUrl;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use uuid::Uuid;

use crate::serve::{Remote, api};

/// The port's own cookie.
pub const PORT_COOKIE: &str = "rookery_port";
/// The path on a port's origin that takes a ticket.
const CLAIM: &str = "/.rookery/claim";
const TICKET_LIFE: Duration = Duration::from_secs(60);
const PORT_SESSION_LIFE: Duration = Duration::from_secs(12 * 3600);
/// The VNC server of a workstation, on its loopback.
const VNC_PORT: u16 = 5901;

/// Where ports are served: `ROOKERY_PORTS_URL`, which the server sets.
#[derive(Debug, Clone)]
pub struct Ports {
    /// `http` or `https`.
    pub scheme: String,
    /// The host and port each port's own name is put before.
    pub base: String,
}

impl Ports {
    /// From `SCHEME://BASE`, such as `https://ports.rookery.example`.
    pub fn parse(url: &str) -> Result<Ports, String> {
        let (scheme, base) = url
            .trim_end_matches('/')
            .split_once("://")
            .ok_or_else(|| format!("{url} is no URL"))?;
        if !matches!(scheme, "http" | "https") || base.is_empty() || base.contains('/') {
            return Err(format!("{url}: http or https, and a host with no path"));
        }
        Ok(Ports {
            scheme: scheme.into(),
            base: base.to_ascii_lowercase(),
        })
    }

    /// The ports of a server reached on the loopback: under `localhost`, which browsers resolve
    /// with every name before it. Elsewhere it takes `ROOKERY_PORTS_URL`.
    pub fn for_public_url(public: &str) -> Option<Ports> {
        let (scheme, rest) = public.split_once("://")?;
        let authority = rest.split('/').next()?;
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') || host.starts_with('[') => {
                (host, Some(port))
            }
            _ => (authority, None),
        };
        let local = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
        local.then(|| Ports {
            scheme: scheme.into(),
            base: match port {
                Some(port) => format!("localhost:{port}"),
                None => "localhost".into(),
            },
        })
    }

    pub fn origin(&self, host_id: &str, port: u16) -> String {
        format!("{}://{port}-{host_id}.{}", self.scheme, self.base)
    }

    /// The host's id and the port a `Host` header names, when it is one of the ports' origins.
    pub fn parse_host(&self, host: &str) -> Option<(String, u16)> {
        let host = host.to_ascii_lowercase();
        let label = host.strip_suffix(&format!(".{}", self.base))?;
        let (port, id) = label.split_once('-')?;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some((id.to_owned(), port.parse().ok().filter(|p| *p > 0)?))
    }
}

/// The ports' origin of a kernel, if it has one.
pub fn ports(kernel: &Kernel) -> Option<Arc<Ports>> {
    kernel.get_facility::<Ports>().or_else(|| {
        let derived = Ports::for_public_url(&kernel.config.public_url)?;
        kernel.set_facility(derived);
        kernel.get_facility::<Ports>()
    })
}

/// Whether a request is for one of the ports' origins.
pub fn is_port_request(kernel: &Kernel, headers: &HeaderMap) -> bool {
    let Some(ports) = ports(kernel) else {
        return false;
    };
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| ports.parse_host(host).is_some())
}

// ── Tickets and the ports' own sessions ─────────────────────────────────────

#[derive(Clone)]
struct Grantee {
    user: Uuid,
    host_id: String,
    port: u16,
    until: Instant,
}

#[derive(Default)]
struct Passes {
    tickets: Mutex<HashMap<String, Grantee>>,
    sessions: Mutex<HashMap<String, Grantee>>,
}

fn token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn take(map: &Mutex<HashMap<String, Grantee>>, key: &str, once: bool) -> Option<Grantee> {
    let mut map = map.lock().unwrap();
    let now = Instant::now();
    map.retain(|_, g| g.until > now);
    match once {
        true => map.remove(key),
        false => map.get(key).cloned(),
    }
}

fn host_id(kernel: &Kernel, name: &str) -> Result<String, RpcError> {
    let state = crate::hosts(kernel).serve()?;
    let remote = state
        .host(name)
        .ok_or_else(|| RpcError::not_found(format!("no host {name}")))?;
    if remote.id.is_empty() {
        return Err(RpcError::not_found(format!("{name} is not a saved host")));
    }
    Ok(remote.id.clone())
}

/// An address that signs a person in to one port of a host and shows `path` there.
pub fn port_url(
    kernel: &Kernel,
    user: Uuid,
    name: &str,
    port: u16,
    path: &str,
) -> Result<PortUrl, RpcError> {
    if port == 0 {
        return Err(RpcError::invalid("port: from 1 to 65535"));
    }
    let ports = ports(kernel).ok_or_else(|| {
        RpcError::unavailable(
            "ports have no origin of their own: set ROOKERY_PORTS_URL, such as https://ports.rookery.example, with a wildcard name for it",
        )
    })?;
    let host_id = host_id(kernel, name)?;
    let ticket = token();
    kernel.facility::<Passes>().tickets.lock().unwrap().insert(
        ticket.clone(),
        Grantee {
            user,
            host_id: host_id.clone(),
            port,
            until: Instant::now() + TICKET_LIFE,
        },
    );
    let path = safe_path(path);
    let origin = ports.origin(&host_id, port);
    Ok(PortUrl {
        url: format!(
            "{origin}{CLAIM}?ticket={ticket}&path={}",
            rookery_api::client::encode(&path)
        ),
        origin,
    })
}

/// A path on the port's own origin: normalized as a browser would, absolute, and never another origin's.
fn safe_path(path: &str) -> String {
    let cleaned: String = path
        .trim_matches(|c: char| c <= ' ' || c == '\x7f')
        .chars()
        .filter(|c| !c.is_ascii_control())
        .collect();
    let split = cleaned.find(['?', '#']).unwrap_or(cleaned.len());
    let (head, tail) = cleaned.split_at(split);
    let normalized_head = head.replace('\\', "/");
    let trimmed_head = normalized_head.trim_start_matches('/');
    format!("/{trimmed_head}{tail}")
}

// ── Routes ──────────────────────────────────────────────────────────────────

/// The routes on Rookery's own origin.
pub fn routes() -> Router<Kernel> {
    Router::new()
        .route("/env/{name}/port/{port}", any(to_port_root))
        .route("/env/{name}/port/{port}/", any(to_port_root))
        .route("/env/{name}/port/{port}/{*rest}", any(to_port))
        .route("/env/{name}/desktop", get(desktop))
}

/// The routes of the ports' origins: everything there.
pub fn port_routes() -> Router<Kernel> {
    Router::new().fallback(on_port)
}

fn refusal(error: RpcError) -> Response {
    use rookery_proto::ErrorCode::*;
    let status = match error.error_code() {
        Unauthenticated => StatusCode::UNAUTHORIZED,
        Forbidden => StatusCode::FORBIDDEN,
        NotFound => StatusCode::NOT_FOUND,
        Invalid => StatusCode::BAD_REQUEST,
        Timeout => StatusCode::GATEWAY_TIMEOUT,
        Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    };
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        error.message.clone(),
    )
        .into_response()
}

fn origin_of(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if (!scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https"))
        || authority.is_empty()
        || authority.contains('@')
        || authority.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some((scheme, authority))
}

/// Whether a request comes from a page of Rookery's own, or from no page at all.
/// A present but malformed or cross-scheme `Origin` is always refused.
fn same_origin(kernel: &Kernel, headers: &HeaderMap) -> bool {
    same_origin_for(&kernel.config.public_url, headers)
}

fn same_origin_for(public_url: &str, headers: &HeaderMap) -> bool {
    let Some(raw) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = raw.to_str() else {
        return false;
    };
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if (!scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https"))
        || rest.is_empty()
        || rest.contains(['/', '?', '#', '@'])
        || rest.chars().any(char::is_whitespace)
    {
        return false;
    }
    if let Some((pub_scheme, pub_auth)) = origin_of(public_url)
        && scheme.eq_ignore_ascii_case(pub_scheme)
        && rest.eq_ignore_ascii_case(pub_auth)
    {
        return true;
    }
    let expected_scheme = origin_of(public_url).map(|(s, _)| s).unwrap_or("http");
    if !scheme.eq_ignore_ascii_case(expected_scheme) {
        return false;
    }
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| !host.is_empty() && rest.eq_ignore_ascii_case(host))
}

/// The signed-in caller of a request on Rookery's origin.
async fn caller_of(kernel: &Kernel, headers: &HeaderMap) -> Option<Caller> {
    use rookery_kernel::auth::{authenticate, bearer, cookie};
    let token = bearer(headers).or_else(|| {
        same_origin(kernel, headers)
            .then(|| cookie(headers))
            .flatten()
    })?;
    authenticate(kernel, &token).await
}

/// A caller with `use` on a host, or the response that refuses them.
async fn with_use(
    kernel: &Kernel,
    caller: Option<Caller>,
    name: &str,
) -> Result<Call, Box<Response>> {
    let Some(caller) = caller else {
        return Err(Box::new(
            (StatusCode::UNAUTHORIZED, "sign in to Rookery first").into_response(),
        ));
    };
    let call = Call::new(kernel.clone(), caller, None);
    let uri = crate::uri(name);
    match rookery_facilities::access::holds(&call, &uri, Grant::Use).await {
        Ok(true) => Ok(call),
        Ok(false) => Err(Box::new(refusal(RpcError::forbidden(format!(
            "this needs use on {uri}"
        ))))),
        Err(e) => Err(Box::new(refusal(e))),
    }
}

async fn to_port_root(
    State(kernel): State<Kernel>,
    Path((name, port)): Path<(String, u16)>,
    request: Request,
) -> Response {
    to(kernel, name, port, String::new(), request).await
}

async fn to_port(
    State(kernel): State<Kernel>,
    Path((name, port, rest)): Path<(String, u16, String)>,
    request: Request,
) -> Response {
    to(kernel, name, port, rest, request).await
}

/// Rookery's address of a port sends the browser on to the port's own origin with a ticket.
async fn to(kernel: Kernel, name: String, port: u16, rest: String, request: Request) -> Response {
    let caller = caller_of(&kernel, request.headers()).await;
    let call = match with_use(&kernel, caller, &name).await {
        Ok(call) => call,
        Err(refused) => return *refused,
    };
    let Ok(user) = call.user_id() else {
        return refusal(RpcError::forbidden("a port is opened by a person"));
    };
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    match port_url(&kernel, user, &name, port, &format!("/{rest}{query}")) {
        Ok(found) => {
            let mut response = Redirect::to(&found.url).into_response();
            response.headers_mut().insert(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(e) => refusal(e),
    }
}

fn port_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == PORT_COOKIE)
        .map(|(_, value)| value.to_owned())
}

fn query_param(uri: &Uri, key: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Everything on a port's origin: the ticket's claim, or the port itself for a person signed in to it.
async fn on_port(State(kernel): State<Kernel>, request: Request) -> Response {
    let Some(ports) = ports(&kernel) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((host_id, port)) = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| ports.parse_host(h))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let passes = kernel.facility::<Passes>();
    if request.uri().path() == CLAIM {
        let ticket = query_param(request.uri(), "ticket").unwrap_or_default();
        let Some(grantee) =
            take(&passes.tickets, &ticket, true).filter(|g| g.host_id == host_id && g.port == port)
        else {
            return (
                StatusCode::FORBIDDEN,
                "this link has been used or is too old: open the port from Rookery again",
            )
                .into_response();
        };
        let session = token();
        passes.sessions.lock().unwrap().insert(
            session.clone(),
            Grantee {
                until: Instant::now() + PORT_SESSION_LIFE,
                ..grantee
            },
        );
        let secure = if ports.scheme == "https" {
            "; Secure"
        } else {
            ""
        };
        let cookie = format!(
            "{PORT_COOKIE}={session}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
            PORT_SESSION_LIFE.as_secs()
        );
        let path = safe_path(&query_param(request.uri(), "path").unwrap_or_else(|| "/".into()));
        let mut response = Redirect::to(&path).into_response();
        let headers = response.headers_mut();
        if let Ok(cookie) = HeaderValue::from_str(&cookie) {
            headers.insert(header::SET_COOKIE, cookie);
        }
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    }
    let Some(grantee) = port_cookie(request.headers())
        .and_then(|session| take(&passes.sessions, &session, false))
        .filter(|g| g.host_id == host_id && g.port == port)
    else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "open this port from Rookery: its address there signs you in to it",
        )
            .into_response();
    };
    // Each request is the person's again: still there, not disabled, still with use on the host.
    let caller = match kernel.store.users().get(grantee.user).await {
        Ok(Some(user)) if !user.is_disabled() => Caller::User {
            user: rookery_kernel::auth::wire_user(&user),
            session: None,
            token: None,
        },
        _ => {
            return refusal(RpcError::forbidden(
                "the person this port was opened for is gone",
            ));
        }
    };
    let remote = match crate::hosts(&kernel).serve().ok().and_then(|state| {
        state
            .hosts
            .read()
            .unwrap()
            .values()
            .find(|r| r.id == host_id)
            .cloned()
    }) {
        Some(remote) => remote,
        None => return refusal(RpcError::not_found("no such host")),
    };
    if let Err(refused) = with_use(&kernel, Some(caller.clone()), &remote.name).await {
        return *refused;
    }
    proxy(&kernel, caller, remote, port, request).await
}

// ── Proxying HTTP over a host's link ────────────────────────────────────────

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The cookie header without Rookery's own cookies.
fn strip_own_cookies(value: &str) -> Option<String> {
    let session = format!("{}=", rookery_kernel::auth::SESSION_COOKIE);
    let port = format!("{PORT_COOKIE}=");
    let kept: Vec<&str> = value
        .split(';')
        .map(str::trim)
        .filter(|kv| !kv.is_empty() && !kv.starts_with(&session) && !kv.starts_with(&port))
        .collect();
    (!kept.is_empty()).then(|| kept.join("; "))
}

/// The headers that go on to the port: not `Host`, nor hop-by-hop ones unless it upgrades, nor
/// Rookery's cookies and tokens.
fn request_headers(source: &HeaderMap, upgrade: bool) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (key, value) in source {
        let name = key.as_str();
        if name == "host" || (!upgrade && HOP_BY_HOP.contains(&name)) {
            continue;
        }
        if name == "cookie" {
            if let Some(kept) = value.to_str().ok().and_then(strip_own_cookies)
                && let Ok(kept) = HeaderValue::from_str(&kept)
            {
                out.append(key, kept);
            }
            continue;
        }
        if name == "authorization"
            && value
                .to_str()
                .ok()
                .and_then(rookery_kernel::auth::bearer_token)
                .is_some_and(|t| t.starts_with("rookery_"))
        {
            continue;
        }
        out.append(key, value.clone());
    }
    out
}

/// A `Location` to the port's own address, as the port's origin has it.
fn rewrite_location(location: &str, port: u16) -> Option<String> {
    for host in [format!("localhost:{port}"), format!("127.0.0.1:{port}")] {
        for scheme in ["http://", "https://"] {
            if let Some(rest) = location.strip_prefix(&format!("{scheme}{host}"))
                && (rest.is_empty() || rest.starts_with(['/', '?', '#']))
            {
                return Some(if rest.is_empty() {
                    "/".into()
                } else {
                    rest.into()
                });
            }
        }
    }
    None
}

/// A `Set-Cookie` of the port's without a `Domain`, so it stays on the port's origin; `None` for
/// one that would take a name of Rookery's.
fn rewrite_cookie(value: &str) -> Option<String> {
    let mut parts = value.split(';');
    let first = parts.next()?.trim();
    let name = first.split('=').next().unwrap_or("").trim();
    if name == rookery_kernel::auth::SESSION_COOKIE || name == PORT_COOKIE {
        return None;
    }
    let mut kept = vec![first.to_owned()];
    for part in parts {
        let part = part.trim();
        if part.to_ascii_lowercase().starts_with("domain=") {
            continue;
        }
        kept.push(part.to_owned());
    }
    Some(kept.join("; "))
}

fn filter_set_cookies(headers: &mut HeaderMap) {
    let cookies: Vec<String> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(rewrite_cookie)
        .collect();
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            headers.append(header::SET_COOKIE, value);
        }
    }
}

fn response_headers(headers: &mut HeaderMap, port: u16) {
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
    if let Some(found) = headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|l| rewrite_location(l, port))
        && let Ok(value) = HeaderValue::from_str(&found)
    {
        headers.insert(header::LOCATION, value);
    }
    filter_set_cookies(headers);
}

/// A host's stream to a port as bytes both ways.
pub struct PortConn(DuplexStream);

impl PortConn {
    pub fn new(stream: rookery_core::link::LinkStream) -> PortConn {
        let (ours, theirs) = tokio::io::duplex(256 * 1024);
        let (mut from_us, mut to_us) = tokio::io::split(theirs);
        let (mut sink, mut source) = stream.split();
        tokio::spawn(async move {
            while let Some(Ok(frame)) = source.next().await {
                match frame {
                    Frame::Binary(bytes) => {
                        if to_us.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                    Frame::Text(text) if text.as_str().contains("\"eof\"") => break,
                    Frame::Close(_) => break,
                    _ => {}
                }
            }
            let _ = to_us.shutdown().await;
        });
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match from_us.read(&mut buffer).await {
                    Ok(0) | Err(_) => {
                        let _ = sink
                            .send(Frame::Text(rookery_api::streams::EOF.into()))
                            .await;
                        break;
                    }
                    Ok(n) => {
                        if sink
                            .send(Frame::Binary(buffer[..n].to_vec().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        PortConn(ours)
    }
}

impl AsyncRead for PortConn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for PortConn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl Connection for PortConn {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

/// Connects to a port of one host, whatever the URI's host says.
#[derive(Clone)]
struct HostConnector(Arc<Remote>);

impl tower_service::Service<Uri> for HostConnector {
    type Response = TokioIo<PortConn>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let remote = self.0.clone();
        Box::pin(async move {
            let port = uri.port_u16().unwrap_or(80);
            let link = remote
                .link
                .as_ref()
                .ok_or_else(|| std::io::Error::other("no link"))?;
            let stream = link
                .stream(&format!("/v1/ports/{port}"))
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(TokioIo::new(PortConn::new(stream)))
        })
    }
}

type PortClient = Client<HostConnector, Body>;

/// A few connections to each port, kept between a page's requests.
#[derive(Default)]
struct Clients(Mutex<HashMap<String, PortClient>>);

fn client(kernel: &Kernel, remote: &Arc<Remote>) -> PortClient {
    let clients = kernel.facility::<Clients>();
    let mut all = clients.0.lock().unwrap();
    // A host that was taken up again has a new link.
    let key = format!("{}:{:p}", remote.id, Arc::as_ptr(remote));
    all.retain(|k, _| !k.starts_with(&format!("{}:", remote.id)) || *k == key);
    all.entry(key)
        .or_insert_with(|| {
            Client::builder(TokioExecutor::new())
                .pool_idle_timeout(Duration::from_secs(30))
                .pool_max_idle_per_host(4)
                .build(HostConnector(remote.clone()))
        })
        .clone()
}

fn switch_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = headers.clone();
    filter_set_cookies(&mut out);
    out
}

async fn proxy(
    kernel: &Kernel,
    caller: Caller,
    remote: Arc<Remote>,
    port: u16,
    mut request: Request,
) -> Response {
    let host_type_ok = match crate::hosts(kernel).serve() {
        Ok(state) => api::controller(&state, &remote.name, Capability::Sessions)
            .await
            .is_ok(),
        Err(_) => false,
    };
    if !host_type_ok {
        return refusal(RpcError::unavailable(format!(
            "{} does not reach its ports now",
            remote.name
        )));
    }
    let path = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".into());
    let target: Uri = match format!("http://127.0.0.1:{port}{path}").parse() {
        Ok(target) => target,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let upgrade = request.headers().contains_key(header::UPGRADE);
    let mut out = hyper::Request::builder()
        .method(request.method().clone())
        .uri(target);
    let original_host = request.headers().get(header::HOST).cloned();
    for (key, value) in &request_headers(request.headers(), upgrade) {
        out = out.header(key, value);
    }
    out = out.header(header::HOST, format!("localhost:{port}"));
    if let Some(host) = original_host {
        out = out.header("x-forwarded-host", host);
    }
    let scheme = ports(kernel)
        .map(|p| p.scheme.clone())
        .unwrap_or_else(|| "http".into());
    out = out.header("x-forwarded-proto", scheme);
    let on_upgrade = upgrade.then(|| hyper::upgrade::on(&mut request));
    let body = std::mem::take(request.body_mut());
    let out = match out.body(body) {
        Ok(out) => out,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let mut access =
        crate::AccessWatch::new(kernel.clone(), caller, crate::uri(&remote.name), Grant::Use)
            .subscribe();
    let mut answer = match client(kernel, &remote).request(out).await {
        Ok(answer) => answer,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("nothing answered on port {port} of {}: {e}", remote.name),
            )
                .into_response();
        }
    };
    if answer.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(incoming) = on_upgrade
    {
        let headers = switch_headers(answer.headers());
        let outgoing = hyper::upgrade::on(&mut answer);
        tokio::spawn(async move {
            let pair = tokio::select! {
                () = access.wait_revoked() => return,
                pair = async { tokio::join!(incoming, outgoing) } => pair,
            };
            let (Ok(a), Ok(b)) = pair else {
                return;
            };
            let mut a = TokioIo::new(a);
            let mut b = TokioIo::new(b);
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
                () = access.wait_revoked() => {
                    let _ = tokio::join!(a.shutdown(), b.shutdown());
                }
            }
        });
        let mut switched = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
        for (key, value) in &headers {
            switched = switched.header(key, value);
        }
        return switched
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    }
    let (mut parts, body) = answer.into_parts();
    response_headers(&mut parts.headers, port);
    Response::from_parts(parts, Body::new(body))
}

// ── The desktop ─────────────────────────────────────────────────────────────

async fn desktop(
    State(kernel): State<Kernel>,
    Path(name): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !same_origin(&kernel, &headers) {
        return (
            StatusCode::FORBIDDEN,
            "a desktop is opened from Rookery's own pages",
        )
            .into_response();
    }
    let caller = caller_of(&kernel, &headers).await;
    let call = match with_use(&kernel, caller, &name).await {
        Ok(call) => call,
        Err(refused) => return *refused,
    };
    let remote = match crate::hosts(&kernel).serve() {
        Ok(state) => match api::controller(&state, &name, Capability::Desktop).await {
            Ok(remote) => remote,
            Err(e) => return refusal(e.into()),
        },
        Err(e) => return refusal(e),
    };
    let access = crate::access_watch(&call, crate::uri(&name), Grant::Use).subscribe();
    call.audit(
        "env.desktop",
        Some(&crate::uri(&name)),
        serde_json::json!({}),
    )
    .await;
    upgrade
        .protocols(["binary"])
        .on_upgrade(move |socket| async move {
            if let Err(e) = serve_desktop(&kernel, remote, socket, access).await {
                tracing::debug!(error = %e.message, "a desktop connection ended");
            }
        })
}

/// Where a workstation keeps its VNC password, as its set-up writes it: in the agent's home.
async fn password_paths(kernel: &Kernel, remote: &Remote) -> Vec<String> {
    let agent = match crate::hosts(kernel).serve() {
        Ok(state) => {
            let (paths, name) = (state.paths.clone(), remote.name.clone());
            tokio::task::spawn_blocking(move || {
                rookery_core::host::Host::load(&paths, &name)
                    .map(|host| host.agent().to_owned())
                    .ok()
            })
            .await
            .ok()
            .flatten()
        }
        Err(_) => None,
    };
    let agent = agent
        .filter(|a| {
            !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        })
        .unwrap_or_else(|| "agent".into());
    vec![format!("/home/{agent}/.config/tigervnc/password")]
}

async fn vnc_password(kernel: &Kernel, remote: &Remote) -> Option<String> {
    let link = remote.link.as_ref()?;
    for path in password_paths(kernel, remote).await {
        if let Ok(Some(bytes)) = crate::env::read_bytes(link, &path).await {
            let text = String::from_utf8_lossy(&bytes)
                .trim_end_matches(['\n', '\r'])
                .to_owned();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

async fn serve_desktop(
    kernel: &Kernel,
    remote: Arc<Remote>,
    socket: WebSocket,
    mut access: crate::AccessWatcher,
) -> Result<(), RpcError> {
    let handshake = async {
        let password = vnc_password(kernel, &remote).await;
        let link = remote
            .link
            .as_ref()
            .ok_or_else(|| RpcError::unavailable("no link"))?;
        let stream = link
            .stream(&format!("/v1/ports/{VNC_PORT}"))
            .await
            .map_err(|e| RpcError::from(api::Fail::from(e)))?;
        let mut server = crate::vnc::Peer::new(stream);
        server.handshake(password.as_deref()).await?;
        Ok::<_, RpcError>(server)
    };
    let server = tokio::select! {
        () = access.wait_revoked() => return Err(RpcError::forbidden("access revoked")),
        res = handshake => res?,
    };
    let (mut sink, mut source) = socket.split();
    let lost = |e: axum::Error| RpcError::unavailable(e.to_string());
    // Towards noVNC: a server that needs no password.
    let mut viewer = crate::vnc::Viewer::default();
    let viewer_handshake = async {
        sink.send(WsMessage::Binary(b"RFB 003.008\n".to_vec().into()))
            .await
            .map_err(lost)?;
        viewer.read_exact(&mut source, 12).await?;
        sink.send(WsMessage::Binary(vec![1u8, 1].into()))
            .await
            .map_err(lost)?;
        viewer.read_exact(&mut source, 1).await?;
        sink.send(WsMessage::Binary(vec![0u8, 0, 0, 0].into()))
            .await
            .map_err(lost)?;
        Ok::<_, RpcError>(())
    };
    tokio::select! {
        () = access.wait_revoked() => {
            let _ = sink.close().await;
            return Err(RpcError::forbidden("access revoked"));
        }
        res = viewer_handshake => res?,
    };
    // From here the bytes pass unchanged.
    let (pending, mut to_host, mut from_host) = server.into_parts();
    let leftover = viewer.take();
    if !leftover.is_empty() {
        to_host
            .send(Frame::Binary(leftover.into()))
            .await
            .map_err(|e| RpcError::unavailable(e.to_string()))?;
    }
    if !pending.is_empty() {
        sink.send(WsMessage::Binary(pending.into()))
            .await
            .map_err(lost)?;
    }
    let up = async {
        while let Some(Ok(message)) = source.next().await {
            match message {
                WsMessage::Binary(bytes) => {
                    if to_host
                        .send(Frame::Binary(bytes.to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                WsMessage::Close(_) => break,
                _ => {}
            }
        }
        let _ = to_host
            .send(Frame::Text(rookery_api::streams::EOF.into()))
            .await;
    };
    let down = async {
        while let Some(Ok(frame)) = from_host.next().await {
            match frame {
                Frame::Binary(bytes) => {
                    if sink
                        .send(WsMessage::Binary(bytes.to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Frame::Text(text) if text.as_str().contains("\"eof\"") => break,
                Frame::Close(_) => break,
                _ => {}
            }
        }
    };
    tokio::select! {
        () = up => {}
        () = down => {}
        () = access.wait_revoked() => {
            let _ = to_host
                .send(Frame::Text(rookery_api::streams::EOF.into()))
                .await;
        }
    }
    let _ = sink.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_have_origins_of_their_own() {
        let ports = Ports::parse("https://ports.rookery.example").unwrap();
        let id = "0190f0a0b1c27d3e8f9a0b1c2d3e4f50";
        assert_eq!(
            ports.origin(id, 3000),
            format!("https://3000-{id}.ports.rookery.example")
        );
        assert_eq!(
            ports.parse_host(&format!("3000-{id}.ports.rookery.example")),
            Some((id.to_owned(), 3000))
        );
        assert_eq!(ports.parse_host("ports.rookery.example"), None);
        assert_eq!(ports.parse_host("rookery.example"), None);
        assert_eq!(
            ports.parse_host(&format!("0-{id}.ports.rookery.example")),
            None
        );
        assert_eq!(ports.parse_host("3000-studio.ports.rookery.example"), None);
        assert!(Ports::parse("ports.example").is_err());
        assert!(Ports::parse("https://ports.example/x").is_err());
    }

    #[test]
    fn a_server_on_the_loopback_serves_ports_under_localhost() {
        let ports = Ports::for_public_url("http://127.0.0.1:7777").unwrap();
        assert_eq!(ports.origin("ab", 8080), "http://8080-ab.localhost:7777");
        assert!(Ports::for_public_url("https://rookery.example").is_none());
    }

    #[test]
    fn the_port_never_gets_rookerys_credentials() {
        let mut h = HeaderMap::new();
        h.insert("host", HeaderValue::from_static("3000-ab.localhost:7777"));
        h.insert(
            "cookie",
            HeaderValue::from_static("rookery_session=rookery_ses_x; rookery_port=abc; theme=dark"),
        );
        h.insert(
            "authorization",
            HeaderValue::from_static("  bearer   rookery_pat_x "),
        );
        h.insert("connection", HeaderValue::from_static("upgrade"));
        h.insert("upgrade", HeaderValue::from_static("websocket"));
        let out = request_headers(&h, false);
        assert_eq!(out.get("cookie").unwrap(), "theme=dark");
        assert!(out.get("host").is_none() && out.get("authorization").is_none());
        assert!(out.get("upgrade").is_none());
        assert_eq!(
            request_headers(&h, true).get("upgrade").unwrap(),
            "websocket"
        );
        h.insert(
            "authorization",
            HeaderValue::from_static("Basic dXNlcjpwdw=="),
        );
        assert!(request_headers(&h, false).get("authorization").is_some());
    }

    #[test]
    fn the_ports_cookies_stay_on_its_origin() {
        assert_eq!(
            rewrite_cookie("sid=1; Domain=localhost; Path=/; HttpOnly").as_deref(),
            Some("sid=1; Path=/; HttpOnly")
        );
        assert_eq!(rewrite_cookie("rookery_session=evil; Path=/"), None);
        assert_eq!(rewrite_cookie("rookery_port=evil"), None);
        assert_eq!(
            rewrite_location("http://localhost:3000/a?b", 3000).as_deref(),
            Some("/a?b")
        );
        assert_eq!(
            rewrite_location("http://127.0.0.1:3000", 3000).as_deref(),
            Some("/")
        );
        assert_eq!(rewrite_location("https://example.com/x", 3000), None);

        let mut ws_headers = HeaderMap::new();
        ws_headers.append(
            header::SET_COOKIE,
            HeaderValue::from_static("rookery_session=forged; Path=/"),
        );
        ws_headers.append(
            header::SET_COOKIE,
            HeaderValue::from_static("app=1; Domain=localhost; Path=/"),
        );
        let filtered = switch_headers(&ws_headers);
        let cookies: Vec<_> = filtered
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(cookies, vec!["app=1; Path=/"]);
    }

    #[test]
    fn a_claim_goes_on_only_to_a_path_of_the_port() {
        assert_eq!(safe_path("/a?b"), "/a?b");
        assert_eq!(safe_path("//evil.example/x"), "/evil.example/x");
        assert_eq!(safe_path("https://evil.example"), "/https://evil.example");
        assert_eq!(safe_path("/\\evil.example"), "/evil.example");
        assert_eq!(safe_path("/\t/evil.example?a=\\b"), "/evil.example?a=\\b");
        assert_eq!(safe_path(" /\x01\\\\evil.example"), "/evil.example");
    }

    #[test]
    fn desktop_origin_checks_scheme_and_rejects_malformed() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("rookery.example"));
        // No Origin header (e.g. non-browser CLI client) is allowed.
        assert!(same_origin_for("https://rookery.example", &h));

        // Same scheme + host matches.
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://rookery.example"),
        );
        assert!(same_origin_for("https://rookery.example", &h));

        // Wrong scheme is refused even when authority matches Host / public_url.
        h.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://rookery.example"),
        );
        assert!(!same_origin_for("https://rookery.example", &h));

        // Present but malformed Origin is refused, never treated as absent.
        for bad in [
            "not-a-url",
            "ftp://rookery.example",
            "https://user@rookery.example",
            "https://rookery.example/path",
        ] {
            h.insert(header::ORIGIN, HeaderValue::from_static(bad));
            assert!(!same_origin_for("https://rookery.example", &h), "{bad}");
        }
        h.insert(
            header::ORIGIN,
            HeaderValue::from_bytes(b"https://\xff").unwrap(),
        );
        assert!(!same_origin_for("https://rookery.example", &h));
    }
}
