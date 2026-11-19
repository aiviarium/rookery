//! `data.*` through the methods, as people, admins and plugins call them: scoping and access first,
//! then live queries, Rookery's own collections and plugins' migrations.

use std::time::Duration;

use rookery_data::plugins::{self, PluginData, Transforms};
use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Caller, Kernel, Methods, RpcError, test_kernel};
use rookery_store::Caller as StoreCaller;
use rookery_store::spaces::NewSpace;
use serde_json::{Value, json};

fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_data::register(&mut methods);
    methods
}

fn code(result: Result<Value, RpcError>) -> &'static str {
    result
        .err()
        .map(|e| e.error_code().as_str())
        .unwrap_or("ok")
}

fn uid(caller: &Caller) -> uuid::Uuid {
    caller.user().unwrap().id.parse().unwrap()
}

fn notes() -> Value {
    json!([
        {
            "name": "notes",
            "scope": "space",
            "entity": { "scheme": "note", "title": "title" },
            "access": { "read": "view", "write": "use" },
            "history": true,
            "trash": true,
            "fields": [
                { "name": "title", "type": "text", "search": true, "required": true },
                { "name": "body", "type": "richtext" },
                { "name": "about", "type": "ref" }
            ]
        },
        {
            "name": "drafts",
            "scope": "user",
            "fields": [{ "name": "text", "type": "longtext" }]
        },
        {
            "name": "settings",
            "scope": "global",
            "access": { "read": "view", "write": "manage" },
            "fields": [{ "name": "key", "type": "text", "unique": true }, { "name": "value", "type": "json" }]
        }
    ])
}

async fn install(k: &TestKernel, collections: Value) {
    plugins::apply(
        &k.kernel,
        &PluginData {
            plugin: "notes".into(),
            collections,
            ..Default::default()
        },
        &plugins::NoTransforms,
    )
    .await
    .expect("applying the plugin's collections");
}

