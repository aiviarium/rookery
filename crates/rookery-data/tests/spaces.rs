//! `spaces.*`, `entities.*`, `search.query`, `recents.*`, `pins.*` and `activity.recent` through the
//! methods, as the people of a space and outside it call them.

use std::time::Duration;

use rookery_data::plugins::{self, PluginData};
use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Caller, Methods, RpcError, test_kernel};
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

fn uid(caller: &Caller) -> String {
    caller.user().unwrap().id.clone()
}

async fn install(k: &TestKernel) {
    plugins::apply(
        &k.kernel,
        &PluginData {
            plugin: "notes".into(),
            collections: json!([{
                "name": "notes",
                "scope": "space",
                "entity": { "scheme": "note", "title": "title" },
                "fields": [
                    { "name": "title", "type": "text", "search": true },
                    { "name": "body", "type": "longtext", "search": true },
                    { "name": "state", "type": "enum", "values": [{ "value": "open", "title": "Open" }, "done"] }
                ]
            }]),
            ..Default::default()
        },
        &plugins::NoTransforms,
    )
    .await
    .unwrap();
    rookery_data::spaces::set_kinds(
        &k.kernel,
        "notes",
        vec![rookery_proto::data::SpaceKindInfo {
            id: "notes.team".into(),
            plugin: "notes".into(),
            title: "Team".into(),
            icon: None,
            description: None,
        }],
    );
}

#[tokio::test]
async fn spaces_are_made_entered_shared_and_hidden_from_who_is_not_in_them() {
    let k: TestKernel = test_kernel!(methods());
    install(&k).await;
    let ada = k.person("Ada", false).await;
    let bob = k.person("Bob", false).await;
    // Everyone has a personal space, `~` to themselves.
    let mine = k
        .call(&ada, "spaces.get", json!({ "space": "~" }))
        .await
        .unwrap();
    assert_eq!(
        (mine["personal"].clone(), mine["slug"].clone()),
        (json!(true), json!("~"))
    );
    assert_eq!(
        code(
            k.call(
                &ada,
                "spaces.create",
                json!({ "kind": "nobody.knows", "name": "X" })
            )
            .await
        ),
        "INVALID"
    );
    let team = k
        .call(
            &ada,
            "spaces.create",
            json!({ "kind": "notes.team", "name": "Platform" }),
        )
        .await
        .unwrap();
    assert_eq!(team["level"], "manage");
    let id = team["id"].as_str().unwrap().to_owned();
    // Someone outside does not see it: by id it is forbidden, by slug it does not exist.
    assert_eq!(
        code(k.call(&bob, "spaces.get", json!({ "space": id })).await),
        "FORBIDDEN"
    );
    assert_eq!(
        code(
            k.call(&bob, "spaces.get", json!({ "space": team["slug"] }))
                .await
        ),
        "NOT_FOUND"
    );
    let listed = k.call(&bob, "spaces.list", json!({})).await.unwrap();
    assert_eq!(
        listed["spaces"].as_array().unwrap().len(),
        1,
        "only Bob's own"
    );
    // Membership is a grant on the space.
    k.db.store
        .access()
        .grant(
            &format!("space://{id}"),
            &format!("user:{}", uid(&bob)),
            1,
            "system:test",
        )
        .await
        .unwrap();
    assert_eq!(
        k.call(&bob, "spaces.get", json!({ "space": id }))
            .await
            .unwrap()["level"],
        "view"
    );
    assert_eq!(
        code(
            k.call(
                &bob,
                "spaces.rename",
                json!({ "space": id, "name": "Mine" })
            )
            .await
        ),
        "FORBIDDEN"
    );
    k.call(
        &ada,
        "spaces.rename",
        json!({ "space": id, "name": "Core" }),
    )
    .await
    .unwrap();
    // A connection that entered the space uses it for calls that name none.
    let conn = k.connect(&ada);
    k.call_on(&conn, "spaces.enter", json!({ "space": id }))
        .await
        .unwrap();
    k.call_on(
        &conn,
        "data.insert",
        json!({ "plugin": "notes", "collection": "notes", "values": { "title": "in core" } }),
    )
    .await
    .unwrap();
    let found = k
        .call(
            &bob,
            "data.find",
            json!({ "plugin": "notes", "collection": "notes", "space": id }),
        )
        .await
        .unwrap();
    assert_eq!(found["rows"][0]["title"], "in core");
    k.close().await;
}

