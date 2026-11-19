//! The method registry.
//!
//! A facility adds a method with [`Methods::add`] and says what a caller needs with
//! [`MethodDef::level`]. The params are read into the handler's type, whose JSON Schema goes into the
//! method report under `api/`. A method without a level is refused to every caller, and a test of
//! `rookery-server` fails for it.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use rookery_proto::methods::MethodInfo;
pub use rookery_proto::methods::{Grant, Level};
use rookery_proto::{Principal, PrincipalKind, RpcError, User};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use crate::conn::ConnHandle;
use crate::kernel::Kernel;

/// Who a call is from. The edge decides it before any method runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// A person, signed in with a device session or an access token.
    User {
        user: User,
        session: Option<Uuid>,
        token: Option<Uuid>,
    },
    /// The server's own machine, over the Unix socket: an admin with no user and no token.
    Local,
    /// Code inside the server, such as a job handler, named by its part.
    System(String),
    /// A plugin acting for itself.
    Plugin(String),
}

impl Caller {
    pub fn principal(&self) -> Principal {
        match self {
            Caller::User { user, .. } => Principal {
                kind: PrincipalKind::User,
                id: user.id.clone(),
            },
            Caller::Local => Principal {
                kind: PrincipalKind::Local,
                id: "socket".into(),
            },
            Caller::System(part) => Principal {
                kind: PrincipalKind::System,
                id: part.clone(),
            },
            Caller::Plugin(plugin) => Principal {
                kind: PrincipalKind::Plugin,
                id: plugin.clone(),
            },
        }
    }

    /// Whether the caller has the org role admin. The server's own machine and its own code do.
    pub fn is_admin(&self) -> bool {
        match self {
            Caller::User { user, .. } => user.admin,
            Caller::Local | Caller::System(_) => true,
            Caller::Plugin(_) => false,
        }
    }

    pub fn user(&self) -> Option<&User> {
        match self {
            Caller::User { user, .. } => Some(user),
            _ => None,
        }
    }
}

/// One call of a method: the kernel, the caller, the connection it came on and its deadline.
#[derive(Clone)]
pub struct Call {
    pub kernel: Kernel,
    pub caller: Caller,
    /// The `/ws` connection or the socket's; `None` for `POST /rpc` and calls from inside the server.
    pub conn: Option<ConnHandle>,
    pub deadline: Instant,
    /// The plugin that makes this call for its caller: a host half's env call runs as a person, and
    /// the audit log names both.
    pub on_behalf: Option<String>,
    /// The call is one a host half made through its `host.env` for an invocation. A plugin's `env.*`
    /// calls are refused unless it is set, so env reaches a host only that way.
    pub host_env: bool,
}

impl Call {
    pub fn new(kernel: Kernel, caller: Caller, conn: Option<ConnHandle>) -> Call {
        let deadline = Instant::now() + kernel.config.call_timeout;
        Call {
            kernel,
            caller,
            conn,
            deadline,
            on_behalf: None,
            host_env: false,
        }
    }

    pub fn store(&self) -> &rookery_store::Store {
        &self.kernel.store
    }

    pub fn principal(&self) -> Principal {
        self.caller.principal()
    }

    pub fn is_admin(&self) -> bool {
        self.caller.is_admin()
    }

    pub fn user(&self) -> Option<&User> {
        self.caller.user()
    }

    /// The person the call is from, for methods about one's own things.
    pub fn require_user(&self) -> Result<&User, RpcError> {
        self.user().ok_or_else(|| {
            RpcError::forbidden("this is about a person's own things, and the caller is none")
        })
    }

    /// The caller's user id.
    pub fn user_id(&self) -> Result<Uuid, RpcError> {
        let user = self.require_user()?;
        Uuid::parse_str(&user.id).map_err(|_| RpcError::internal("a user id is not a UUID"))
    }

