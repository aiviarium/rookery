//! The operations of the API by name. An operation is one thing a caller can
//! do: `sessions.start`, `kv.set`. Most can be called two ways, as an HTTP
//! request or as a frame on the portal, the one WebSocket at `/v1/portal`;
//! this table is what ties the name to both. Operations that stream are
//! subscriptions on the portal (and still a WebSocket of their own at their
//! path). A few carry bytes that do not belong in JSON frames and say so.
//!
//! ## The portal's frames
//!
//! The caller numbers its requests with `id`, an integer below 2^32 that it
//! does not reuse while the request or its stream is open.
//!
//! ```text
//! → {"id": 7, "op": "sessions.start", "args": {"wait": "30s"}, "data": {…}}
//! ← {"id": 7, "result": {…}}            or   ← {"id": 7, "error": {"code": "…", "message": "…", "status": 404}}
//! ```
//!
//! `args` are the operation's parameters (what HTTP has in the path and the
//! query), `data` is its input (what HTTP has as the body). On `rookery serve`
//! a frame with `"host": "NAME"` is an operation of that host.
//!
//! A streaming operation answers `{"id": 7, "result": {"open": true}}` and
//! then sends its frames under the same number, until either side ends it:
//!
//! ```text
//! ← {"sub": 7, "data": {…}}             a text frame of the stream
//! ← binary: 4 bytes (7, big-endian), then the stream's bytes
//! → {"sub": 7, "data": {…}}             and the same two forms towards the stream
//! ↔ {"sub": 7, "end": true}             the last frame; from the server it may carry "error"
//! ```
//!
//! A request to open a stream may add `"window": BYTES`. A server that keeps
//! to it says so in its answer, `{"open": true, "window": BYTES}`, with the
//! window it took. Each side then sends at most that many bytes of binary
//! frames of the stream that the other side has not yet said it took, and
//! says what it took as it goes:
//!
//! ```text
//! ↔ {"sub": 7, "credit": 131072}        so many more bytes may come
//! ```
//!
//! Bytes in a result or as input are `{"base64": "…"}` (a result adds
//! `content_type`), up to [`PORTAL_BYTES`]; larger transfers are HTTP's.

/// The portal's path.
pub const PORTAL: &str = "/v1/portal";

/// The most bytes one operation carries as base64 over the portal.
pub const PORTAL_BYTES: usize = 8 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A request and its answer.
    Call,
    /// Frames from the server until it ends; some take frames back.
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// No input, or JSON.
    Json,
    /// Bytes: `{"base64": …}` on the portal.
    Bytes,
}

/// What a caller has to be allowed for an operation. Each role includes the
/// ones before it.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Look: hosts, sessions and their output, logs, the inbox, the store.
    View,
    /// Use the hosts: run actions and commands, write files, control units.
    Operate,
    /// Set up: profiles, secrets, registries, plugins, and applying them to hosts.
    Admin,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::View, Role::Operate, Role::Admin];

    pub fn name(self) -> &'static str {
        match self {
            Role::View => "view",
            Role::Operate => "operate",
            Role::Admin => "admin",
        }
    }

    pub fn named(name: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|role| role.name() == name)
    }
}

#[derive(Debug)]
pub struct Op {
    /// `group.verb`; the part before the dot groups the reference.
    pub name: &'static str,
    pub kind: Kind,
    pub method: &'static str,
    pub path: &'static str,
    pub body: Body,
    /// Parameters that are headers over HTTP: (argument, header).
    pub headers: &'static [(&'static str, &'static str)],
    /// Why the portal does not carry it, when it does not.
    pub http_only: &'static str,
    /// The least role that may call it on `rookery serve`.
    pub role: Role,
    /// What the caller shows instead of a token with a role, for an
    /// operation that checks a key of its own; empty for the others.
    pub key: &'static str,
    /// Why only `rookery serve` calls it on a host, for itself; empty for the
    /// others. It passes none of these through for a caller, and no portal
    /// carries them.
    pub server_only: &'static str,
}

