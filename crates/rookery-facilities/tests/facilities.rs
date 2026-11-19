//! The facilities through their methods, as callers of different levels call them.

use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Caller, Methods, test_kernel};
use serde_json::{Value, json};

fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_facilities::register(&mut methods);
    methods
}

fn code(result: Result<Value, rookery_kernel::RpcError>) -> &'static str {
    result
        .err()
        .map(|e| e.error_code().as_str())
        .unwrap_or("ok")
}

fn id(caller: &Caller) -> String {
    caller.user().unwrap().id.clone()
}

#[tokio::test]
async fn grants_give_levels_on_resources() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let eve = k.person("Eve", false).await;
    let studio = json!({"resource": "env://studio"});
    assert_eq!(
        k.call(&ops, "access.level", studio.clone()).await.unwrap(),
        json!({"level": null})
    );
    assert_eq!(
        k.call(&admin, "access.level", studio.clone())
            .await
            .unwrap(),
        json!({"level": "manage"})
    );
    // Only manage on the resource grants on it.
    assert_eq!(code(k.call(&ops, "access.grant", json!({"resource": "env://studio", "principal": format!("user:{}", id(&eve)), "level": "use"})).await), "FORBIDDEN");
    k.call(&admin, "access.grant", json!({"resource": "env://studio", "principal": format!("user:{}", id(&ops)), "level": "manage"})).await.unwrap();
    k.call(&ops, "access.grant", json!({"resource": "env://studio", "principal": format!("user:{}", id(&eve)), "level": "use"})).await.unwrap();
    assert_eq!(
        k.call(&eve, "access.level", studio.clone()).await.unwrap(),
        json!({"level": "use"})
    );
    assert_eq!(
        code(
            k.call(
                &eve,
                "access.require",
                json!({"resource": "env://studio", "level": "manage"})
            )
            .await
        ),
        "FORBIDDEN"
    );
    assert_eq!(
        code(
            k.call(
                &eve,
                "access.require",
                json!({"resource": "env://studio", "level": "view"})
            )
            .await
        ),
        "ok"
    );
    assert_eq!(
        code(k.call(&eve, "access.grants", studio.clone()).await),
        "FORBIDDEN"
    );
    let grants = k.call(&ops, "access.grants", studio.clone()).await.unwrap();
    assert_eq!(grants["grants"].as_array().unwrap().len(), 2);
    let filtered = k
        .call(
            &eve,
            "access.filter",
            json!({"resources": ["env://studio", "env://forge"], "level": "view"}),
        )
        .await
        .unwrap();
    assert_eq!(filtered, json!({"resources": ["env://studio"]}));
    assert_eq!(
        code(
            k.call(
                &eve,
                "access.levelOf",
                json!({"resource": "env://studio", "principal": "everyone"})
            )
            .await
        ),
        "FORBIDDEN"
    );

    // A group's grant reaches its members.
    let group = k
        .call(&admin, "admin.createGroup", json!({"name": "platform"}))
        .await
        .unwrap();
    let forge = json!({"resource": "env://forge", "principal": format!("group:{}", group["id"].as_str().unwrap()), "level": "view"});
    k.call(&admin, "access.grant", forge).await.unwrap();
    k.call(
        &admin,
        "admin.addMember",
        json!({"group": group["id"], "user": id(&eve)}),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&eve, "access.level", json!({"resource": "env://forge"}))
            .await
            .unwrap(),
        json!({"level": "view"})
    );
    k.call(
        &ops,
        "access.revoke",
        json!({"resource": "env://studio", "principal": format!("user:{}", id(&eve))}),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&eve, "access.level", studio).await.unwrap(),
        json!({"level": null})
    );
    let audit = k
        .call(&admin, "audit.list", json!({"resource": "env://studio"}))
        .await
        .unwrap();
    assert_eq!(audit["entries"][0]["action"], "access.revoke");
    assert_eq!(
        code(k.call(&ops, "audit.list", json!({})).await),
        "FORBIDDEN"
    );
    k.close().await;
}