#[tokio::test]
async fn entities_search_text_recents_pins_and_activity_show_what_the_caller_may_see() {
    let k: TestKernel = test_kernel!(methods());
    install(&k).await;
    let ada = k.person("Ada", false).await;
    let bob = k.person("Bob", false).await;
    let team = k
        .call(
            &ada,
            "spaces.create",
            json!({ "kind": "notes.team", "name": "Platform" }),
        )
        .await
        .unwrap();
    let id = team["id"].as_str().unwrap().to_owned();
    let insert = |title: &str, body: &str| {
        json!({ "plugin": "notes", "collection": "notes", "space": id,
                "values": { "title": title, "body": body, "state": "open" } })
    };
    let row = k
        .call(
            &ada,
            "data.insert",
            insert("Deploy plan", "roll out on friday"),
        )
        .await
        .unwrap()["row"]
        .clone();
    let uri = format!("note://{}", row["id"].as_str().unwrap());
    // Ada resolves and finds it; Bob, outside the space, does not.
    assert_eq!(
        k.call(&ada, "entities.resolve", json!({ "uri": uri }))
            .await
            .unwrap()["title"],
        "Deploy plan"
    );
    assert_eq!(
        k.call(&bob, "entities.resolve", json!({ "uri": uri }))
            .await
            .unwrap(),
        Value::Null
    );
    let hits = k
        .call(
            &ada,
            "entities.search",
            json!({ "text": "depl", "schemes": ["note"] }),
        )
        .await
        .unwrap();
    assert_eq!(hits["entities"][0]["uri"], uri);
    let none = k
        .call(&bob, "entities.search", json!({ "text": "depl" }))
        .await
        .unwrap();
    assert!(
        none["entities"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["uri"] != uri)
    );
    let search = k
        .call(
            &ada,
            "search.query",
            json!({ "text": "friday", "facets": true }),
        )
        .await
        .unwrap();
    assert_eq!(search["hits"][0]["entity"]["uri"], uri);
    assert_eq!(search["facets"][0]["scheme"], "note");
    // Its text, as Markdown from its fields; nothing for Bob.
    let text = k
        .call(&ada, "entities.text", json!({ "uris": [uri] }))
        .await
        .unwrap();
    let body = text["texts"][0]["text"].as_str().unwrap();
    assert!(body.starts_with("# Deploy plan"), "{body}");
    assert!(body.contains("- state: Open"), "{body}");
    assert!(body.contains("roll out on friday"), "{body}");
    let text = k
        .call(&bob, "entities.text", json!({ "uris": [uri] }))
        .await
        .unwrap();
    assert_eq!(text["missing"], json!([uri]));
    // Recents keep what a person opened and may see.
    assert_eq!(
        k.call(
            &ada,
            "recents.touch",
            json!({ "kind": "entity", "key": uri })
        )
        .await
        .unwrap(),
        json!({ "recorded": true })
    );
    assert_eq!(
        k.call(
            &bob,
            "recents.touch",
            json!({ "kind": "entity", "key": uri })
        )
        .await
        .unwrap(),
        json!({ "recorded": false })
    );
    let recent = k
        .call(&ada, "recents.list", json!({ "kind": "entity" }))
        .await
        .unwrap();
    assert_eq!(recent["entities"][0]["entity"]["uri"], uri);
    // A pin of one's own, and one shared in the space, which tells the space's connections.
    let mut inbox = k.connect(&ada);
    let pin = k
        .call(
            &ada,
            "pins.add",
            json!({ "kind": "entity", "target": uri, "sharedIn": id }),
        )
        .await
        .unwrap();
    let told = tokio::time::timeout(Duration::from_secs(5), inbox.notification("pins.changed"))
        .await
        .unwrap();
    assert_eq!(told["spaceId"], id);
    let pins = k
        .call(&ada, "pins.list", json!({ "space": id }))
        .await
        .unwrap();
    assert_eq!(pins["pins"][0]["id"], pin["id"]);
    assert_eq!(
        code(k.call(&bob, "pins.list", json!({ "space": id })).await),
        "FORBIDDEN"
    );
    // What changed lately, in the spaces the caller is in.
    let activity = k
        .call(&ada, "activity.recent", json!({ "space": id }))
        .await
        .unwrap();
    assert_eq!(activity["items"][0]["entity"]["uri"], uri);
    assert_eq!(
        code(
            k.call(&bob, "activity.recent", json!({ "space": id }))
                .await
        ),
        "FORBIDDEN"
    );
    k.close().await;
}