const fn call(name: &'static str, method: &'static str, path: &'static str) -> Op {
    Op {
        name,
        kind: Kind::Call,
        method,
        path,
        body: Body::Json,
        headers: &[],
        http_only: "",
        // Reading is looking, anything else is using.
        role: match method.as_bytes() {
            b"GET" => Role::View,
            _ => Role::Operate,
        },
        key: "",
        server_only: "",
    }
}

const fn stream(name: &'static str, path: &'static str) -> Op {
    Op {
        name,
        kind: Kind::Stream,
        method: "GET",
        path,
        body: Body::Json,
        headers: &[],
        http_only: "",
        role: Role::Operate,
        key: "",
        server_only: "",
    }
}

const fn role(mut op: Op, role: Role) -> Op {
    op.role = role;
    op
}

const fn admin(op: Op) -> Op {
    role(op, Role::Admin)
}

const fn bytes(mut op: Op) -> Op {
    op.body = Body::Bytes;
    op
}

/// An operation of rookeryd that `rookery serve` calls itself after checks of
/// its own, and that no caller reaches through it.
const fn server_only(mut op: Op, why: &'static str) -> Op {
    op.server_only = why;
    op.http_only = why;
    op
}

/// The operations of rookeryd.
pub static HOST: &[Op] = &[
    call("version", "GET", "/v1/version"),
    call("paths", "GET", "/v1/paths"),
    call("reload", "POST", "/v1/reload"),
    // It holds the accounts' password hashes.
    admin(call("applied.profile", "GET", "/v1/applied/profile")),
    call("sessions.start", "POST", "/v1/sessions"),
    call("sessions.list", "GET", "/v1/sessions"),
    call("sessions.get", "GET", "/v1/sessions/{id}"),
    stream("sessions.attach", "/v1/sessions/{id}"),
    call("sessions.delete", "DELETE", "/v1/sessions/{id}"),
    call("sessions.output", "GET", "/v1/sessions/{id}/output"),
    call("sessions.screen", "GET", "/v1/sessions/{id}/screen"),
    call("sessions.input", "POST", "/v1/sessions/{id}/input"),
    call("sessions.resize", "POST", "/v1/sessions/{id}/resize"),
    call("sessions.signal", "POST", "/v1/sessions/{id}/signal"),
    call("actions.list", "GET", "/v1/actions"),
    call("actions.run", "POST", "/v1/actions/{action}"),
    call(
        "actions.choices",
        "GET",
        "/v1/actions/{action}/choices/{argument}",
    ),
    call("system.stats", "GET", "/v1/stats"),
    call("system.power", "POST", "/v1/power/{verb}"),
    call("desktop.screenshot", "GET", "/v1/screenshot"),
    role(call("files.read", "GET", "/v1/files"), Role::Operate),
    bytes(call("files.write", "PUT", "/v1/files")),
    call("files.delete", "DELETE", "/v1/files"),
    call("units.list", "GET", "/v1/units"),
    call("units.get", "GET", "/v1/units/{unit}"),
    call("units.act", "POST", "/v1/units/{unit}/{verb}"),
    call("logs.read", "GET", "/v1/logs"),
    role(stream("logs.follow", "/v1/logs"), Role::View),
    call("processes.list", "GET", "/v1/processes"),
    call("processes.signal", "POST", "/v1/processes/{pid}/signal"),
    call("channel.publish", "POST", "/v1/channel/{*topic}"),
    call("channel.read", "GET", "/v1/channel"),
    role(stream("channel.listen", "/v1/channel"), Role::View),
    call("services.list", "GET", "/v1/services"),
    call(
        "services.call",
        "POST",
        "/v1/services/{plugin}/{name}/{method}",
    ),
    stream("services.serve", "/v1/services/{plugin}/{name}/serve"),
    stream("ports.connect", "/v1/ports/{port}"),
    server_only(
        stream("agents.connect", "/v1/agents/connect"),
        "rookery serve opens it after checking access to the host",
    ),
    server_only(
        stream("endpoints.connect", "/v1/endpoints/{address}/{port}"),
        "rookery serve dials it itself, only for an endpoint a provisioner reported: open the endpoint with resources.connect",
    ),
];