#[tokio::test]
async fn a_grant_names_its_principal_in_any_spelling() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let eve = k.person("Eve", false).await;
    let studio = json!({"resource": "env://studio"});
    let upper = format!("user:{}", id(&eve).to_uppercase());
    k.call(
        &admin,
        "access.grant",
        json!({"resource": "env://studio", "principal": upper, "level": "use"}),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&eve, "access.level", studio.clone()).await.unwrap(),
        json!({"level": "use"})
    );
    let grants = k
        .call(&admin, "access.grants", studio.clone())
        .await
        .unwrap();
    assert_eq!(
        grants["grants"][0]["principal"],
        format!("user:{}", id(&eve))
    );
    // It is revoked by its own spelling as well as by another.
    k.call(
        &admin,
        "access.revoke",
        json!({"resource": "env://studio", "principal": format!("user:{}", id(&eve).replace('-', ""))}),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&eve, "access.level", studio).await.unwrap(),
        json!({"level": null})
    );
    k.close().await;
}

#[tokio::test]
async fn admins_manage_people() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let users = k.call(&admin, "admin.users", json!({})).await.unwrap();
    assert_eq!(users["users"].as_array().unwrap().len(), 2);
    assert_eq!(
        code(k.call(&ops, "admin.users", json!({})).await),
        "FORBIDDEN"
    );
    let alternate = id(&ops).replace('-', "");
    // A disabled person's connections end even with a different spelling of the same UUID.
    let inbox = k.connect(&ops);
    let off = k
        .call(
            &admin,
            "admin.setDisabled",
            json!({"id": alternate, "disabled": true}),
        )
        .await
        .unwrap();
    assert_eq!(off["disabled"], true);
    assert!(inbox.conn.ended());
    // The last admin stays one.
    assert_eq!(
        code(
            k.call(
                &admin,
                "admin.setRole",
                json!({"id": id(&admin), "admin": false})
            )
            .await
        ),
        "CONFLICT"
    );
    k.call(
        &admin,
        "admin.setDisabled",
        json!({"id": id(&ops), "disabled": false}),
    )
    .await
    .unwrap();
    let inbox = k.connect(&ops);
    assert_eq!(
        k.call(
            &admin,
            "admin.setRole",
            json!({"id": alternate, "admin": true})
        )
        .await
        .unwrap()["admin"],
        true
    );
    assert!(inbox.conn.ended());
    k.close().await;
}

#[tokio::test]
async fn settings_resolve_and_only_admins_set_global_ones() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let fleet = Caller::Plugin("fleet".into());
    let set = |scope: &str, value: Value| json!({"plugin": "fleet", "key": "columns", "scope": scope, "value": value});
    k.call(&fleet, "settings.set", set("default", json!(3)))
        .await
        .unwrap();
    assert_eq!(
        code(k.call(&ops, "settings.set", set("default", json!(9))).await),
        "FORBIDDEN"
    );
    assert_eq!(
        code(k.call(&ops, "settings.set", set("global", json!(9))).await),
        "FORBIDDEN"
    );
    k.call(&admin, "settings.set", set("global", json!(4)))
        .await
        .unwrap();
    k.call(&ops, "settings.set", set("user", json!(5)))
        .await
        .unwrap();
    let get = json!({"plugin": "fleet", "key": "columns"});
    assert_eq!(
        k.call(&ops, "settings.get", get.clone()).await.unwrap(),
        json!({"key": "columns", "value": 5, "scope": "user"})
    );
    assert_eq!(
        k.call(&admin, "settings.get", get.clone()).await.unwrap()["value"],
        4
    );
    assert_eq!(
        k.call(
            &admin,
            "settings.get",
            json!({"plugin": "fleet", "key": "none"})
        )
        .await
        .unwrap(),
        json!({"key": "none", "value": null, "scope": null})
    );
    k.call(
        &ops,
        "settings.reset",
        json!({"plugin": "fleet", "key": "columns", "scope": "user"}),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&ops, "settings.list", json!({"plugin": "fleet"}))
            .await
            .unwrap()["entries"][0]["scope"],
        "global"
    );
    assert_eq!(
        code(
            k.call(
                &ops,
                "settings.get",
                json!({"plugin": "NOT A PLUGIN", "key": "x"})
            )
            .await
        ),
        "INVALID"
    );
    k.close().await;
}