/// A space of `owner`'s, with `member` holding `level` on it.
async fn space(
    k: &TestKernel,
    owner: &Caller,
    member: Option<(&Caller, rookery_store::Level)>,
) -> String {
    let s =
        k.db.store
            .spaces()
            .create(
                &StoreCaller::user(uid(owner)),
                &NewSpace {
                    kind: "project".into(),
                    name: "Rookery".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    if let Some((m, level)) = member {
        k.db.store
            .spaces()
            .add_member(
                &StoreCaller::user(uid(owner)),
                s.id,
                &format!("user:{}", uid(m)),
                level,
            )
            .await
            .unwrap();
    }
    s.id.to_string()
}

fn titles(page: &Value) -> Vec<String> {
    let mut out: Vec<String> = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["title"].as_str().unwrap().to_owned())
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn reads_and_writes_follow_the_scope_and_the_declared_levels() {
    let k: TestKernel = test_kernel!(methods());
    install(&k, notes()).await;
    let ada = k.person("Ada", false).await;
    let bob = k.person("Bob", false).await;
    let eve = k.person("Eve", false).await;
    let s = space(&k, &ada, Some((&bob, rookery_store::Level::View))).await;
    let at = |c: &str| json!({ "plugin": "notes", "collection": c, "space": s });
    let with = |c: &str, extra: Value| {
        let mut v = at(c);
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    let row = k
        .call(
            &ada,
            "data.insert",
            with("notes", json!({ "values": { "title": "plan" } })),
        )
        .await
        .unwrap()["row"]
        .clone();
    assert_eq!(row["spaceId"], s);
    // A member with view reads and may not write; someone outside the space does neither.
    assert_eq!(
        titles(&k.call(&bob, "data.find", at("notes")).await.unwrap()),
        ["plan"]
    );
    assert_eq!(
        code(
            k.call(
                &bob,
                "data.insert",
                with("notes", json!({ "values": { "title": "x" } }))
            )
            .await
        ),
        "FORBIDDEN"
    );
    assert!(titles(&k.call(&eve, "data.find", at("notes")).await.unwrap()).is_empty());
    assert_eq!(
        code(
            k.call(&eve, "data.get", with("notes", json!({ "id": row["id"] })))
                .await
                .map(|r| r["row"].clone())
                .and_then(|r| if r.is_null() {
                    Err(RpcError::not_found("none"))
                } else {
                    Ok(r)
                })
        ),
        "NOT_FOUND"
    );
    // A space's collection needs the space; a person names the plugin.
    assert_eq!(
        code(
            k.call(
                &ada,
                "data.find",
                json!({ "plugin": "notes", "collection": "notes" })
            )
            .await
        ),
        "INVALID"
    );
    assert_eq!(
        code(
            k.call(
                &ada,
                "data.find",
                json!({ "collection": "notes", "space": s })
            )
            .await
        ),
        "INVALID"
    );
    // A person's own rows are theirs alone.
    k.call(
        &ada,
        "data.insert",
        with("drafts", json!({ "values": { "text": "mine" } })),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&ada, "data.count", at("drafts")).await.unwrap(),
        json!({ "count": 1 })
    );
    assert_eq!(
        k.call(&bob, "data.count", at("drafts")).await.unwrap(),
        json!({ "count": 0 })
    );
    // The global collection needs manage on plugin://notes to write, which everyone lacks.
    assert_eq!(
        code(
            k.call(
                &ada,
                "data.insert",
                with("settings", json!({ "values": { "key": "a" } }))
            )
            .await
        ),
        "FORBIDDEN"
    );
    // The plugin acts on its own collections as their admin, and on no others.
    let plugin = Caller::Plugin("notes".into());
    k.call(
        &plugin,
        "data.insert",
        json!({ "collection": "settings", "values": { "key": "a" } }),
    )
    .await
    .unwrap();
    assert_eq!(
        titles(
            &k.call(
                &plugin,
                "data.find",
                json!({ "collection": "notes", "space": s })
            )
            .await
            .unwrap()
        ),
        ["plan"]
    );
    let other = Caller::Plugin("other".into());
    assert_eq!(
        code(k.call(&other, "data.find", at("notes")).await),
        "FORBIDDEN"
    );
    // Updates take a revision, and history records them.
    let id = row["id"].clone();
    k.call(
        &ada,
        "data.update",
        with(
            "notes",
            json!({ "id": id, "patch": { "title": "plan b" }, "rev": 1 }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        code(
            k.call(
                &ada,
                "data.update",
                with(
                    "notes",
                    json!({ "id": id, "patch": { "title": "c" }, "rev": 1 })
                )
            )
            .await
        ),
        "CONFLICT"
    );
    let history = k
        .call(&bob, "data.history", with("notes", json!({ "id": id })))
        .await
        .unwrap();
    assert_eq!(
        history["entries"][0]["changes"]["title"],
        json!(["plan", "plan b"])
    );
    assert_eq!(
        history["entries"][0]["principal"],
        format!("user:{}", uid(&ada))
    );
    k.close().await;
}

#[tokio::test]
async fn references_and_mentions_reach_the_people_they_name() {
    let k: TestKernel = test_kernel!(methods());
    install(&k, notes()).await;
    let ada = k.person("Ada", false).await;
    let bob = k.person("Bob", false).await;
    let s = space(&k, &ada, Some((&bob, rookery_store::Level::Use))).await;
    let first = k
        .call(&ada, "data.insert", json!({ "plugin": "notes", "collection": "notes", "space": s, "values": { "title": "first" } }))
        .await
        .unwrap()["row"]["id"]
        .clone();
    let body = json!({ "type": "doc", "content": [{ "type": "paragraph", "content": [
        { "type": "text", "text": "ask " },
        { "type": "mention", "attrs": { "uri": format!("user://{}", uid(&bob)), "label": "Bob" } }
    ]}]});
    k.call(
        &ada,
        "data.insert",
        json!({ "plugin": "notes", "collection": "notes", "space": s,
                "values": { "title": "second", "about": format!("note://{}", first.as_str().unwrap()), "body": body } }),
    )
    .await
    .unwrap();
    let backlinks = k
        .call(
            &bob,
            "data.refsTo",
            json!({ "uri": format!("note://{}", first.as_str().unwrap()) }),
        )
        .await
        .unwrap();
    assert_eq!(backlinks["backlinks"][0]["field"], "about");
    assert_eq!(backlinks["backlinks"][0]["title"], "second");
    let inbox =
        k.db.store
            .notify()
            .list(uid(&bob), false, None, 10)
            .await
            .unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].kind, "rookery/mention");
    assert_eq!(inbox[0].title, "Ada mentioned you in second");
    k.close().await;
}

async fn changed(inbox: &mut rookery_kernel::testing::Inbox) -> Value {
    tokio::time::timeout(Duration::from_secs(10), inbox.notification("data.changed"))
        .await
        .expect("a data.changed in time")
}

#[tokio::test]
async fn live_queries_send_each_change_as_the_subscriber_may_read_it() {
    let k: TestKernel = test_kernel!(methods());
    install(&k, notes()).await;
    let ada = k.person("Ada", false).await;
    let bob = k.person("Bob", false).await;
    let eve = k.person("Eve", false).await;
    let s = space(&k, &ada, Some((&bob, rookery_store::Level::View))).await;
    let at = json!({ "plugin": "notes", "collection": "notes", "space": s });
    k.call(&ada, "data.insert", json!({ "plugin": "notes", "collection": "notes", "space": s, "values": { "title": "before" } }))
        .await
        .unwrap();
    let mut inbox = k.connect(&bob);
    let started = k.call_on(&inbox, "data.live", at.clone()).await.unwrap();
    assert_eq!(titles(&started), ["before"]);
    // Someone outside the space does not follow it.
    let outsider = k.connect(&eve);
    assert_eq!(
        code(k.call_on(&outsider, "data.live", at.clone()).await),
        "FORBIDDEN"
    );
    let row = k
        .call(&ada, "data.insert", json!({ "plugin": "notes", "collection": "notes", "space": s, "values": { "title": "after" } }))
        .await
        .unwrap()["row"]
        .clone();
    let change = changed(&mut inbox).await;
    assert_eq!(change["subscription"], started["subscription"]);
    assert_eq!(change["changes"][0]["op"], "insert");
    assert_eq!(change["changes"][0]["row"]["title"], "after");
    k.call(
        &ada,
        "data.delete",
        json!({ "plugin": "notes", "collection": "notes", "space": s, "id": row["id"] }),
    )
    .await
    .unwrap();
    let change = changed(&mut inbox).await;
    assert_eq!(change["changes"][0]["op"], "delete");
    // A resumed query gets what it missed from the log.
    let mut again = k.connect(&bob);
    let mut resume = at.clone();
    resume["after"] = started["seq"].clone();
    k.call_on(&again, "data.live", resume).await.unwrap();
    // Changes read from the log carry the row as it is now: in the trash, so none.
    let missed = changed(&mut again).await;
    assert_eq!(missed["changes"][0]["op"], "insert");
    assert_eq!(missed["changes"][0]["id"], row["id"]);
    assert!(missed["changes"][0]["row"].is_null());
    assert_eq!(changed(&mut again).await["changes"][0]["op"], "delete");
    // A query ends with its connection.
    let running = rookery_data::live::running(&k.kernel);
    k.kernel.methods.connection_closed(&k.kernel, &inbox.conn);
    assert_eq!(rookery_data::live::running(&k.kernel), running - 1);
    k.close().await;
}

#[tokio::test]
async fn rookerys_own_data_is_read_like_a_plugins() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let rookery = |c: &str| json!({ "plugin": "rookery", "collection": c });
    let mut inbox = k.connect(&admin);
    let started = k
        .call_on(&inbox, "data.live", rookery("jobs"))
        .await
        .unwrap();
    assert_eq!(started["rows"], json!([]));
    let job =
        k.db.store
            .jobs()
            .enqueue(rookery_store::jobs::NewJob {
                kind: "apply",
                params: &json!({}),
                max_attempts: 1,
                run_after: None,
                subject: Some("env://studio"),
                enqueued_by: "system:rookery",
                token: None,
            })
            .await
            .unwrap();
    let change = changed(&mut inbox).await;
    assert_eq!(change["changes"][0]["id"], job.id.to_string());
    assert_eq!(change["changes"][0]["row"]["subject"], "env://studio");
    // A person sees the jobs on what they hold view on; they write none of them.
    let found = |c: Caller| {
        let k = &k;
        async move { k.call(&c, "data.count", rookery("jobs")).await.unwrap()["count"].clone() }
    };
    assert_eq!(found(ops.clone()).await, 0);
    k.db.store
        .access()
        .grant(
            "env://studio",
            &format!("user:{}", uid(&ops)),
            1,
            "system:test",
        )
        .await
        .unwrap();
    assert_eq!(found(ops.clone()).await, 1);
    let mut insert = rookery("jobs");
    insert["values"] = json!({ "kind": "x" });
    assert_eq!(
        code(k.call(&admin, "data.insert", insert).await),
        "FORBIDDEN"
    );
    // Storage is admins' through data.*, and every plugin's through storage.*.
    assert_eq!(
        code(k.call(&ops, "data.find", rookery("storage")).await),
        "FORBIDDEN"
    );
    k.db.store
        .kv()
        .set("fleet", None, "cursor", &json!(1), None)
        .await
        .unwrap();
    let stored = k
        .call(&admin, "data.find", rookery("storage"))
        .await
        .unwrap();
    assert_eq!(stored["rows"][0]["plugin"], "fleet");
    // A plugin does not reach Rookery's data through data.*.
    assert_eq!(
        code(
            k.call(
                &Caller::Plugin("fleet".into()),
                "data.find",
                rookery("storage")
            )
            .await
        ),
        "FORBIDDEN"
    );
    k.close().await;
}

/// The extension host's half of a transform, played by the test: it calls `data.tx*` as the plugin.
struct Upper(Kernel);

#[async_trait::async_trait]
impl Transforms for Upper {
    async fn run(&self, plugin: &str, name: &str, migration: &str) -> Result<(), RpcError> {
        assert_eq!((plugin, name), ("notes", "upper"));
        let call = |method: &'static str, params: Value| {
            let kernel = self.0.clone();
            async move {
                let call =
                    rookery_kernel::Call::new(kernel.clone(), Caller::Plugin("notes".into()), None);
                kernel.methods.dispatch(call, method, params).await
            }
        };
        let rows = call(
            "data.txRows",
            json!({ "migration": migration, "collection": "notes" }),
        )
        .await?;
        for row in rows["rows"].as_array().unwrap() {
            let heading = row["heading"].as_str().unwrap().to_uppercase();
            call(
                "data.txUpdate",
                json!({ "migration": migration, "collection": "notes", "id": row["id"], "patch": { "heading": heading } }),
            )
            .await?;
        }
        // Another plugin cannot reach this migration's transaction.
        let other = rookery_kernel::Call::new(self.0.clone(), Caller::Plugin("other".into()), None);
        let refused = self
            .0
            .methods
            .dispatch(
                other,
                "data.txRows",
                json!({ "migration": migration, "collection": "notes" }),
            )
            .await
            .unwrap_err();
        assert_eq!(refused.error_code(), rookery_proto::ErrorCode::NotFound);
        Ok(())
    }
}