    pub fn require_admin(&self) -> Result<(), RpcError> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(RpcError::forbidden("this needs the role admin"))
        }
    }

    /// The connection, for methods that open streams.
    pub fn conn(&self) -> Result<&ConnHandle, RpcError> {
        self.conn
            .as_ref()
            .ok_or_else(|| RpcError::unavailable("this needs a connection on /ws"))
    }

    /// The plugin a call at [`Level::Plugin`] is for: the calling plugin itself, or the one an admin names.
    pub fn plugin(&self, named: Option<&str>) -> Result<String, RpcError> {
        match (&self.caller, named) {
            // A plugin that calls itself rookery would act as the server.
            (Caller::Plugin(own), _) if reserved_plugin(own) => Err(RpcError::forbidden(format!(
                "{own} is Rookery's own, and no plugin acts as it"
            ))),
            (Caller::Plugin(own), None) => Ok(own.clone()),
            (Caller::Plugin(own), Some(named)) if named == own => Ok(own.clone()),
            (Caller::Plugin(_), Some(_)) => {
                Err(RpcError::forbidden("a plugin acts for itself alone"))
            }
            (_, Some(named)) if self.is_admin() => crate::methods::named_plugin(self, named),
            (_, None) if self.is_admin() => Err(RpcError::invalid("name the plugin in plugin")),
            _ => Err(RpcError::forbidden(
                "this is for plugins, and for admins naming one",
            )),
        }
    }

    /// How the audit log names the caller, and the plugin that made the call for them.
    pub fn audit_name(&self) -> String {
        let principal = self.principal().uri();
        match &self.on_behalf {
            Some(plugin) => format!("{principal} via plugin:{plugin}"),
            None => principal,
        }
    }

    /// Adds an entry to the audit log. A failure is logged and does not fail the call.
    pub async fn audit(&self, action: &str, resource: Option<&str>, details: Value) {
        if let Err(e) = self
            .kernel
            .store
            .audit()
            .record(&self.audit_name(), action, resource, details)
            .await
        {
            tracing::warn!(action, error = %e, "writing the audit log failed");
        }
    }
}

/// Whether a plugin id is Rookery's own, which no installed plugin may have: `rookery`, and
/// `rookery.*` or `rookery/*`. The server keeps its secrets, settings and leases under it.
pub fn reserved_plugin(id: &str) -> bool {
    id == "rookery" || id.starts_with("rookery.") || id.starts_with("rookery/")
}

/// A plugin id a package may have: lower-case letters, digits, `-` and `_`, starting with a letter, and
/// none Rookery keeps for itself, as `rookery_api::plugin_ids` says.
pub fn plugin_id(id: &str) -> Result<String, RpcError> {
    if rookery_api::plugin_ids::valid_plugin_id(id) {
        Ok(id.to_owned())
    } else {
        Err(RpcError::invalid(format!("{id:?} is not a plugin id")))
    }
}

/// The plugin a call names: one a package may have, or Rookery's own (see [`reserved_plugin`]),
/// whose settings and secrets an admin alone reaches.
pub fn named_plugin(call: &Call, id: &str) -> Result<String, RpcError> {
    if reserved_plugin(id) {
        call.require_admin()
            .map_err(|_| RpcError::forbidden("the plugin rookery is Rookery's own, for admins"))?;
        return Ok(id.to_owned());
    }
    plugin_id(id)
}

type Handler =
    Arc<dyn Fn(Call, Value) -> BoxFuture<'static, Result<Value, RpcError>> + Send + Sync>;
type StartHook = Arc<dyn Fn(&Kernel) + Send + Sync>;
type ConnHook = Arc<dyn Fn(&Kernel, &ConnHandle) + Send + Sync>;
/// Says whether a caller holds a level on a resource: see [`Methods::set_access`].
pub type AccessCheck =
    Arc<dyn Fn(Call, String, Grant) -> BoxFuture<'static, Result<bool, RpcError>> + Send + Sync>;

/// One method in the registry.
#[derive(Clone)]
pub struct MethodDef {
    name: &'static str,
    summary: &'static str,
    level: Option<Level>,
    stream: bool,
    max_timeout: Option<Duration>,
    params: Value,
    result: Value,
    handler: Handler,
}

impl MethodDef {
    /// What a caller needs.
    pub fn level(&mut self, level: Level) -> &mut Self {
        self.level = Some(level);
        self
    }

    /// One sentence for the report.
    pub fn summary(&mut self, summary: &'static str) -> &mut Self {
        self.summary = summary;
        self
    }