#[tokio::test]
async fn secrets_belong_to_their_plugin_or_person() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let fleet = Caller::Plugin("fleet".into());
    let secret = json!({"plugin": "fleet", "name": "api", "value": "made-up-secret"});
    k.call(&fleet, "secrets.set", secret.clone()).await.unwrap();
    assert_eq!(
        code(k.call(&ops, "secrets.set", secret.clone()).await),
        "FORBIDDEN"
    );
    assert_eq!(
        code(
            k.call(
                &Caller::Plugin("other".into()),
                "secrets.get",
                json!({"plugin": "fleet", "name": "api"})
            )
            .await
        ),
        "FORBIDDEN"
    );
    assert_eq!(
        k.call(
            &admin,
            "secrets.get",
            json!({"plugin": "fleet", "name": "api"})
        )
        .await
        .unwrap(),
        json!({"value": "made-up-secret"})
    );
    k.call(
        &ops,
        "secrets.set",
        json!({"plugin": "fleet", "name": "api", "scope": "user", "value": "mine"}),
    )
    .await
    .unwrap();
    // A person sees their own secrets, not the global ones; none shows a value.
    let mine = k.call(&ops, "secrets.list", json!({})).await.unwrap();
    assert_eq!(mine["secrets"].as_array().unwrap().len(), 1);
    assert!(
        mine.to_string()
            .find("mine")
            .is_none_or(|_| mine["secrets"][0].get("value").is_none())
    );
    assert_eq!(
        k.call(&admin, "secrets.list", json!({"plugin": "fleet"}))
            .await
            .unwrap()["secrets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    k.call(
        &fleet,
        "secrets.rename",
        json!({"plugin": "fleet", "name": "api", "newName": "token"}),
    )
    .await
    .unwrap();
    k.call(
        &fleet,
        "secrets.delete",
        json!({"plugin": "fleet", "name": "token"}),
    )
    .await
    .unwrap();
    assert_eq!(
        code(
            k.call(
                &fleet,
                "secrets.delete",
                json!({"plugin": "fleet", "name": "token"})
            )
            .await
        ),
        "NOT_FOUND"
    );
    k.close().await;
}

#[tokio::test]
async fn a_server_without_a_key_refuses_secrets_only() {
    let db = match rookery_store::testing::TestDb::new().await {
        Some(db) => db,
        None => return,
    };
    let store = db.store.with_key(None);
    let kernel = rookery_kernel::Kernel::new(
        rookery_kernel::Config::new("http://127.0.0.1:7777"),
        store,
        methods(),
    );
    let call = |caller: Caller, method: &'static str, params: Value| {
        let call = rookery_kernel::Call::new(kernel.clone(), caller, None);
        kernel.methods.dispatch(call, method, params)
    };
    let refused = call(
        Caller::Local,
        "secrets.set",
        json!({"plugin": "fleet", "name": "api", "value": "x"}),
    )
    .await;
    assert_eq!(code(refused), "UNAVAILABLE");
    let refused = call(Caller::Local, "connections.create", json!({"type": "http", "name": "x", "config": {"url": "https://api.example.test"}, "credential": "t"})).await;
    assert_eq!(code(refused), "UNAVAILABLE");
    // A connection with no credential needs no key, and nor does storage.
    call(
        Caller::Local,
        "connections.create",
        json!({"type": "http", "name": "x", "config": {"url": "https://api.example.test"}}),
    )
    .await
    .unwrap();
    call(
        Caller::Local,
        "storage.set",
        json!({"plugin": "fleet", "key": "k", "value": 1}),
    )
    .await
    .unwrap();
    db.close().await;
}

#[tokio::test]
async fn storage_is_a_plugins_with_revisions() {
    let k: TestKernel = test_kernel!(methods());
    let fleet = Caller::Plugin("fleet".into());
    let ops = k.person("Ops", false).await;
    let admin = k.person("Maria", true).await;
    let set =
        |revision: Option<i64>| json!({"key": "cursor", "value": {"at": 3}, "revision": revision});
    assert_eq!(
        k.call(&fleet, "storage.set", set(Some(0))).await.unwrap(),
        json!({"revision": 1})
    );
    assert_eq!(
        code(k.call(&fleet, "storage.set", set(Some(0))).await),
        "CONFLICT"
    );
    assert_eq!(
        k.call(&fleet, "storage.set", set(Some(1))).await.unwrap(),
        json!({"revision": 2})
    );
    assert_eq!(
        k.call(&fleet, "storage.get", json!({"key": "cursor"}))
            .await
            .unwrap()["revision"],
        2
    );
    // People do not reach a plugin's storage; an admin does by naming it.
    assert_eq!(
        code(
            k.call(
                &ops,
                "storage.get",
                json!({"plugin": "fleet", "key": "cursor"})
            )
            .await
        ),
        "FORBIDDEN"
    );
    assert_eq!(
        k.call(
            &admin,
            "storage.get",
            json!({"plugin": "fleet", "key": "cursor"})
        )
        .await
        .unwrap()["value"],
        json!({"at": 3})
    );
    assert_eq!(
        code(
            k.call(
                &Caller::Plugin("other".into()),
                "storage.get",
                json!({"plugin": "fleet", "key": "cursor"})
            )
            .await
        ),
        "FORBIDDEN"
    );
    let none = k
        .call(
            &Caller::Plugin("other".into()),
            "storage.get",
            json!({"key": "cursor"}),
        )
        .await
        .unwrap();
    assert_eq!(
        (none["value"].clone(), none["revision"].as_i64()),
        (json!(null), Some(0))
    );
    // A plugin outside a person's call has no user scope.
    assert_eq!(
        code(
            k.call(&fleet, "storage.get", json!({"key": "x", "scope": "user"}))
                .await
        ),
        "INVALID"
    );
    assert_eq!(
        k.call(&fleet, "storage.list", json!({"prefix": "cur"}))
            .await
            .unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        k.call(
            &fleet,
            "storage.delete",
            json!({"key": "cursor", "revision": 2})
        )
        .await
        .unwrap(),
        json!({"deleted": true})
    );
    k.close().await;
}