pub fn find<'a>(table: &'a [Op], name: &str) -> Option<&'a Op> {
    table.iter().find(|op| op.name == name)
}

/// The names in a path: `{id}` and `{*topic}`, the latter taking slashes.
pub fn names(path: &str) -> impl Iterator<Item = (&str, bool)> {
    path.split('/').filter_map(|part| {
        let name = part.strip_prefix('{')?.strip_suffix('}')?;
        Some(match name.strip_prefix('*') {
            Some(rest) => (rest, true),
            None => (name, false),
        })
    })
}

/// The operation a request is, and its path parameters: the way back from
/// `GET /v1/sessions/abc` to `sessions.get` with `id`.
pub fn recognise<'a>(
    table: &'a [Op],
    kind: Kind,
    method: &str,
    path: &str,
) -> Option<(&'a Op, Vec<(&'a str, String)>)> {
    let given: Vec<&str> = path.split('/').collect();
    'ops: for op in table.iter().filter(|op| op.kind == kind) {
        if !op.method.eq_ignore_ascii_case(method) {
            continue;
        }
        let pattern: Vec<&str> = op.path.split('/').collect();
        let mut found = vec![];
        for (index, part) in pattern.iter().enumerate() {
            match part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                Some(name) => match name.strip_prefix('*') {
                    Some(name) if index < given.len() => {
                        found.push((name, crate::escape::decode(&given[index..].join("/"))));
                        return Some((op, found));
                    }
                    Some(_) => continue 'ops,
                    None => match given.get(index) {
                        Some(value) if !value.is_empty() => {
                            found.push((name, crate::escape::decode(value)))
                        }
                        _ => continue 'ops,
                    },
                },
                None if given.get(index) == Some(part) => {}
                None => continue 'ops,
            }
        }
        if pattern.len() == given.len() {
            return Some((op, found));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_requests_are_recognised() {
        for table in [HOST] {
            for (index, op) in table.iter().enumerate() {
                assert!(
                    !table[..index].iter().any(|other| other.name == op.name),
                    "{} twice",
                    op.name
                );
            }
        }
        let (op, args) = recognise(HOST, Kind::Call, "GET", "/v1/sessions/a%20b/output").unwrap();
        assert_eq!(
            (op.name, args),
            ("sessions.output", vec![("id", "a b".to_owned())])
        );
        let (op, args) = recognise(HOST, Kind::Call, "post", "/v1/channel/ask/desk").unwrap();
        assert_eq!(
            (op.name, args),
            ("channel.publish", vec![("topic", "ask/desk".to_owned())])
        );
        assert_eq!(
            recognise(HOST, Kind::Stream, "GET", "/v1/sessions/x")
                .unwrap()
                .0
                .name,
            "sessions.attach"
        );
        assert_eq!(
            recognise(HOST, Kind::Call, "GET", "/v1/sessions")
                .unwrap()
                .0
                .name,
            "sessions.list"
        );
        assert!(recognise(HOST, Kind::Call, "GET", "/v1/nothing").is_none());
        // Looking is for everyone who is let in, set-up only for an admin.
        let role = |table, name| find(table, name).unwrap().role;
        assert_eq!(role(HOST, "sessions.list"), Role::View);
        assert_eq!(role(HOST, "files.read"), Role::Operate);
        assert_eq!(role(HOST, "applied.profile"), Role::Admin);
        assert_eq!(role(HOST, "sessions.attach"), Role::Operate);
        assert_eq!(role(HOST, "ports.connect"), Role::Operate);
        assert!(Role::View < Role::Operate && Role::Operate < Role::Admin);
        assert_eq!(Role::named("operate"), Some(Role::Operate));
        assert_eq!(
            names("/v1/channel/{*topic}").collect::<Vec<_>>(),
            vec![("topic", true)]
        );
    }
}
