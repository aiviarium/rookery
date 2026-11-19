//! Collection definitions: creating tables, additive changes, refusing the rest, numbered
//! plugin migrations.

use super::common;

use common::*;
use rookery_proto::ErrorCode;
use rookery_store::collections::{
    MigrationStep, MigrationTx, NoTransforms, PluginMigration, Query, Transformer,
};
use rookery_store::testing::TestDb;
use rookery_store::{Caller, Error};
use serde_json::json;

#[tokio::test]
async fn validation_rejects_bad_definitions() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let cases = [
        json!([{ "name": "x", "fields": [{ "name": "id", "type": "text" }] }]),
        json!([{ "name": "x", "fields": [{ "name": "a", "type": "text" }, { "name": "a", "type": "text" }] }]),
        json!([{ "name": "x", "fields": [{ "name": "a", "type": "enum", "values": [] }] }]),
        json!([{ "name": "x", "fields": [{ "name": "a", "type": "integer", "search": true }] }]),
        json!([{ "name": "x", "fields": [{ "name": "_a", "type": "text" }] }]),
        json!([{ "name": "x", "fields": [{ "name": "a\"; drop", "type": "text" }] }]),
        json!([{ "name": "x;y", "fields": [] }]),
        json!([{ "name": "x", "fields": [{ "name": "a", "type": "text" }], "indexes": [["b"]] }]),
        json!([{ "name": "x", "entity": { "scheme": "user", "title": "a" }, "fields": [{ "name": "a", "type": "text" }] }]),
        json!([{ "name": "x", "entity": { "scheme": "x", "title": "a", "parent": "a" }, "fields": [{ "name": "a", "type": "text" }] }]),
        json!([{ "name": "x", "scope": "global", "fields": [{ "name": "n", "type": "sequence", "per": "space" }] }]),
        json!([{ "name": "x", "fields": [{ "name": "a", "type": "enum", "values": ["a"], "default": "b" }] }]),
    ];
    for c in cases {
        let e = db
            .store
            .collections()
            .apply("bad", &defs(c.clone()))
            .await
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Invalid, "{c} gave {e}");
    }
    assert_eq!(
        db.store
            .collections()
            .apply("Bad_Id", &[])
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Invalid
    );
}