/// A made-up service on `addr` that answers 200, or sends every request to `to`.
async fn service(addr: &str, to: Option<String>) -> String {
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new().fallback(move || {
        let to = to.clone();
        async move {
            match to {
                Some(to) => axum::response::Redirect::temporary(&to).into_response(),
                None => axum::http::StatusCode::OK.into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// Makes a connection of `caller`'s own to `url` and tests it.
async fn test(k: &TestKernel, caller: Caller, url: String) -> Value {
    let made = k
        .call(
            &caller,
            "connections.create",
            json!({"type": "http", "name": format!("svc-{}", uuid::Uuid::now_v7()), "scope": "user", "config": {"url": url}}),
        )
        .await
        .unwrap();
    k.call(&caller, "connections.test", json!({"id": made["id"]}))
        .await
        .unwrap()
}

#[tokio::test]
async fn testing_a_connection_does_not_reach_internal_addresses() {
    let inside = service("127.0.0.2:0", None).await;
    let hop = service("127.0.0.1:0", Some(format!("{inside}/"))).await;

    let k: TestKernel = test_kernel!(methods());
    let ops = k.person("Ops", false).await;
    for url in [
        inside.clone(),
        hop.clone(),
        "http://localhost:9/".to_owned(),
        "http://[::1]:9/".to_owned(),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
    ] {
        let tested = test(&k, ops.clone(), url.clone()).await;
        assert_eq!(tested["ok"], false, "{url}: {tested}");
        assert!(
            tested["message"]
                .as_str()
                .unwrap()
                .contains("ROOKERY_OUTBOUND_ALLOW"),
            "{url}: {tested}"
        );
    }
    k.close().await;

    // Allowed, the first hop answers, but the redirect to an address that is not allowed is refused.
    let k: TestKernel = test_kernel!(methods(), |config| {
        config.outbound_allow =
            rookery_kernel::outbound::AddrRange::parse_list("127.0.0.1").unwrap();
    });
    let ops = k.person("Ops", false).await;
    let tested = test(&k, ops.clone(), hop).await;
    assert_eq!(tested["ok"], false, "{tested}");
    assert!(
        tested["message"].as_str().unwrap().contains("127.0.0.2"),
        "{tested}"
    );
    k.close().await;

    let k: TestKernel = test_kernel!(methods(), |config| {
        config.outbound_allow =
            rookery_kernel::outbound::AddrRange::parse_list("127.0.0.0/8").unwrap();
    });
    let ops = k.person("Ops", false).await;
    let tested = test(&k, ops, inside).await;
    assert_eq!(tested["ok"], true, "{tested}");
    k.close().await;
}

#[tokio::test]
async fn connections_keep_credentials_sealed() {
    // A made-up registry that wants Basic ops:made-up-password.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/v2/",
        axum::routing::get(|headers: axum::http::HeaderMap| async move {
            let want = "Basic b3BzOm1hZGUtdXAtcGFzc3dvcmQ=";
            if headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(want) {
                axum::http::StatusCode::OK
            } else {
                axum::http::StatusCode::UNAUTHORIZED
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // The made-up registry is on this machine, which an installation allows for its own network.
    let k: TestKernel = test_kernel!(methods(), |config| {
        config.outbound_allow =
            rookery_kernel::outbound::AddrRange::parse_list("127.0.0.1").unwrap();
    });
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let types = k.call(&ops, "connections.types", json!({})).await.unwrap();
    assert!(
        types["types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == "registry")
    );
    let registry = json!({"type": "registry", "name": "studio-registry", "config": {"url": url, "username": "ops"}, "credential": "made-up-password"});
    assert_eq!(
        code(k.call(&ops, "connections.create", registry.clone()).await),
        "FORBIDDEN"
    );
    let made = k
        .call(&admin, "connections.create", registry)
        .await
        .unwrap();
    assert_eq!(made["hasCredential"], true);
    assert!(!made.to_string().contains("made-up-password"));
    let listed = k
        .call(&ops, "connections.list", json!({"type": "registry"}))
        .await
        .unwrap();
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
    assert_eq!(
        k.call(&admin, "connections.test", json!({"id": made["id"]}))
            .await
            .unwrap()["ok"],
        true
    );
    k.call(
        &admin,
        "connections.update",
        json!({"id": made["id"], "credential": "wrong"}),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&admin, "connections.test", json!({"id": made["id"]}))
            .await
            .unwrap()["ok"],
        false
    );
    // Only the plugin that declares its type, or an admin, gets the credential. Rookery's own
    // types are not a plugin's: it declares one of its own and reads only that one.
    assert_eq!(
        code(
            k.call(&ops, "connections.resolve", json!({"id": made["id"]}))
                .await
        ),
        "FORBIDDEN"
    );
    assert_eq!(
        code(
            k.call(
                &Caller::Plugin("docker".into()),
                "connections.resolve",
                json!({"type": "registry", "name": "studio-registry"}),
            )
            .await
        ),
        "FORBIDDEN"
    );
    let resolved = k
        .call(
            &admin,
            "connections.resolve",
            json!({"id": made["id"], "plugin": "docker"}),
        )
        .await
        .unwrap();
    assert_eq!(resolved["credential"], "wrong");
    assert_eq!(
        code(
            k.call(&ops, "connections.delete", json!({"id": made["id"]}))
                .await
        ),
        "FORBIDDEN"
    );
    assert_eq!(
        code(
            k.call(
                &admin,
                "connections.create",
                json!({"type": "ftp", "name": "x", "config": {}})
            )
            .await
        ),
        "INVALID"
    );
    assert_eq!(
        code(
            k.call(
                &admin,
                "connections.create",
                json!({"type": "http", "name": "x", "config": {"url": "ftp://x"}})
            )
            .await
        ),
        "INVALID"
    );
    k.call(&admin, "connections.delete", json!({"id": made["id"]}))
        .await
        .unwrap();
    k.close().await;
}

#[tokio::test]
async fn a_connection_type_takes_its_secret_only_as_a_list_of_one_field() {
    use rookery_facilities::connections::PluginKinds;

    let k: TestKernel = test_kernel!(methods());
    let fields = json!({
        "type": "object",
        "required": ["key"],
        "properties": {"key": {"type": "string"}, "token": {"type": "string"}}
    });
    let manifest = json!({"contributes": {"connectionTypes": [
        {"id": "models.text", "title": "A string secret", "fields": fields, "secret": "key"},
        {"id": "models.pair", "title": "Two secrets", "fields": fields, "secret": ["key", "token"]},
        {"id": "models.object", "title": "An object secret", "fields": fields, "secret": {"key": true}},
        {"id": "models.absent", "title": "No secret", "fields": fields},
        {"id": "models.null", "title": "A null secret", "fields": fields, "secret": null},
        {"id": "models.one", "title": "One secret", "fields": fields, "secret": ["key"]}
    ]}});
    k.kernel
        .set_facility(PluginKinds::from_manifests([("models", &manifest)]));
    let admin = k.person("Mira", true).await;
    let types = k
        .call(&admin, "connections.types", json!({}))
        .await
        .unwrap();
    // A secret that is not a list of one field's name is no type at all: it would
    // leave the secret in the plain config.
    let ids: Vec<&str> = types["types"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|kind| kind["id"].as_str())
        .filter(|id| id.starts_with("models."))
        .collect();
    assert_eq!(ids, ["models.absent", "models.null", "models.one"]);
    let one = types["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["id"] == "models.one")
        .unwrap();
    assert_eq!(one["credential"], "key");
    assert!(one["config"]["properties"].get("key").is_none());
    let absent = types["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["id"] == "models.absent")
        .unwrap();
    assert_eq!(absent["credential"], json!(null));
    k.close().await;
}

#[tokio::test]
async fn a_plugin_connection_seals_its_secret_for_the_declaring_plugin() {
    use rookery_facilities::connections::PluginKinds;

    let k: TestKernel = test_kernel!(methods());
    let manifest = json!({"contributes": {"connectionTypes": [{
        "id": "models.example",
        "title": "Example Models",
        "fields": {
            "type": "object",
            "required": ["key"],
            "properties": {"key": {"type": "string"}}
        },
        "secret": ["key"],
        "scopes": ["global"]
    }]}});
    k.kernel
        .set_facility(PluginKinds::from_manifests([("models", &manifest)]));
    let admin = k.person("Mira", true).await;
    let types = k
        .call(&admin, "connections.types", json!({}))
        .await
        .unwrap();
    let listed = types["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["id"] == "models.example")
        .unwrap();
    assert_eq!(listed["credential"], "key");
    assert!(listed["config"]["properties"].get("key").is_none());

    let mut params = json!({
        "type": "models.example",
        "name": "Example",
        "scope": "global",
        "config": {"key": "plain-key"},
        "credential": "made-up-key"
    });
    assert_eq!(
        code(k.call(&admin, "connections.create", params.clone()).await),
        "INVALID"
    );
    params["config"] = json!({});
    let made = k.call(&admin, "connections.create", params).await.unwrap();
    assert_eq!(made["hasCredential"], true);
    assert_eq!(made["config"], json!({}));
    let reference = json!({"id": made["id"]});
    assert_eq!(
        code(
            k.call(
                &Caller::Plugin("other".into()),
                "connections.resolve",
                reference.clone()
            )
            .await
        ),
        "FORBIDDEN"
    );
    let resolved = k
        .call(
            &Caller::Plugin("models".into()),
            "connections.resolve",
            reference,
        )
        .await
        .unwrap();
    assert_eq!(resolved["credential"], "made-up-key");
    k.close().await;
}

#[tokio::test]
async fn blobs_are_read_by_their_owner_on_a_stream() {
    let k: TestKernel = test_kernel!(methods());
    let ops = k.person("Ops", false).await;
    let eve = k.person("Eve", false).await;
    let put = k
        .call(
            &ops,
            "blobs.put",
            json!({"contentType": "text/plain", "name": "notes.txt", "data": "aGVsbG8="}),
        )
        .await
        .unwrap();
    assert_eq!(put["size"], 5);
    assert_eq!(
        code(k.call(&eve, "blobs.get", json!({"id": put["id"]})).await),
        "NOT_FOUND"
    );
    let mut inbox = k.connect(&ops);
    let opened = k
        .call_on(&inbox, "blobs.read", json!({"id": put["id"]}))
        .await
        .unwrap();
    let stream = opened["stream"].as_u64().unwrap() as u32;
    assert_eq!(inbox.frame(stream).await, b"hello");
    k.close().await;
}

#[tokio::test]
async fn layouts_are_a_persons_own() {
    let k: TestKernel = test_kernel!(methods());
    let ops = k.person("Ops", false).await;
    assert_eq!(
        k.call(&ops, "layout.get", json!({})).await.unwrap(),
        json!({"value": null, "revision": 0})
    );
    assert_eq!(
        k.call(
            &ops,
            "layout.put",
            json!({"value": {"left": 240}, "revision": 0})
        )
        .await
        .unwrap(),
        json!({"revision": 1})
    );
    assert_eq!(
        code(
            k.call(&ops, "layout.put", json!({"value": {}, "revision": 0}))
                .await
        ),
        "CONFLICT"
    );
    assert_eq!(
        k.call(&ops, "layout.get", json!({})).await.unwrap(),
        json!({"value": {"left": 240}, "revision": 1})
    );
    assert_eq!(
        code(k.call(&Caller::Local, "layout.get", json!({})).await),
        "INVALID"
    );
    k.close().await;
}

#[tokio::test]
async fn a_plugin_asks_what_one_person_holds_and_a_manager_finds_people_to_share_with() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let eve = k.person("Eve", false).await;
    let fleet = Caller::Plugin("fleet".into());
    k.call(
        &admin,
        "access.grant",
        json!({"resource": "env://studio", "principal": format!("user:{}", id(&ops)), "level": "manage"}),
    )
    .await
    .unwrap();
    let asked = |principal: String| json!({"resource": "env://studio", "principal": principal});
    assert_eq!(
        k.call(
            &fleet,
            "access.levelOf",
            asked(format!("user:{}", id(&ops)))
        )
        .await
        .unwrap(),
        json!({"level": "manage"})
    );
    assert_eq!(
        k.call(
            &fleet,
            "access.levelOf",
            asked(format!("user:{}", id(&eve)))
        )
        .await
        .unwrap(),
        json!({"level": null})
    );
    // One person at a time; a person asks nothing of others.
    assert_eq!(
        code(
            k.call(&fleet, "access.levelOf", asked("everyone".into()))
                .await
        ),
        "INVALID"
    );
    assert_eq!(
        code(
            k.call(&eve, "access.levelOf", asked(format!("user:{}", id(&ops))))
                .await
        ),
        "FORBIDDEN"
    );
    // Someone who holds manage finds people by a part of their name; someone who does not, nobody.
    let search = json!({"resource": "env://studio", "text": "ev"});
    let found = k.call(&ops, "users.search", search.clone()).await.unwrap();
    assert_eq!(found["people"][0]["name"], "Eve");
    assert_eq!(found["people"].as_array().unwrap().len(), 1);
    assert_eq!(
        code(k.call(&eve, "users.search", search).await),
        "FORBIDDEN"
    );
    k.close().await;
}

/// A space owned by `owner`, with `member` holding `level` on it.
async fn space_of(k: &TestKernel, owner: &Caller, member: &Caller, level: i16) -> String {
    let uid = |c: &Caller| uuid::Uuid::parse_str(&c.user().unwrap().id).unwrap();
    let s =
        k.db.store
            .spaces()
            .create(
                &rookery_store::Caller::user(uid(owner)),
                &rookery_store::spaces::NewSpace {
                    kind: "notes.team".into(),
                    name: "Platform".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    k.db.store
        .access()
        .grant(
            &format!("space://{}", s.id),
            &format!("user:{}", uid(member)),
            level,
            "system:test",
        )
        .await
        .unwrap();
    s.id.to_string()
}

#[tokio::test]
async fn settings_storage_and_connections_have_a_space_scope_for_its_members() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let ada = k.person("Ada", false).await;
    let bob = k.person("Bob", false).await;
    let eve = k.person("Eve", false).await;
    let space = space_of(&k, &ada, &bob, 1).await;
    let set = |scope: &str, value: i64| json!({ "plugin": "notes", "key": "pageSize", "value": value, "scope": scope, "space": space });
    k.call(&admin, "settings.set", set("global", 50))
        .await
        .unwrap();
    // A space's value is its managers' to set; its members read it in the space.
    assert_eq!(
        code(k.call(&bob, "settings.set", set("space", 20)).await),
        "FORBIDDEN"
    );
    k.call(&ada, "settings.set", set("space", 20))
        .await
        .unwrap();
    let get = |space: Option<&str>| match space {
        Some(s) => json!({ "plugin": "notes", "key": "pageSize", "space": s }),
        None => json!({ "plugin": "notes", "key": "pageSize" }),
    };
    assert_eq!(
        k.call(&bob, "settings.get", get(Some(&space)))
            .await
            .unwrap(),
        json!({ "key": "pageSize", "value": 20, "scope": "space" })
    );
    assert_eq!(
        k.call(&bob, "settings.get", get(None)).await.unwrap()["value"],
        50
    );
    assert_eq!(
        code(k.call(&eve, "settings.get", get(Some(&space))).await),
        "FORBIDDEN"
    );
    // A person's own value comes first.
    k.call(&bob, "settings.set", set("user", 5)).await.unwrap();
    assert_eq!(
        k.call(&bob, "settings.get", get(Some(&space)))
            .await
            .unwrap()["value"],
        5
    );
    k.call(
        &ada,
        "settings.reset",
        json!({ "plugin": "notes", "key": "pageSize", "scope": "space", "space": space }),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&ada, "settings.get", get(Some(&space)))
            .await
            .unwrap()["value"],
        50
    );

    // A plugin keeps values of a space of its own.
    let notes = Caller::Plugin("notes".into());
    let stored = json!({ "key": "cursor", "value": 3, "scope": "space", "space": space });
    k.call(&notes, "storage.set", stored).await.unwrap();
    assert_eq!(
        k.call(
            &notes,
            "storage.get",
            json!({ "key": "cursor", "scope": "space", "space": space })
        )
        .await
        .unwrap()["value"],
        3
    );
    assert_eq!(
        k.call(&notes, "storage.get", json!({ "key": "cursor" }))
            .await
            .unwrap()["value"],
        Value::Null
    );

    // A space's connection is its managers' to make and change, and its members' to see.
    let create = json!({ "type": "http", "name": "wiki", "scope": "space", "space": space,
                         "config": { "url": "https://wiki.example.test" } });
    assert_eq!(
        code(k.call(&bob, "connections.create", create.clone()).await),
        "FORBIDDEN"
    );
    let made = k.call(&ada, "connections.create", create).await.unwrap();
    assert_eq!(
        (made["scope"].clone(), made["spaceId"].clone()),
        (json!("space"), json!(space))
    );
    let listed = k
        .call(&bob, "connections.list", json!({ "space": space }))
        .await
        .unwrap();
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
    assert!(
        k.call(&bob, "connections.list", json!({})).await.unwrap()["connections"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        code(
            k.call(&eve, "connections.list", json!({ "space": space }))
                .await
        ),
        "FORBIDDEN"
    );
    assert_eq!(
        code(
            k.call(&eve, "connections.get", json!({ "id": made["id"] }))
                .await
        ),
        "NOT_FOUND"
    );
    assert_eq!(
        code(
            k.call(&bob, "connections.delete", json!({ "id": made["id"] }))
                .await
        ),
        "NOT_FOUND"
    );
    // A plugin resolves a space's connection only for a person who uses the space.
    let docs = Caller::Plugin("docs".into());
    let id_of = |c: &Caller| c.user().unwrap().id.clone();
    let by_id = |user: Option<String>| match user {
        Some(u) => json!({ "id": made["id"], "user": u }),
        None => json!({ "id": made["id"] }),
    };
    assert_eq!(
        code(k.call(&docs, "connections.resolve", by_id(None)).await),
        "FORBIDDEN"
    );
    for outsider in [&bob, &eve] {
        assert_eq!(
            code(
                k.call(&docs, "connections.resolve", by_id(Some(id_of(outsider))))
                    .await
            ),
            "NOT_FOUND",
            "view is not use"
        );
    }
    let resolved = k
        .call(&docs, "connections.resolve", by_id(Some(id_of(&ada))))
        .await
        .unwrap();
    assert_eq!(resolved["connection"]["name"], "wiki");
    let by_name = json!({ "type": "http", "name": "wiki", "space": space, "user": id_of(&ada) });
    assert_eq!(
        k.call(&docs, "connections.resolve", by_name).await.unwrap()["connection"]["id"],
        made["id"]
    );
    assert_eq!(
        code(
            k.call(
                &docs,
                "connections.resolve",
                json!({ "type": "http", "name": "wiki", "user": id_of(&ada) })
            )
            .await
        ),
        "NOT_FOUND",
        "without the space, only the person's own and global connections"
    );
    // A space's setting is audited on the space.
    let audited =
        k.db.store
            .audit()
            .list(&rookery_store::audit::Filter {
                action: Some("settings.set".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
    assert!(
        audited
            .iter()
            .any(|e| e.resource.as_deref() == Some(&format!("space://{space}")))
    );
    k.call(&ada, "connections.delete", json!({ "id": made["id"] }))
        .await
        .unwrap();
    // A secret has no space scope.
    assert_eq!(
        code(
            k.call(
                &notes,
                "secrets.set",
                json!({ "name": "token", "value": "x", "scope": "space" })
            )
            .await
        ),
        "INVALID"
    );
    k.close().await;
}

#[tokio::test]
async fn disabled_accounts_hold_no_access_level() {
    let k: TestKernel = test_kernel!(methods());
    let _active_admin = k.person("Fran", true).await;
    let plugin = Caller::Plugin("fleet".into());
    for admin in [true, false] {
        let user = k.person(if admin { "Dana" } else { "Jo" }, admin).await;
        let resource = "env://studio";
        let principal = format!("user:{}", id(&user));
        k.db.store
            .access()
            .grant(resource, &principal, 2, "system:test")
            .await
            .unwrap();
        k.db.store
            .users()
            .set_disabled(id(&user).parse().unwrap(), true)
            .await
            .unwrap();
        assert_eq!(
            k.call(
                &plugin,
                "access.levelOf",
                json!({ "resource": resource, "principal": principal })
            )
            .await
            .unwrap(),
            json!({"level": null})
        );
    }
    k.close().await;
}
