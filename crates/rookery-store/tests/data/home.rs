//! Search, recents, pins and recent activity: what the command palette, the search view, the sidebar and the
//! home screen read.

use super::common;

use common::*;
use rookery_store::Caller;
use rookery_store::entities::EntitySearch;
use rookery_store::pins::{NewPin, PinKind};
use rookery_store::search::SearchQuery;
use serde_json::{Value, json};
use uuid::Uuid;

fn id(row: &Value) -> Uuid {
    row["id"].as_str().unwrap().parse().unwrap()
}

async fn issue(w: &World, caller: &Caller, title: &str, body: &str) -> Uuid {
    let row = w
        .data()
        .insert(
            caller,
            "issues",
            json!({ "title": title, "body": doc_with_mentions(body, &[]) }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    id(&row)
}

#[tokio::test]
async fn search_finds_titles_and_text_hides_what_the_caller_may_not_see_and_counts_types() {
    let Some(w) = World::new().await else { return };
    w.db.store
        .collections()
        .apply("docs", &docs_defs())
        .await
        .unwrap();
    let owner = w.as_user(w.owner);
    let a = issue(
        &w,
        &owner,
        "Fix the login redirect",
        "Users land on a blank page",
    )
    .await;
    issue(&w, &owner, "Unrelated", "Mentions a blank page too").await;
    w.db.store
        .data("docs")
        .insert(&owner, "pages", json!({ "title": "Login runbook" }))
        .await
        .unwrap();

    let search = w.db.store.search();
    let q = |text: &str| SearchQuery {
        text: text.into(),
        facets: true,
        ..Default::default()
    };
    // By a prefix of a title word.
    let r = search.query(&owner, &q("logi")).await.unwrap();
    let uris: Vec<_> = r.hits.iter().map(|h| h.entity.uri.clone()).collect();
    assert_eq!(r.hits.len(), 2, "{uris:?}");
    assert!(uris.contains(&format!("issue://{a}")));
    assert!(r.hits.iter().all(|h| h.title_match));
    let facets = r.facets.unwrap();
    assert_eq!(
        facets
            .iter()
            .map(|f| (f.scheme.as_str(), f.count))
            .collect::<Vec<_>>(),
        vec![("doc", 1), ("issue", 1)]
    );
    // By text in the body, which the title does not contain.
    let r = search.query(&owner, &q("blank page")).await.unwrap();
    assert_eq!(r.hits.len(), 2);
    assert!(r.hits.iter().all(|h| !h.title_match));
    // Narrowed to one type; pages through.
    let only = SearchQuery {
        schemes: Some(vec!["issue".into()]),
        ..q("blank")
    };
    assert_eq!(search.query(&owner, &only).await.unwrap().hits.len(), 2);
    let page = SearchQuery {
        limit: Some(1),
        ..only.clone()
    };
    let first = search.query(&owner, &page).await.unwrap();
    assert!(first.more);
    let second = search
        .query(
            &owner,
            &SearchQuery {
                offset: Some(1),
                ..page
            },
        )
        .await
        .unwrap();
    assert!(!second.more);
    assert_ne!(first.hits[0].entity.uri, second.hits[0].entity.uri);

    // A member of the space sees them; someone outside it sees nothing, not even counts.
    let member = w.as_user(w.member);
    assert_eq!(
        search.query(&member, &q("logi")).await.unwrap().hits.len(),
        2
    );
    let outsider = w.as_user(w.outsider);
    let r = search.query(&outsider, &q("logi")).await.unwrap();
    assert!(r.hits.is_empty());
    assert!(r.facets.unwrap().is_empty());
    // A trashed entity drops out.
    w.data().delete(&owner, "issues", a).await.unwrap();
    assert_eq!(
        search.query(&owner, &q("logi")).await.unwrap().hits.len(),
        1
    );
}

#[tokio::test]
async fn empty_search_lists_what_the_user_opened_then_what_changed_and_respects_permissions() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    let first = issue(&w, &owner, "First", "").await;
    let second = issue(&w, &owner, "Second", "").await;
    let third = issue(&w, &owner, "Third", "").await;
    let store = &w.db.store;

    // Nothing opened yet: newest change first.
    let r = store
        .search()
        .query(&owner, &SearchQuery::default())
        .await
        .unwrap();
    let titles: Vec<_> = r.hits.iter().map(|h| h.entity.title.as_str()).collect();
    assert_eq!(&titles[..3], ["Third", "Second", "First"]);
    assert!(r.hits.iter().all(|h| !h.recent));

    // Opened ones lead, the latest first.
    assert!(
        store
            .recents()
            .touch_entity(&owner, &format!("issue://{first}"))
            .await
            .unwrap()
    );
    assert!(
        store
            .recents()
            .touch_entity(&owner, &format!("issue://{second}"))
            .await
            .unwrap()
    );
    store
        .recents()
        .touch_entity(&owner, &format!("issue://{first}"))
        .await
        .unwrap();
    let r = store
        .search()
        .query(&owner, &SearchQuery::default())
        .await
        .unwrap();
    let titles: Vec<_> = r.hits.iter().map(|h| h.entity.title.as_str()).collect();
    assert_eq!(&titles[..3], ["First", "Second", "Third"]);
    assert_eq!(
        r.hits.iter().map(|h| h.recent).collect::<Vec<_>>()[..3],
        [true, true, false]
    );
    let _ = third;

    // The entity search the mention picker calls does the same with no text.
    let hits = store
        .entities()
        .search(
            &owner,
            &EntitySearch {
                text: "  ".into(),
                schemes: Some(vec!["issue".into()]),
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|h| h.entity.title.as_str())
            .collect::<Vec<_>>(),
        ["First", "Second"]
    );

    // Someone who may not view the space gets neither, and cannot record what they cannot see.
    let outsider = w.as_user(w.outsider);
    assert!(
        !store
            .recents()
            .touch_entity(&outsider, &format!("issue://{first}"))
            .await
            .unwrap()
    );
    assert!(
        store
            .search()
            .query(&outsider, &SearchQuery::default())
            .await
            .unwrap()
            .hits
            .iter()
            .all(|h| h.entity.scheme != "issue")
    );
    // Recents are per user: the member has opened nothing.
    let member = w.as_user(w.member);
    assert!(
        store
            .recents()
            .entities(&member, None, None, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn recents_are_trimmed_follow_visibility_and_include_commands() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    let store = &w.db.store;
    let a = issue(&w, &owner, "A", "").await;
    store
        .recents()
        .touch_entity(&owner, &format!("issue://{a}"))
        .await
        .unwrap();
    for i in 0..(rookery_store::recents::KEEP + 20) {
        store
            .recents()
            .touch_command(&owner, &format!("plugin.cmd{i}"))
            .await
            .unwrap();
    }
    store
        .recents()
        .touch_command(&owner, "plugin.cmd0")
        .await
        .unwrap();
    let cmds = store.recents().commands(&owner, 500).await.unwrap();
    assert_eq!(cmds.len() as i64, rookery_store::recents::KEEP);
    assert_eq!(cmds[0].id, "plugin.cmd0", "the latest run is first");
    // Kept until the entity is gone from the caller's view.
    assert_eq!(
        store
            .recents()
            .entities(&owner, None, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    w.data().delete(&owner, "issues", a).await.unwrap();
    assert!(
        store
            .recents()
            .entities(&owner, None, None, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .recents()
            .forget(&owner, "command", None)
            .await
            .unwrap() as i64,
        rookery_store::recents::KEEP
    );
}

#[tokio::test]
async fn pins_are_personal_or_shared_with_a_space_and_follow_permissions() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    let member = w.as_user(w.member);
    let outsider = w.as_user(w.outsider);
    let store = &w.db.store;
    let a = issue(&w, &owner, "Pinned issue", "").await;
    let uri = format!("issue://{a}");
    let entity_pin = |shared: Option<Uuid>| NewPin {
        kind: PinKind::Entity,
        target: uri.clone(),
        path: String::new(),
        view_space: None,
        shared_in: shared,
        title: String::new(),
        icon: None,
    };

    // Personal: the same pin twice is one; others do not see it.
    let p = store.pins().add(&owner, &entity_pin(None)).await.unwrap();
    assert_eq!(
        store
            .pins()
            .add(&owner, &entity_pin(None))
            .await
            .unwrap()
            .id,
        p.id
    );
    assert_eq!(p.entity.as_ref().unwrap().title, "Pinned issue");
    assert!(store.pins().list(&member, None).await.unwrap().is_empty());

    // Shared in the space: members see it, an outsider does not, and an outsider cannot share.
    let shared = store
        .pins()
        .add(&owner, &entity_pin(Some(w.space)))
        .await
        .unwrap();
    assert_eq!(shared.space_id, Some(w.space));
    let listed = store.pins().list(&member, Some(w.space)).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, shared.id);
    assert!(store.pins().list(&outsider, None).await.unwrap().is_empty());
    assert!(store.pins().list(&outsider, Some(w.space)).await.is_err());
    assert!(
        store
            .pins()
            .add(&outsider, &entity_pin(Some(w.space)))
            .await
            .is_err()
    );
    // Personal pins come first, then shared ones.
    let both = store.pins().list(&owner, None).await.unwrap();
    assert_eq!(
        both.iter()
            .map(|p| p.space_id.is_some())
            .collect::<Vec<_>>(),
        [false, true]
    );

    // A view pin, in a space.
    let view = store
        .pins()
        .add(
            &member,
            &NewPin {
                kind: PinKind::View,
                target: "notes.list".into(),
                path: String::new(),
                view_space: Some(w.space),
                shared_in: None,
                title: "Notes".into(),
                icon: Some("file".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(view.title, "Notes");
    // An entity the caller cannot see cannot be pinned.
    assert!(
        store
            .pins()
            .add(&outsider, &entity_pin(None))
            .await
            .is_err()
    );

    // A member may unpin a shared pin (they have `use`); nobody removes another's personal pin.
    assert!(store.pins().remove(&member, p.id).await.unwrap().is_none());
    assert!(
        store
            .pins()
            .remove(&member, shared.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(store.pins().remove(&owner, p.id).await.unwrap().is_some());
    // A pin to an entity that goes away is not listed.
    let again = store.pins().add(&owner, &entity_pin(None)).await.unwrap();
    w.data().delete(&owner, "issues", a).await.unwrap();
    assert!(
        store
            .pins()
            .list(&owner, None)
            .await
            .unwrap()
            .iter()
            .all(|p| p.id != again.id)
    );
}

#[tokio::test]
async fn activity_lists_recent_changes_in_my_spaces_collapses_runs_and_hides_what_i_cannot_see() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    let member = w.as_user(w.member);
    let outsider = w.as_user(w.outsider);
    let store = &w.db.store;

    let a = issue(&w, &owner, "Flaky test", "").await;
    // Three edits in a row are one entry, with the fields they touched.
    for t in ["Flaky tests", "Flaky tests in CI"] {
        w.data()
            .update(&owner, "issues", a, json!({ "title": t }), None)
            .await
            .unwrap();
    }
    w.data()
        .update(&owner, "issues", a, json!({ "state": "done" }), None)
        .await
        .unwrap();
    // Another person's edit is its own entry.
    w.data()
        .update(&member, "issues", a, json!({ "estimate": 3 }), None)
        .await
        .unwrap();
    let b = issue(&w, &member, "Second issue", "").await;

    let items = store.activity().recent(&member, None, 20).await.unwrap();
    let summary: Vec<_> = items
        .iter()
        .map(|i| {
            (
                i.entity.title.as_str(),
                i.op.as_str(),
                i.changes,
                i.actor_id,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("Second issue", "insert", 1, Some(w.member)),
            ("Flaky tests in CI", "update", 1, Some(w.member)),
            ("Flaky tests in CI", "insert", 4, Some(w.owner)),
        ],
        "{items:#?}"
    );
    assert!(items[1].fields.contains(&"estimate".to_string()));
    assert!(items[2].fields.iter().any(|f| f == "title"));
    assert!(items[2].fields.iter().any(|f| f == "state"));
    assert_eq!(items[0].actor_name.as_deref(), Some("member"));

    // Filtered to a space, limited, and invisible to someone outside.
    assert_eq!(
        store
            .activity()
            .recent(&owner, Some(w.space), 1)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .activity()
            .recent(&outsider, None, 20)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .activity()
            .recent(&outsider, Some(w.space), 20)
            .await
            .is_err()
    );
    let _ = b;
}