#[tokio::test]
async fn migrations_rename_fields_and_run_transforms_in_one_transaction() {
    let k: TestKernel = test_kernel!(methods());
    install(&k, notes()).await;
    let ada = k.person("Ada", false).await;
    let s = space(&k, &ada, None).await;
    for title in ["one", "two"] {
        k.call(&ada, "data.insert", json!({ "plugin": "notes", "collection": "notes", "space": s, "values": { "title": title } }))
            .await
            .unwrap();
    }
    let mut renamed = notes();
    renamed[0]["fields"][0]["name"] = json!("heading");
    renamed[0]["entity"]["title"] = json!("heading");
    // Without the migration, the change is refused with what differs.
    let refused = plugins::apply(
        &k.kernel,
        &PluginData {
            plugin: "notes".into(),
            collections: renamed.clone(),
            ..Default::default()
        },
        &plugins::NoTransforms,
    )
    .await
    .unwrap_err();
    assert_eq!(refused.error_code(), rookery_proto::ErrorCode::Invalid);
    assert!(refused.details().unwrap()["differences"].is_array());
    let migrations = serde_json::from_value(json!([
        { "version": 1, "steps": [{ "op": "renameField", "collection": "notes", "from": "title", "to": "heading" }] },
        { "version": 2, "steps": [{ "op": "transform", "name": "upper" }] }
    ]))
    .unwrap();
    let data = PluginData {
        plugin: "notes".into(),
        collections: renamed,
        migrations,
        ..Default::default()
    };
    let applied = plugins::apply(&k.kernel, &data, &Upper(k.kernel.clone()))
        .await
        .unwrap();
    assert_eq!(applied.migrated, [1, 2]);
    let page = k
        .call(
            &ada,
            "data.find",
            json!({ "plugin": "notes", "collection": "notes", "space": s }),
        )
        .await
        .unwrap();
    let mut headings: Vec<&str> = page["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["heading"].as_str().unwrap())
        .collect();
    headings.sort();
    assert_eq!(headings, ["ONE", "TWO"]);
    // Applied again, nothing runs.
    let again = plugins::apply(&k.kernel, &data, &Upper(k.kernel.clone()))
        .await
        .unwrap();
    assert!(again.migrated.is_empty());
    // A transform that fails leaves nothing of its migration.
    struct Fails;
    #[async_trait::async_trait]
    impl Transforms for Fails {
        async fn run(&self, _: &str, _: &str, _: &str) -> Result<(), RpcError> {
            Err(RpcError::internal("the step threw"))
        }
    }
    let mut more = data.clone();
    more.migrations.push(
        serde_json::from_value(json!({ "version": 3, "steps": [
            { "op": "renameField", "collection": "notes", "from": "heading", "to": "name" },
            { "op": "transform", "name": "upper" }
        ]}))
        .unwrap(),
    );
    assert!(plugins::apply(&k.kernel, &more, &Fails).await.is_err());
    assert_eq!(k.db.store.collections().version("notes").await.unwrap(), 2);
    k.close().await;
}

#[test]
fn migrations_are_read_from_their_folder_in_order() {
    let dir = std::env::temp_dir().join(format!("rookery-migrations-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("migrations")).unwrap();
    std::fs::write(
        dir.join("migrations/0002.json"),
        r#"{ "steps": [{ "op": "transform", "name": "fill" }] }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("migrations/0001.json"),
        r#"{ "onInstall": true, "steps": [] }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("migrations/fill.ts"),
        "export default async () => {}",
    )
    .unwrap();
    let read = plugins::read_migrations(&dir).unwrap();
    assert_eq!(read.iter().map(|m| m.version).collect::<Vec<_>>(), [1, 2]);
    assert!(read[0].on_install);
    std::fs::write(dir.join("migrations/two.json"), "{}").unwrap();
    assert_eq!(
        plugins::read_migrations(&dir).unwrap_err().error_code(),
        rookery_proto::ErrorCode::Invalid
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rookerys_own_collections_are_not_written_through_data() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    for (method, params) in [
        (
            "data.insert",
            json!({ "plugin": "rookery", "collection": "storage", "values": { "plugin": "x", "key": "k" } }),
        ),
        (
            "data.import",
            json!({ "plugin": "rookery", "collection": "storage", "rows": [] }),
        ),
        ("data.batch", json!({ "plugin": "rookery", "ops": [] })),
    ] {
        assert_eq!(
            code(k.call(&admin, method, params).await),
            "FORBIDDEN",
            "{method}"
        );
    }
    k.close().await;
}

#[tokio::test]
async fn a_log_collection_is_appended_to_followed_and_kept_by_the_hourly_round() {
    let k: TestKernel = test_kernel!(methods());
    plugins::apply(
        &k.kernel,
        &PluginData {
            plugin: "ai".into(),
            collections: json!([{
                "name": "usage",
                "scope": "global",
                "kind": "log",
                "retention": { "days": 30 },
                "fields": [{ "name": "model", "type": "text" }, { "name": "tokens", "type": "integer" }]
            }]),
            ..Default::default()
        },
        &plugins::NoTransforms,
    )
    .await
    .unwrap();
    let ai = Caller::Plugin("ai".into());
    for tokens in [10, 20] {
        k.call(
            &ai,
            "data.insert",
            json!({ "collection": "usage", "values": { "model": "m", "tokens": tokens } }),
        )
        .await
        .unwrap();
    }
    // A log is only appended to.
    let first = k
        .call(
            &ai,
            "data.tail",
            json!({ "collection": "usage", "fromSeq": 0 }),
        )
        .await
        .unwrap();
    assert_eq!(first["rows"].as_array().unwrap().len(), 2);
    let id = first["rows"][0]["id"].clone();
    assert_eq!(
        code(
            k.call(
                &ai,
                "data.update",
                json!({ "collection": "usage", "id": id, "patch": { "tokens": 1 } })
            )
            .await
        ),
        "INVALID"
    );
    let sum = k
        .call(
            &ai,
            "data.aggregate",
            json!({ "collection": "usage", "groupBy": ["model"], "measures": { "total": { "sum": "tokens" } } }),
        )
        .await
        .unwrap();
    assert_eq!(sum["rows"][0]["total"], 30);
    // The round makes the coming partitions and trims nothing young.
    rookery_data::maintenance::once(&k.kernel).await;
    k.call(
        &ai,
        "data.insert",
        json!({ "collection": "usage", "values": { "model": "m", "tokens": 5 } }),
    )
    .await
    .unwrap();
    let after = k
        .call(
            &ai,
            "data.tail",
            json!({ "collection": "usage", "fromSeq": first["seq"] }),
        )
        .await
        .unwrap();
    assert_eq!(after["rows"].as_array().unwrap().len(), 1);
    k.close().await;
}

#[tokio::test]
async fn a_retried_write_with_its_idempotency_key_writes_once() {
    let k: TestKernel = test_kernel!(methods());
    install(&k, notes()).await;
    let notes = Caller::Plugin("notes".into());
    let insert = |key: &str, value: &str| json!({ "collection": "settings", "values": { "key": value }, "idempotencyKey": key });
    let first = k
        .call(&notes, "data.insert", insert("k1", "a"))
        .await
        .unwrap();
    let again = k
        .call(&notes, "data.insert", insert("k1", "a"))
        .await
        .unwrap();
    assert_eq!(
        first["row"]["id"], again["row"]["id"],
        "the repeat answers as the first"
    );
    let count = k
        .call(&notes, "data.count", json!({ "collection": "settings" }))
        .await
        .unwrap();
    assert_eq!(count["count"], 1);
    // The key with another request is refused; another caller's keys are its own.
    assert_eq!(
        code(k.call(&notes, "data.insert", insert("k1", "b")).await),
        "INVALID"
    );
    let admin = k.person("Maria", true).await;
    let mut theirs = insert("k1", "c");
    theirs["plugin"] = json!("notes");
    k.call(&admin, "data.insert", theirs).await.unwrap();
    let count = k
        .call(&notes, "data.count", json!({ "collection": "settings" }))
        .await
        .unwrap();
    assert_eq!(count["count"], 2);
    // A batch with a key too.
    let batch = json!({ "ops": [{ "op": "insert", "collection": "settings", "values": { "key": "d" } }], "idempotencyKey": "b1" });
    k.call(&notes, "data.batch", batch.clone()).await.unwrap();
    k.call(&notes, "data.batch", batch).await.unwrap();
    let count = k
        .call(&notes, "data.count", json!({ "collection": "settings" }))
        .await
        .unwrap();
    assert_eq!(count["count"], 3);
    k.close().await;
}
