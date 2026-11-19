//! Entities, references, backlinks and mentions across plugins.

use super::common;

use common::*;
use rookery_store::Caller;
use rookery_store::entities::{EntityRecord, EntitySearch};
use serde_json::{Value, json};
use uuid::Uuid;

fn id(row: &Value) -> Uuid {
    row["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn refs_and_backlinks_across_two_plugins() {
    let Some(w) = World::new().await else { return };
    w.db.store
        .collections()
        .apply("docs", &docs_defs())
        .await
        .unwrap();
    let owner = w.as_user(w.owner);
    let issues = w.db.store.data("issues");
    let docs = w.db.store.data("docs");
    let project = issues
        .insert(&owner, "projects", json!({ "name": "Crow" }))
        .await
        .unwrap()
        .row
        .unwrap();
    let issue = issues
        .insert(&owner, "issues", json!({ "title": "Fix login", "project": id(&project).to_string(), "assignee": w.member.to_string() }))
        .await
        .unwrap()
        .row
        .unwrap();
    let issue_uri = format!("issue://{}", id(&issue));
    let member_uri = format!("user://{}", w.member);

    // A page in the docs plugin mentions the issue and the member; a comment in the issues
    // plugin mentions the issue too.
    let out = docs
        .insert(
            &owner,
            "pages",
            json!({ "title": "Release notes", "body": doc_with_mentions("see", &[&issue_uri, &member_uri]), "related": [issue_uri] }),
        )
        .await
        .unwrap();
    let page = out.row.unwrap();
    let page_uri = format!("doc://{}", id(&page));
    assert_eq!(out.mentions.len(), 1, "the member mention is reported");
    assert_eq!(out.mentions[0].user, w.member);
    assert_eq!(out.mentions[0].entity.as_deref(), Some(page_uri.as_str()));
    assert_eq!(out.mentions[0].plugin, "docs");
    let comment = issues
        .insert(
            &owner,
            "comments",
            json!({ "issue": issue_uri, "body": doc_with_mentions("dup of", &[&issue_uri]) }),
        )
        .await
        .unwrap()
        .row
        .unwrap();

    let back =
        w.db.store
            .refs()
            .to(&w.as_user(w.member), &issue_uri)
            .await
            .unwrap();
    let mut got: Vec<(String, String)> = back
        .iter()
        .map(|b| (b.source.clone(), b.field.clone()))
        .collect();
    got.sort();
    let comment_src = format!("row://issues/comments/{}", id(&comment));
    let mut want = vec![
        (page_uri.clone(), "body".to_string()),
        (page_uri.clone(), "related".to_string()),
        (comment_src.clone(), "body".to_string()),
        (comment_src.clone(), "issue".to_string()),
    ];
    want.sort();
    assert_eq!(got, want);
    let from_page = back.iter().find(|b| b.source == page_uri).unwrap();
    assert_eq!(from_page.title.as_deref(), Some("Release notes"));
    assert_eq!(from_page.plugin.as_deref(), Some("docs"));
    assert_eq!(from_page.row_id, Some(id(&page)));
    let from_comment = back.iter().find(|b| b.source == comment_src).unwrap();
    assert_eq!(from_comment.row_id, Some(id(&comment)));

    // Backlinks to the user, from the issue's assignee and the page mention.
    let to_member = w.db.store.refs().to(&owner, &member_uri).await.unwrap();
    assert_eq!(to_member.len(), 2);
    // Outgoing references of the page.
    let out_refs = w.db.store.refs().from(&owner, &page_uri).await.unwrap();
    assert_eq!(out_refs.len(), 3);
    assert!(out_refs.iter().all(|r| !r.dangling));
    assert_eq!(
        out_refs
            .iter()
            .find(|r| r.target == issue_uri)
            .unwrap()
            .title
            .as_deref(),
        Some("Fix login")
    );

    // Someone who cannot see the sources gets no backlinks.
    assert!(
        w.db.store
            .refs()
            .to(&w.as_user(w.outsider), &issue_uri)
            .await
            .unwrap()
            .is_empty()
    );

    // Editing the body: refs follow, only newly added mentions are reported.
    let up = docs
        .update(
            &owner,
            "pages",
            id(&page),
            json!({ "body": doc_with_mentions("now", &[&member_uri, &format!("user://{}", w.outsider)]) }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        up.mentions.iter().map(|m| m.user).collect::<Vec<_>>(),
        vec![w.outsider]
    );
    let back = w.db.store.refs().to(&owner, &issue_uri).await.unwrap();
    assert_eq!(
        back.iter().filter(|b| b.source == page_uri).count(),
        1,
        "only `related` is left"
    );

    // Deleting the issue (into the trash) leaves references dangling; restoring revives them.
    issues.delete(&owner, "issues", id(&issue)).await.unwrap();
    let out_refs = w.db.store.refs().from(&owner, &page_uri).await.unwrap();
    assert!(
        out_refs
            .iter()
            .find(|r| r.target == issue_uri)
            .unwrap()
            .dangling
    );
    issues.restore(&owner, "issues", id(&issue)).await.unwrap();
    assert!(
        !w.db
            .store
            .refs()
            .from(&owner, &page_uri)
            .await
            .unwrap()
            .iter()
            .find(|r| r.target == issue_uri)
            .unwrap()
            .dangling
    );
    // A trashed source drops out of backlinks.
    docs.delete(&owner, "pages", id(&page)).await.unwrap();
    assert!(
        w.db.store
            .refs()
            .to(&owner, &issue_uri)
            .await
            .unwrap()
            .iter()
            .all(|b| b.source != page_uri)
    );
    docs.purge(&owner, "pages", id(&page)).await.unwrap();
    // Purging the issue removes it from the index; inbound refs stay, dangling.
    issues.delete(&owner, "issues", id(&issue)).await.unwrap();
    issues.purge(&owner, "issues", id(&issue)).await.unwrap();
    assert!(
        w.db.store
            .entities()
            .get(&owner, &issue_uri)
            .await
            .unwrap()
            .is_none()
    );
    let back = w.db.store.refs().to(&owner, &issue_uri).await.unwrap();
    assert_eq!(back.len(), 2, "the comment's references remain");
    let dangling: bool =
        sqlx::query_scalar("SELECT bool_and(dangling) FROM rookery.refs WHERE dst_id = $1")
            .bind(id(&issue).to_string())
            .fetch_one(w.db.store.pool())
            .await
            .unwrap();
    assert!(dangling);
}

#[tokio::test]
async fn user_scoped_rows_keep_their_backlinks_private() {
    let Some(w) = World::new().await else { return };
    let target = "env://build-01";
    w.data()
        .insert(
            &Caller::user(w.member),
            "drafts",
            json!({ "text": "note", "about": target }),
        )
        .await
        .unwrap();
    assert_eq!(
        w.db.store
            .refs()
            .to(&Caller::user(w.member), target)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        w.db.store
            .refs()
            .to(&Caller::user(w.owner), target)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn entity_index_and_search() {
    let Some(w) = World::new().await else { return };
    w.db.store
        .collections()
        .apply("docs", &docs_defs())
        .await
        .unwrap();
    let owner = w.as_user(w.owner);
    let docs = w.db.store.data("docs");
    let page = docs
        .insert(
            &owner,
            "pages",
            json!({ "title": "Deployment runbook", "body": doc_with_mentions("restart the kernel", &[]) }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    w.data()
        .insert(
            &owner,
            "issues",
            json!({ "title": "Deploy fails on Tuesdays" }),
        )
        .await
        .unwrap();
    let hidden = w
        .data()
        .insert(
            &owner,
            "projects",
            json!({ "name": "Deploy secrets", "private": true }),
        )
        .await
        .unwrap();
    assert!(hidden.row.is_some());

    // Entities registered by the kernel, not collection rows.
    w.db.store
        .entities()
        .register(&EntityRecord {
            uri: "env://build-01".into(),
            space_id: Some(w.space),
            parent: Some(format!("space://{}", w.space)),
            title: "build-01".into(),
            subtitle: Some("deploy host".into()),
            search_text: Some("deploy host in rack 4".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let search = |c: Caller, text: &str, schemes: Option<Vec<&str>>| {
        let w = &w;
        let q = EntitySearch {
            text: text.into(),
            schemes: schemes.map(|s| s.into_iter().map(String::from).collect()),
            ..Default::default()
        };
        async move {
            w.db.store
                .entities()
                .search(&c, &q)
                .await
                .unwrap()
                .into_iter()
                .map(|h| h.entity.title)
                .collect::<Vec<_>>()
        }
    };
    // Prefix matching, titles first; the member cannot see the private project.
    let member_hits = search(w.as_user(w.member), "depl", None).await;
    assert_eq!(member_hits.len(), 3, "{member_hits:?}");
    assert!(member_hits[..2].contains(&"Deployment runbook".to_string()));
    assert!(member_hits[..2].contains(&"Deploy fails on Tuesdays".to_string()));
    assert_eq!(member_hits[2], "build-01");
    assert_eq!(search(owner.clone(), "depl", None).await.len(), 4);
    assert_eq!(
        search(w.as_user(w.member), "runbook kernel", None).await,
        ["Deployment runbook"]
    );
    assert_eq!(
        search(w.as_user(w.member), "deploy", Some(vec!["env"])).await,
        ["build-01"]
    );
    assert!(
        search(w.as_user(w.outsider), "deploy", None)
            .await
            .is_empty()
    );
    // Users are mentionable by name for everyone.
    assert_eq!(
        search(w.as_user(w.outsider), "memb", Some(vec!["user"])).await,
        ["member"]
    );

    // The index follows updates.
    docs.update(
        &owner,
        "pages",
        id(&page),
        json!({ "title": "Rollback guide" }),
        None,
    )
    .await
    .unwrap();
    let e =
        w.db.store
            .entities()
            .get(&owner, &format!("doc://{}", id(&page)))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(e.title, "Rollback guide");
    assert_eq!(e.parent, Some(format!("space://{}", w.space)));
    assert_eq!(e.owner_id, Some(w.owner));
    assert_eq!(
        (e.plugin.as_deref(), e.collection.as_deref()),
        (Some("docs"), Some("pages"))
    );
    let children =
        w.db.store
            .entities()
            .children(&w.as_user(w.member), &format!("space://{}", w.space), 50)
            .await
            .unwrap();
    assert_eq!(children.len(), 3, "{children:?}");

    // References from registered entities.
    w.db.store
        .entities()
        .set_refs(
            "env://build-01",
            "runbook",
            &[format!("doc://{}", id(&page))],
        )
        .await
        .unwrap();
    let back =
        w.db.store
            .refs()
            .to(&owner, &format!("doc://{}", id(&page)))
            .await
            .unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].title.as_deref(), Some("build-01"));
    assert!(
        w.db.store
            .entities()
            .remove("env://build-01")
            .await
            .unwrap()
    );
    assert!(
        w.db.store
            .refs()
            .to(&owner, &format!("doc://{}", id(&page)))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn references_keep_the_time_they_were_made_across_rewrites() {
    let Some(w) = World::new().await else { return };
    w.db.store
        .collections()
        .apply("docs", &docs_defs())
        .await
        .unwrap();
    let owner = w.as_user(w.owner);
    let issues = w.db.store.data("issues");
    let docs = w.db.store.data("docs");
    let a = issues
        .insert(&owner, "issues", json!({ "title": "A" }))
        .await
        .unwrap()
        .row
        .unwrap();
    let b = issues
        .insert(&owner, "issues", json!({ "title": "B" }))
        .await
        .unwrap()
        .row
        .unwrap();
    let (ua, ub) = (format!("issue://{}", id(&a)), format!("issue://{}", id(&b)));
    let page = docs
        .insert(
            &owner,
            "pages",
            json!({ "title": "Spec", "body": doc_with_mentions("see", &[&ua]), "related": [ua] }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let first = w.db.store.refs().to(&owner, &ua).await.unwrap();
    assert_eq!(first.len(), 2);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    // The page is written again with A still referenced and B added: A's references keep their time.
    docs.update(
        &owner,
        "pages",
        id(&page),
        json!({ "body": doc_with_mentions("now", &[&ua, &ub]), "related": [ua, ub] }),
        None,
    )
    .await
    .unwrap();
    let again = w.db.store.refs().to(&owner, &ua).await.unwrap();
    assert_eq!(again.len(), 2);
    for b in &again {
        let before = first.iter().find(|x| x.field == b.field).unwrap();
        assert_eq!(b.at, before.at, "{} kept its time", b.field);
    }
    let to_b = w.db.store.refs().to(&owner, &ub).await.unwrap();
    assert_eq!(to_b.len(), 2);
    assert!(to_b.iter().all(|x| x.at > first[0].at));
    // Dropping A from both fields removes its references.
    docs.update(
        &owner,
        "pages",
        id(&page),
        json!({ "body": doc_with_mentions("only", &[&ub]), "related": [ub] }),
        None,
    )
    .await
    .unwrap();
    assert!(w.db.store.refs().to(&owner, &ua).await.unwrap().is_empty());
    assert_eq!(w.db.store.refs().to(&owner, &ub).await.unwrap().len(), 2);
}

#[tokio::test]
async fn references_apply_collection_read_rules_at_both_ends() {
    let Some(w) = World::new().await else { return };
    let mut definitions = issues_defs();
    definitions[0].access.admin = true;
    definitions[1].access.read = rookery_store::Level::Manage;
    definitions[2].access.read = rookery_store::Level::Manage;
    w.db.store
        .collections()
        .apply("issues", &definitions)
        .await
        .unwrap();

    let owner = w.as_user(w.owner).as_admin();
    let member = w.as_user(w.member);
    let user_uri = format!("user://{}", w.owner);
    let project = w
        .data()
        .insert(&owner, "projects", json!({ "name": "Internal" }))
        .await
        .unwrap()
        .row
        .unwrap();
    let project_uri = format!("project://{}", id(&project));
    let issue = w
        .data()
        .insert(
            &owner,
            "issues",
            json!({ "title": "Repair", "project": project_uri, "assignee": user_uri }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let issue_uri = format!("issue://{}", id(&issue));
    w.data()
        .insert(&owner, "comments", json!({ "issue": issue_uri }))
        .await
        .unwrap();
    w.db.store
        .collections()
        .apply("docs", &docs_defs())
        .await
        .unwrap();
    let page =
        w.db.store
            .data("docs")
            .insert(
                &owner,
                "pages",
                json!({ "title": "Notes", "related": [project_uri, issue_uri] }),
            )
            .await
            .unwrap()
            .row
            .unwrap();
    let page_uri = format!("doc://{}", id(&page));

    let refs = w.db.store.refs();
    assert!(refs.to(&member, &user_uri).await.unwrap().is_empty());
    assert!(refs.to(&member, &issue_uri).await.unwrap().is_empty());
    assert!(refs.from(&member, &issue_uri).await.unwrap().is_empty());
    let targets = refs.from(&member, &page_uri).await.unwrap();
    assert_eq!(targets.len(), 2);
    assert!(
        targets
            .iter()
            .all(|target| target.dangling && target.title.is_none() && target.icon.is_none())
    );
    assert_eq!(refs.to(&owner, &issue_uri).await.unwrap().len(), 2);
    assert!(
        refs.from(&owner, &page_uri)
            .await
            .unwrap()
            .iter()
            .all(|target| !target.dangling && target.title.is_some())
    );
}

#[tokio::test]
async fn attached_entities_inherit_collection_read_rules_through_intermediate_parents() {
    let Some(w) = World::new().await else { return };
    let mut definitions = issues_defs();
    definitions[0].access.admin = true;
    w.db.store
        .collections()
        .apply("issues", &definitions)
        .await
        .unwrap();
    let admin = w.as_user(w.owner).as_admin();
    let project = w
        .data()
        .insert(&admin, "projects", json!({ "name": "Internal" }))
        .await
        .unwrap()
        .row
        .unwrap();

    let entities = w.db.store.entities();
    let mut parent = format!("project://{}", id(&project));
    for uri in ["note://section", "note://page"] {
        entities
            .register(&EntityRecord {
                uri: uri.into(),
                parent: Some(parent),
                ..Default::default()
            })
            .await
            .unwrap();
        parent = uri.into();
    }
    assert!(entities.get(&admin, &parent).await.unwrap().is_some());
    assert!(
        entities
            .get(&w.as_user(w.member), &parent)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn collection_query_skips_non_uuid_resources_in_its_scheme() {
    let Some(w) = World::new().await else { return };
    let project = w
        .data()
        .insert(&w.as_user(w.owner), "projects", json!({ "name": "Build" }))
        .await
        .unwrap()
        .row
        .unwrap();
    let project_uri = format!("project://{}", id(&project));
    let outsider = w.as_user(w.outsider);
    for uri in [&project_uri, "project://build"] {
        w.db.store
            .access()
            .grant(uri, &outsider.principal, 1, "system:test")
            .await
            .unwrap();
    }
    let found = w
        .data()
        .find(&outsider, "projects", &Default::default())
        .await
        .unwrap();
    assert_eq!(
        found.rows.iter().map(id).collect::<Vec<_>>(),
        vec![id(&project)]
    );
}