    /// The result names a stream that carries the rest.
    pub fn stream(&mut self) -> &mut Self {
        self.stream = true;
        self
    }

    /// The outer limit for a method that applies its own deadline after resolving a target.
    pub fn max_timeout(&mut self, limit: Duration) -> &mut Self {
        self.max_timeout = Some(limit);
        self
    }

    pub fn info(&self) -> Option<MethodInfo> {
        Some(MethodInfo {
            name: self.name.into(),
            summary: self.summary.into(),
            level: self.level.clone()?,
            stream: self.stream,
            params: self.params.clone(),
            result: self.result.clone(),
        })
    }
}

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("a schema serializes")
}

/// The registry. A clone is cheap: its handlers and hooks are shared.
#[derive(Default, Clone)]
pub struct Methods {
    map: BTreeMap<&'static str, MethodDef>,
    on_started: Vec<StartHook>,
    on_conn_closed: Vec<ConnHook>,
    access: Option<AccessCheck>,
}

impl Methods {
    /// Adds a method. Its params are read into `P`, and its result written from `R`.
    pub fn add<P, R, F, Fut>(&mut self, name: &'static str, f: F) -> &mut MethodDef
    where
        P: DeserializeOwned + JsonSchema + Send + 'static,
        R: Serialize + JsonSchema + 'static,
        F: Fn(Call, P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, RpcError>> + Send + 'static,
    {
        assert!(
            name.contains('.'),
            "method {name} is named <namespace>.<verb>"
        );
        let f = Arc::new(f);
        let handler: Handler = Arc::new(move |call, params| {
            let f = f.clone();
            Box::pin(async move {
                let params = if params.is_null() {
                    Value::Object(Default::default())
                } else {
                    params
                };
                let params: P = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid(format!("params: {e}")))?;
                let result = f(call, params).await?;
                serde_json::to_value(result).map_err(|e| RpcError::internal(format!("result: {e}")))
            })
        });
        let def = MethodDef {
            name,
            summary: "",
            level: None,
            stream: false,
            max_timeout: None,
            params: schema::<P>(),
            result: schema::<R>(),
            handler,
        };
        assert!(!self.map.contains_key(name), "method {name} is added twice");
        self.map.entry(name).or_insert(def)
    }

    /// Runs when the server becomes the leader, for facilities with work of their own. Spawned tasks end
    /// when `kernel.shutdown` is cancelled.
    pub fn on_started(&mut self, f: impl Fn(&Kernel) + Send + Sync + 'static) {
        self.on_started.push(Arc::new(f));
    }

    pub fn started(&self, kernel: &Kernel) {
        for hook in &self.on_started {
            hook(kernel);
        }
    }

    /// Runs when a connection closes, to drop what it held.
    pub fn on_connection_closed(
        &mut self,
        f: impl Fn(&Kernel, &ConnHandle) + Send + Sync + 'static,
    ) {
        self.on_conn_closed.push(Arc::new(f));
    }

    pub fn connection_closed(&self, kernel: &Kernel, conn: &ConnHandle) {
        for hook in &self.on_conn_closed {
            hook(kernel, conn);
        }
    }

    /// How a level on a resource is checked. Without one, only admins pass [`Level::Resource`].
    pub fn set_access<F, Fut>(&mut self, f: F)
    where
        F: Fn(Call, String, Grant) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<bool, RpcError>> + Send + 'static,
    {
        self.access = Some(Arc::new(move |call, uri, grant| {
            Box::pin(f(call, uri, grant))
        }));
    }

    pub fn access(&self) -> Option<AccessCheck> {
        self.access.clone()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.map.keys().copied()
    }

    /// The methods that have no level, which every caller is refused.
    pub fn without_level(&self) -> Vec<&'static str> {
        self.map
            .values()
            .filter(|m| m.level.is_none())
            .map(|m| m.name)
            .collect()
    }

    /// The report of every method with a level, in name order.
    pub fn report(&self) -> Vec<MethodInfo> {
        self.map.values().filter_map(MethodDef::info).collect()
    }