#[tokio::test]
async fn additive_changes_apply_and_others_are_refused() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let report = db
        .store
        .collections()
        .apply("issues", &issues_defs())
        .await
        .unwrap();
    assert_eq!(report.created.len(), 4);
    // Applying the same manifest again does nothing.
    let again = db
        .store
        .collections()
        .apply("issues", &issues_defs())
        .await
        .unwrap();
    assert!(again.created.is_empty() && again.altered.is_empty());

    let u = db.user("u").await;
    let space = db.store.spaces().personal(u).await.unwrap().id;
    let c = Caller::user(u).in_space(space);
    let data = db.store.data("issues");
    let row = data
        .insert(
            &c,
            "issues",
            json!({ "title": "Old row", "body": doc_with_mentions("hello world", &[]) }),
        )
        .await
        .unwrap()
        .row
        .unwrap();

    // Add a field, an index, a unique field, an enum value, search on an existing field.
    let mut v = serde_json::to_value(issues_defs()).unwrap();
    let issues = v[1].as_object_mut().unwrap();
    let fields = issues["fields"].as_array_mut().unwrap();
    fields.push(json!({ "name": "priority", "type": "integer", "index": true }));
    fields.push(json!({ "name": "slug", "type": "text", "unique": true }));
    fields[2]["values"] = json!(["open", "done", "wontfix"]);
    fields[6]["search"] = json!(false);
    fields.push(json!({ "name": "notes", "type": "longtext", "search": true }));
    issues["indexes"] = json!([["state", "assignee"], ["priority", "state"]]);
    let report = db
        .store
        .collections()
        .apply("issues", &defs(v.clone()))
        .await
        .unwrap();
    assert!(
        report
            .altered
            .iter()
            .any(|a| a == "issues: add field priority"),
        "{:?}",
        report.altered
    );
    assert!(
        report
            .altered
            .iter()
            .any(|a| a.starts_with("issues: create index"))
    );

    // The old row is still there, with the new fields null, and new values work.
    let got = data
        .get(&c, "issues", row["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got["priority"], json!(null));
    data.insert(
        &c,
        "issues",
        json!({ "title": "New", "priority": 2, "state": "wontfix", "notes": "searchable notes" }),
    )
    .await
    .unwrap();
    let hits = data
        .search(&c, "issues", "searchable", None, 10)
        .await
        .unwrap();
    assert_eq!(hits.rows.len(), 1);
    // Search over the reindexed old row still finds its title.
    assert_eq!(
        data.search(&c, "issues", "old", None, 10)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    let idx: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes WHERE schemaname = 'p_issues' AND tablename = 'issues'",
    )
    .fetch_one(db.store.pool())
    .await
    .unwrap();
    assert!(idx >= 8, "{idx} indexes");

    // Non-additive changes are refused with every difference listed, and nothing changes.
    let mut bad = v.clone();
    let bf = bad[1]["fields"].as_array_mut().unwrap();
    bf[6]["type"] = json!("text"); // estimate: number -> text
    bf.retain(|f| f["name"] != "due"); // removed field
    bf[2]["values"] = json!(["open"]); // enum values removed
    bad[0]["scope"] = json!("global");
    bad.as_array_mut()
        .unwrap()
        .retain(|c| c["name"] != "drafts");
    let e = db
        .store
        .collections()
        .apply("issues", &defs(bad))
        .await
        .unwrap_err();
    let Error::Schema(diffs) = &e else {
        panic!("expected a schema error, got {e}")
    };
    let text: Vec<String> = diffs.iter().map(|d| d.to_string()).collect();
    assert!(
        text.contains(
            &"issues.estimate: type changed from number to text; change it with a migration"
                .to_string()
        ),
        "{text:?}"
    );
    assert!(
        text.contains(&"issues.due: was removed; drop it with a migration".to_string()),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|t| t.starts_with("issues.state: enum values were removed (done, wontfix)")),
        "{text:?}"
    );
    assert!(
        text.contains(&"projects: scope changed from space to global".to_string()),
        "{text:?}"
    );
    assert!(
        text.contains(&"drafts: was removed; drop it with a migration".to_string()),
        "{text:?}"
    );
    assert_eq!(e.code(), ErrorCode::Invalid);
    assert!(
        e.details().unwrap()["differences"]
            .as_array()
            .unwrap()
            .len()
            >= 5
    );
    assert_eq!(
        db.store.collections().get("issues", "issues").unwrap().def,
        defs(v.clone())[1]
    );

    // A unique constraint the data violates fails as a conflict and leaves things as they were.
    let mut dup = v.clone();
    dup[1]["fields"].as_array_mut().unwrap()[1]["unique"] = json!(true); // title
    data.insert(&c, "issues", json!({ "title": "New" }))
        .await
        .unwrap();
    assert_eq!(
        db.store
            .collections()
            .apply("issues", &defs(dup))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn schemes_are_unique_across_plugins() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    db.store
        .collections()
        .apply("issues", &issues_defs())
        .await
        .unwrap();
    let clash = defs(
        json!([{ "name": "tickets", "entity": { "scheme": "issue", "title": "t" }, "fields": [{ "name": "t", "type": "text" }] }]),
    );
    assert_eq!(
        db.store
            .collections()
            .apply("tickets", &clash)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
}

struct Upper;

#[async_trait::async_trait]
impl Transformer for Upper {
    async fn transform(&self, name: &str, tx: &mut MigrationTx<'_>) -> rookery_store::Result<()> {
        assert_eq!(name, "upper");
        let mut after = None;
        loop {
            let rows = tx.rows("issues", after, 2).await?;
            let Some(last) = rows.last() else { break };
            after = Some(last["id"].as_str().unwrap().parse().unwrap());
            for r in rows {
                let t = r["headline"].as_str().unwrap_or_default().to_uppercase();
                tx.update(
                    "issues",
                    r["id"].as_str().unwrap().parse().unwrap(),
                    json!({ "headline": t }),
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn numbered_migrations_rename_drop_change_and_transform() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    db.store
        .collections()
        .apply("issues", &issues_defs())
        .await
        .unwrap();
    let u = db.user("u").await;
    let space = db.store.spaces().personal(u).await.unwrap().id;
    let c = Caller::user(u).in_space(space);
    let data = db.store.data("issues");
    for t in ["one", "two", "three"] {
        data.insert(
            &c,
            "issues",
            json!({ "title": t, "estimate": 2.0, "labels": ["label://a"] }),
        )
        .await
        .unwrap();
    }
    let mut sub = db.store.feed().subscribe().await.unwrap();

    let m2 = PluginMigration {
        version: 2,
        on_install: false,
        steps: vec![
            MigrationStep::RenameField {
                collection: "issues".into(),
                from: "title".into(),
                to: "headline".into(),
            },
            MigrationStep::DropField {
                collection: "issues".into(),
                field: "due".into(),
            },
            MigrationStep::ChangeType {
                collection: "issues".into(),
                field: Box::new(
                    serde_json::from_value(json!({ "name": "estimate", "type": "integer" }))
                        .unwrap(),
                ),
            },
            MigrationStep::RenameCollection {
                from: "comments".into(),
                to: "notes".into(),
            },
            MigrationStep::DropCollection {
                collection: "drafts".into(),
            },
            MigrationStep::Transform {
                name: "upper".into(),
            },
        ],
    };
    let applied = db
        .store
        .collections()
        .migrate("issues", std::slice::from_ref(&m2), &Upper)
        .await
        .unwrap();
    assert_eq!(applied, vec![2]);
    assert_eq!(db.store.collections().version("issues").await.unwrap(), 2);
    // Running again does nothing.
    assert!(
        db.store
            .collections()
            .migrate("issues", &[m2], &NoTransforms)
            .await
            .unwrap()
            .is_empty()
    );

    let coll = db.store.collections().get("issues", "issues").unwrap();
    assert!(coll.def.field("title").is_none() && coll.def.field("due").is_none());
    assert_eq!(coll.def.entity.as_ref().unwrap().title, "headline");
    let rows = data
        .find(&c, "issues", &Query::default())
        .await
        .unwrap()
        .rows;
    assert_eq!(
        rows.iter()
            .map(|r| r["headline"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ONE", "TWO", "THREE"]
    );
    assert_eq!(rows[0]["estimate"], json!(2));
    assert!(rows[0].get("due").is_none());
    // Transform writes went through the write path: entity titles and the feed follow.
    let e = db
        .store
        .entities()
        .get(
            &c,
            &coll
                .entity_uri(rows[0]["id"].as_str().unwrap().parse().unwrap())
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e.title, "ONE");
    let mut seen = 0;
    while seen < 3 {
        if let Some(rookery_store::feed::FeedEvent::Change(ch)) = sub.next().await.unwrap() {
            assert_eq!(ch.op, rookery_store::feed::ChangeOp::Update);
            seen += 1;
        }
    }
    // References keep working under the renamed field names.
    assert_eq!(db.store.refs().to(&c, "label://a").await.unwrap().len(), 3);
    assert!(db.store.collections().get("issues", "notes").is_ok());
    assert!(db.store.collections().get("issues", "drafts").is_err());

    // The manifest that matches the migrated schema applies cleanly; the old one is refused.
    let mut v = serde_json::to_value(issues_defs()).unwrap();
    let issues = v[1].as_object_mut().unwrap();
    issues["entity"]["title"] = json!("headline");
    let f = issues["fields"].as_array_mut().unwrap();
    f[1]["name"] = json!("headline");
    f[6]["type"] = json!("integer");
    f.retain(|x| x["name"] != "due");
    v[2]["name"] = json!("notes");
    v.as_array_mut().unwrap().retain(|c| c["name"] != "drafts");
    db.store
        .collections()
        .apply("issues", &defs(v))
        .await
        .unwrap();
    assert_eq!(
        db.store
            .collections()
            .apply("issues", &issues_defs())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Invalid
    );

    // A failing migration rolls back entirely.
    let bad = PluginMigration {
        version: 3,
        on_install: false,
        steps: vec![
            MigrationStep::RenameField {
                collection: "issues".into(),
                from: "headline".into(),
                to: "name".into(),
            },
            MigrationStep::ChangeType {
                collection: "issues".into(),
                field: Box::new(
                    serde_json::from_value(json!({ "name": "name", "type": "integer" })).unwrap(),
                ),
            },
        ],
    };
    assert!(
        db.store
            .collections()
            .migrate("issues", &[bad], &NoTransforms)
            .await
            .is_err()
    );
    assert_eq!(db.store.collections().version("issues").await.unwrap(), 2);
    assert!(
        db.store
            .collections()
            .get("issues", "issues")
            .unwrap()
            .def
            .field("headline")
            .is_some()
    );
}

#[tokio::test]
async fn becoming_an_entity_collection_indexes_existing_rows_and_trash_can_be_purged() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let plain = defs(json!([{ "name": "notes", "trash": true, "fields": [
        { "name": "title", "type": "text" }, { "name": "link", "type": "ref" }
    ] }]));
    db.store.collections().apply("notes", &plain).await.unwrap();
    let u = db.user("u").await;
    let space = db.store.spaces().personal(u).await.unwrap().id;
    let c = Caller::user(u).in_space(space);
    let data = db.store.data("notes");
    let keep = data
        .insert(
            &c,
            "notes",
            json!({ "title": "Keep me", "link": "env://a" }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let gone = data
        .insert(&c, "notes", json!({ "title": "Bin me" }))
        .await
        .unwrap()
        .row
        .unwrap();
    data.delete(&c, "notes", gone["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap();
    assert_eq!(
        db.store.refs().to(&c, "env://a").await.unwrap()[0].source,
        format!("row://notes/notes/{}", keep["id"].as_str().unwrap())
    );

    // Turning on `entity` and `search` reindexes the rows that are already there.
    let mut v = serde_json::to_value(&plain).unwrap();
    v[0]["entity"] = json!({ "scheme": "note", "title": "title" });
    v[0]["fields"][0]["search"] = json!(true);
    let report = db
        .store
        .collections()
        .apply("notes", &defs(v.clone()))
        .await
        .unwrap();
    assert!(
        report
            .altered
            .iter()
            .any(|a| a.contains("became an entity collection")),
        "{:?}",
        report.altered
    );
    let keep_uri = format!("note://{}", keep["id"].as_str().unwrap());
    assert_eq!(
        db.store
            .entities()
            .get(&c, &keep_uri)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Keep me"
    );
    let gone_entity = db
        .store
        .entities()
        .get(&c, &format!("note://{}", gone["id"].as_str().unwrap()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        gone_entity.deleted_at.is_some(),
        "a trashed row is a trashed entity"
    );
    assert_eq!(
        data.search(&c, "notes", "keep", None, 5)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(
        db.store
            .refs()
            .to(&c, "env://a")
            .await
            .unwrap()
            .iter()
            .any(|b| b.source == keep_uri)
    );

    // Turning trash off needs the trash emptied first.
    v[0]["trash"] = json!(false);
    let e = db
        .store
        .collections()
        .apply("notes", &defs(v.clone()))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("purgeTrash"), "{e}");
    let m = PluginMigration {
        version: 1,
        on_install: false,
        steps: vec![MigrationStep::PurgeTrash {
            collection: "notes".into(),
        }],
    };
    db.store
        .collections()
        .migrate("notes", &[m], &NoTransforms)
        .await
        .unwrap();
    db.store
        .collections()
        .apply("notes", &defs(v))
        .await
        .unwrap();
    assert_eq!(data.count(&c, "notes", &Query::default()).await.unwrap(), 1);
    assert!(
        db.store
            .entities()
            .get(&c, &format!("note://{}", gone["id"].as_str().unwrap()))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_first_install_starts_at_the_latest_migration_without_running_them() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let c = db.store.collections();
    // Nothing stored yet: the manifest already is what the migrations lead to.
    assert!(c.baseline("issues", 3).await.unwrap());
    assert_eq!(c.version("issues").await.unwrap(), 3);
    c.apply("issues", &issues_defs()).await.unwrap();
    assert_eq!(
        c.version("issues").await.unwrap(),
        3,
        "apply keeps the version"
    );
    // Migrations up to 3 are not run again; later ones are.
    let step = |v: i64| PluginMigration {
        version: v,
        on_install: false,
        steps: vec![MigrationStep::Transform {
            name: "nope".into(),
        }],
    };
    assert!(
        c.migrate("issues", &[step(2), step(3)], &NoTransforms)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        c.migrate("issues", &[step(4)], &NoTransforms)
            .await
            .is_err()
    );
    // A plugin with a stored schema is not baselined.
    assert!(!c.baseline("issues", 9).await.unwrap());
    assert_eq!(c.version("issues").await.unwrap(), 3);
    assert!(!c.baseline("other", 0).await.unwrap());
}

/// Plugins load at the same time, and each apply ends by reloading the definitions of every plugin. A
/// reload that read the table before another plugin's apply committed must not write its older view
/// over the newer one: that left the kernel without a plugin's collections ("plugin notes has no
/// collection notes") until something reloaded again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_applies_keep_every_plugins_collections() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    for round in 0..20 {
        let names: Vec<String> = (0..12).map(|i| format!("p{round}x{i}")).collect();
        let mut tasks = Vec::new();
        for name in &names {
            let store = db.store.clone();
            let name = name.clone();
            tasks.push(tokio::spawn(async move {
                let d = defs(
                    json!([{ "name": "things", "fields": [{ "name": "title", "type": "text" }] }]),
                );
                store.collections().apply(&name, &d).await.unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        for name in &names {
            assert!(
                db.store.collections().get(name, "things").is_ok(),
                "round {round}: {name}'s collection went missing"
            );
        }
    }
}

#[tokio::test]
async fn adding_constraints_and_transitions_preserves_existing_rows() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let c = Caller::plugin("test").as_admin();
    let v1 = defs(json!([{
        "name": "deals",
        "scope": "global",
        "fields": [
            { "name": "title", "type": "text" },
            { "name": "amount", "type": "number" },
            { "name": "state", "type": "enum", "values": ["open", "review", "won"] }
        ]
    }]));
    db.store.collections().apply("crmfix", &v1).await.unwrap();
    let data = db.store.data("crmfix");
    let legacy1 = data
        .insert(
            &c,
            "deals",
            json!({ "title": "Very long legacy deal name", "amount": -50, "state": "open" }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let legacy2 = data
        .insert(
            &c,
            "deals",
            json!({ "title": null, "amount": null, "state": "won" }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let id1 = legacy1["id"].as_str().unwrap().parse().unwrap();
    let id2 = legacy2["id"].as_str().unwrap().parse().unwrap();

    // Add constraints, metadata (title/tone/format), and enum transitions on an already-populated collection.
    let v2 = defs(json!([{
        "name": "deals",
        "scope": "global",
        "fields": [
            { "name": "title", "type": "text", "title": "Deal title", "required": true, "minLength": 2, "maxLength": 10 },
            { "name": "amount", "type": "number", "format": "currency", "min": 0 },
            {
                "name": "state",
                "type": "enum",
                "values": [
                    { "value": "open", "title": "Open", "tone": "neutral" },
                    { "value": "review", "title": "Review", "tone": "accent" },
                    { "value": "won", "title": "Won", "tone": "positive" }
                ],
                "transitions": { "open": ["review"], "review": ["won"] }
            }
        ]
    }]));
    db.store.collections().apply("crmfix", &v2).await.unwrap();

    // Existing rows are untouched and readable.
    let got1 = data.get(&c, "deals", id1).await.unwrap().unwrap();
    assert_eq!(got1["title"], json!("Very long legacy deal name"));
    assert_eq!(got1["amount"], json!(-50));
    let got2 = data.get(&c, "deals", id2).await.unwrap().unwrap();
    assert_eq!(got2["title"], json!(null));

    // Partial update on an existing row only validates the fields in the patch.
    let updated = data
        .update(&c, "deals", id1, json!({ "state": "review" }), None)
        .await
        .unwrap()
        .row
        .unwrap();
    assert_eq!(updated["state"], json!("review"));
    assert_eq!(updated["amount"], json!(-50));

    // Subsequent writes violating new constraints/transitions fail with INVALID + details.
    let err = data
        .update(&c, "deals", id1, json!({ "amount": -10 }), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Invalid);
    assert_eq!(
        err.details(),
        Some(json!({ "collection": "deals", "field": "amount", "constraint": "min" }))
    );

    let err = data
        .update(&c, "deals", id2, json!({ "state": "open" }), None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Invalid);
    assert_eq!(
        err.details(),
        Some(json!({ "collection": "deals", "field": "state", "constraint": "transitions" }))
    );
}
