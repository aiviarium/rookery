//! Review: who learns people's emails.
use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Caller, Methods, test_kernel};
use serde_json::json;

fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_facilities::register(&mut methods);
    rookery_data::register(&mut methods);
    methods
}

#[tokio::test]
async fn attack_any_member_lists_everyones_email() {
    let k: TestKernel = test_kernel!(methods());
    let eve = k.person("Eve", false).await;
    let _bob = k.person("Bob", false).await;
    let me = format!("user://{}", eve.user().unwrap().id);
    let r = k
        .call(&eve, "users.search", json!({ "resource": me, "text": "b" }))
        .await;
    println!("users.search on own user://: {r:?}");
    let r2 = k
        .call(&eve, "entities.search", json!({ "text": "bob" }))
        .await;
    println!("entities.search: {r2:?}");
    let plugin = Caller::Plugin("evil".into());
    let r3 = k
        .call(&plugin, "entities.search", json!({ "text": "example" }))
        .await;
    println!("plugin entities.search: {r3:?}");
    let leaked = format!("{r:?}{r2:?}{r3:?}").contains("bob@example.test");
    assert!(!leaked, "LEAK: Eve or a plugin learns Bob's email");
    k.close().await;
}

#[tokio::test]
async fn personal_spaces_allow_name_search_without_email_lookup() {
    let k: TestKernel = test_kernel!(methods());
    let ada = k.person("Ada", false).await;
    let _bo = k.person("Bo", false).await;
    let id = ada.user().unwrap().id.parse().unwrap();
    let space = k.db.store.spaces().personal(id).await.unwrap();
    let personal = format!("space://{}", space.id);
    let found = k
        .call(
            &ada,
            "users.search",
            json!({"resource": personal, "text": "Bo"}),
        )
        .await
        .unwrap();
    assert_eq!(found["people"][0]["name"], "Bo");
    for text in ["bo@example.test", "nobody@example.test"] {
        assert_eq!(
            k.call(
                &ada,
                "users.search",
                json!({"resource": personal, "text": text})
            )
            .await
            .unwrap(),
            json!({"people": []})
        );
    }
    k.close().await;
}