    /// Calls a method: checks the caller's level, reads the params and runs the handler until the call's
    /// deadline.
    pub fn dispatch(
        &self,
        call: Call,
        method: &str,
        params: Value,
    ) -> BoxFuture<'static, Result<Value, RpcError>> {
        let Some(def) = self.map.get(method) else {
            let refused = RpcError::method_not_found(method);
            return Box::pin(async move { Err(refused) });
        };
        let Some(level) = def.level.clone() else {
            let refused =
                RpcError::internal(format!("{method} has no level, so nobody may call it"));
            return Box::pin(async move { Err(refused) });
        };
        // A host's methods are a host half's, through the extension host's `host.env`, which knows the
        // invocation it serves. A plugin caller has no level on a host of its own.
        if matches!(call.caller, Caller::Plugin(_)) && method.starts_with("env.") && !call.host_env
        {
            let refused = RpcError::forbidden(format!(
                "{method} is a host's: a host half reaches a host through host.env, for an invocation"
            ));
            return Box::pin(async move { Err(refused) });
        }
        let handler = def.handler.clone();
        let access = self.access.clone();
        let deadline = tokio::time::Instant::from_std(
            def.max_timeout
                .map_or(call.deadline, |limit| Instant::now() + limit),
        );
        let name = def.name;
        Box::pin(async move {
            let actor = call.audit_name();
            // Writes to Rookery's typed tables record the caller in their history.
            let run = rookery_store::actor::scope(actor, async {
                allowed(&call, &level, &params, access).await?;
                handler(call.clone(), params).await
            });
            match tokio::time::timeout_at(deadline, run).await {
                Ok(done) => done,
                Err(_) => Err(RpcError::timeout(format!(
                    "{name} took longer than its deadline"
                ))),
            }
        })
    }
}

/// Whether the caller may call a method at `level`.
async fn allowed(
    call: &Call,
    level: &Level,
    params: &Value,
    access: Option<AccessCheck>,
) -> Result<(), RpcError> {
    match level {
        Level::Own => Ok(()),
        Level::Admin => call.require_admin(),
        Level::Plugin => {
            let named = params.get("plugin").and_then(Value::as_str);
            call.plugin(named).map(|_| ())
        }
        Level::Resource {
            param,
            grant,
            scheme,
        } => {
            let Some(given) = params.get(param.as_str()).and_then(Value::as_str) else {
                return Err(RpcError::invalid(format!(
                    "params: {param} names the resource"
                )));
            };
            let uri = match scheme {
                Some(scheme) if !given.contains("://") => format!("{scheme}://{given}"),
                _ => given.to_owned(),
            };
            let uri = uri.as_str();
            if call.is_admin() {
                return Ok(());
            }
            let held = match access {
                Some(check) => check(call.clone(), uri.to_owned(), *grant).await?,
                None => false,
            };
            if held {
                Ok(())
            } else {
                Err(RpcError::forbidden(format!(
                    "this needs {} on {uri}",
                    grant_name(*grant)
                )))
            }
        }
    }
}

pub fn grant_name(grant: Grant) -> &'static str {
    match grant {
        Grant::View => "view",
        Grant::Use => "use",
        Grant::Manage => "manage",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rookery_proto::Empty;

    #[test]
    fn plugin_ids_are_package_ids_and_rookery_is_for_admins() {
        for good in ["hello", "a_b", "fleet-2"] {
            assert_eq!(plugin_id(good).unwrap(), good);
        }
        for bad in ["a.b", "a/b", "Hello", "", "rookery", "sign-in"] {
            assert!(plugin_id(bad).is_err(), "{bad}");
        }
    }

    fn kernel(methods: Methods) -> Kernel {
        let store = rookery_store::Store::lazy(rookery_store::StoreOptions::new(
            "postgres://nobody@127.0.0.1:9/none",
        ))
        .unwrap();
        Kernel::new(crate::Config::new("http://127.0.0.1:7777"), store, methods)
    }

    fn member() -> Caller {
        let user = User {
            id: "01a1f29a-9205-74ff-9584-61b930804b33".into(),
            name: "Ops".into(),
            email: None,
            admin: false,
        };
        Caller::User {
            user,
            session: None,
            token: None,
        }
    }

    #[tokio::test]
    async fn levels_are_checked_before_the_handler() {
        let mut methods = Methods::default();
        methods
            .add("test.open", |_: Call, _: Empty| async { Ok(Empty {}) })
            .level(Level::Own);
        methods
            .add("test.admin", |_: Call, _: Empty| async { Ok(Empty {}) })
            .level(Level::Admin);
        methods.add("test.nolevel", |_: Call, _: Empty| async { Ok(Empty {}) });
        methods
            .add("test.host", |_: Call, _: serde_json::Value| async {
                Ok(Empty {})
            })
            .level(Level::Resource {
                param: "host".into(),
                grant: Grant::Use,
                scheme: None,
            });
        methods
            .add("test.plugin", |_: Call, _: serde_json::Value| async {
                Ok(Empty {})
            })
            .level(Level::Plugin);
        assert_eq!(methods.without_level(), ["test.nolevel"]);
        assert!(methods.report().iter().all(|m| m.name != "test.nolevel"));
        let kernel = kernel(methods);
        let run = |caller: Caller, method: &'static str, params: serde_json::Value| {
            let call = Call::new(kernel.clone(), caller, None);
            kernel.methods.dispatch(call, method, params)
        };
        let code = |r: Result<Value, RpcError>| r.err().map(|e| e.error_code());
        use rookery_proto::ErrorCode::*;
        assert_eq!(
            code(run(member(), "test.open", serde_json::json!({})).await),
            None
        );
        assert_eq!(
            code(run(member(), "test.admin", serde_json::json!({})).await),
            Some(Forbidden)
        );
        assert_eq!(
            code(run(Caller::Local, "test.admin", serde_json::json!({})).await),
            None
        );
        // Without a level nobody may call it, not even the server's own machine.
        assert_eq!(
            code(run(Caller::Local, "test.nolevel", serde_json::json!({})).await),
            Some(Internal)
        );
        // Without an access check only admins hold a level on a resource.
        assert_eq!(
            code(
                run(
                    member(),
                    "test.host",
                    serde_json::json!({"host": "env://studio"})
                )
                .await
            ),
            Some(Forbidden)
        );
        assert_eq!(
            code(run(member(), "test.host", serde_json::json!({})).await),
            Some(Invalid)
        );
        assert_eq!(
            code(
                run(
                    Caller::Local,
                    "test.host",
                    serde_json::json!({"host": "env://studio"})
                )
                .await
            ),
            None
        );
        // A plugin acts for itself; an admin names one.
        assert_eq!(
            code(
                run(
                    Caller::Plugin("fleet".into()),
                    "test.plugin",
                    serde_json::json!({})
                )
                .await
            ),
            None
        );
        assert_eq!(
            code(
                run(
                    Caller::Plugin("fleet".into()),
                    "test.plugin",
                    serde_json::json!({"plugin": "agents"})
                )
                .await
            ),
            Some(Forbidden)
        );
        assert_eq!(
            code(run(Caller::Local, "test.plugin", serde_json::json!({})).await),
            Some(Invalid)
        );
        assert_eq!(
            code(
                run(
                    Caller::Local,
                    "test.plugin",
                    serde_json::json!({"plugin": "fleet"})
                )
                .await
            ),
            None
        );
        assert_eq!(
            code(
                run(
                    member(),
                    "test.plugin",
                    serde_json::json!({"plugin": "fleet"})
                )
                .await
            ),
            Some(Forbidden)
        );
    }

    #[tokio::test]
    async fn a_call_ends_at_its_deadline() {
        let mut methods = Methods::default();
        methods
            .add("test.slow", |_: Call, _: Empty| async {
                std::future::pending::<Result<Empty, RpcError>>().await
            })
            .level(Level::Own);
        let kernel = kernel(methods);
        let mut call = Call::new(kernel.clone(), Caller::Local, None);
        call.deadline = Instant::now();
        let refused = kernel
            .methods
            .dispatch(call, "test.slow", Value::Null)
            .await
            .unwrap_err();
        assert_eq!(refused.error_code(), rookery_proto::ErrorCode::Timeout);
    }
}
