//! Queries and writes on collections.

use super::common;

use std::collections::HashSet;

use common::*;
use rookery_proto::ErrorCode;
use rookery_store::collections::{BatchOp, Direction, OrderBy, Query};
use rookery_store::{Caller, Error, Level};
use serde_json::{Value, json};
use uuid::Uuid;

fn id(row: &Value) -> Uuid {
    row["id"].as_str().unwrap().parse().unwrap()
}

fn titles(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|r| r["title"].as_str().unwrap_or("-").to_string())
        .collect()
}

async fn seed(w: &World) -> Vec<Value> {
    let c = w.as_user(w.owner);
    let rows = vec![
        json!({ "title": "alpha", "state": "open", "estimate": 1.5, "due": "2026-01-10", "labels": ["label://bug", "label://ui"],
                "assignee": w.member.to_string(), "closedAt": null, "meta": { "a": 1 } }),
        json!({ "title": "beta", "state": "done", "estimate": 3, "due": "2026-02-01", "labels": ["label://bug"],
                "closedAt": "2026-03-01T10:00:00Z", "meta": { "a": 2 } }),
        json!({ "title": "gamma", "state": "open", "estimate": null, "due": null, "labels": [],
                "assignee": format!("user://{}", w.owner) }),
        json!({ "title": "Delta", "state": "done", "estimate": 8, "due": "2025-12-24", "closedAt": "2025-12-24T08:30:00+02:00" }),
        json!({ "title": "epsilon", "state": "open", "estimate": 3, "labels": ["label://ui"], "meta": [1, 2] }),
    ];
    let mut out = Vec::new();
    for r in rows {
        out.push(w.data().insert(&c, "issues", r).await.unwrap().row.unwrap());
    }
    out
}

#[tokio::test]
async fn insert_get_and_field_types() {
    let Some(w) = World::new().await else { return };
    let rows = seed(&w).await;
    let a = &rows[0];
    assert_eq!(a["number"], json!(1));
    assert_eq!(rows[4]["number"], json!(5));
    assert_eq!(a["state"], json!("open"));
    assert_eq!(
        a["assignee"],
        json!(format!("user://{}", w.member)),
        "a bare id is stored as a URI"
    );
    assert_eq!(a["rev"], json!(1));
    assert_eq!(a["spaceId"], json!(w.space.to_string()));
    assert!(a.get("userId").is_none());
    assert_eq!(a["deletedAt"], json!(null));
    assert_eq!(rows[3]["closedAt"], json!("2025-12-24T06:30:00+00:00"));
    let c = w.as_user(w.member);
    let got = w.data().get(&c, "issues", id(a)).await.unwrap().unwrap();
    assert_eq!(&got, a);
    // The default applied.
    let d = w
        .data()
        .insert(&c, "issues", json!({ "title": "defaulted" }))
        .await
        .unwrap()
        .row
        .unwrap();
    assert_eq!(d["state"], json!("open"));

    for (bad, why) in [
        (json!({ "title": 5 }), "text"),
        (json!({ "state": "nope" }), "enum"),
        (json!({ "due": "01/02/2026" }), "date"),
        (json!({ "closedAt": "yesterday" }), "datetime"),
        (json!({ "estimate": "x" }), "number"),
        (json!({ "assignee": "env://x" }), "ref target"),
        (json!({ "labels": ["no-scheme"] }), "refs"),
        (json!({ "number": 9 }), "sequence"),
        (json!({ "nosuch": 1 }), "unknown"),
        (json!({ "body": "plain" }), "richtext"),
    ] {
        let e = w.data().insert(&c, "issues", bad).await.unwrap_err();
        assert_eq!(e.code(), ErrorCode::Invalid, "{why}: {e}");
    }
    let e = w
        .data()
        .insert(&w.as_user(w.member), "projects", json!({ "key": "K" }))
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid, "required");
}

#[tokio::test]
async fn where_operators_and_in_memory_matching_agree() {
    let Some(w) = World::new().await else { return };
    let rows = seed(&w).await;
    let c = w.as_user(w.owner);
    let member = format!("user://{}", w.member);
    let cases: Vec<(Value, Vec<&str>)> = vec![
        (
            json!({ "state": "open" }),
            vec!["alpha", "gamma", "epsilon"],
        ),
        (
            json!({ "state": "open", "assignee": w.member.to_string() }),
            vec!["alpha"],
        ),
        (json!({ "assignee": member }), vec!["alpha"]),
        (
            json!({ "assignee": null }),
            vec!["beta", "Delta", "epsilon"],
        ),
        (
            json!({ "assignee": { "ne": null } }),
            vec!["alpha", "gamma"],
        ),
        (json!({ "state": { "ne": "open" } }), vec!["beta", "Delta"]),
        (
            json!({ "estimate": { "ne": 3 } }),
            vec!["alpha", "gamma", "Delta"],
        ),
        (
            json!({ "state": { "in": ["done"] } }),
            vec!["beta", "Delta"],
        ),
        (
            json!({ "estimate": { "in": [3, null] } }),
            vec!["beta", "gamma", "epsilon"],
        ),
        (
            json!({ "estimate": { "nin": [3] } }),
            vec!["alpha", "gamma", "Delta"],
        ),
        (
            json!({ "estimate": { "nin": [3, null] } }),
            vec!["alpha", "Delta"],
        ),
        (json!({ "estimate": { "in": [] } }), vec![]),
        (
            json!({ "estimate": { "gt": 1.5 } }),
            vec!["beta", "Delta", "epsilon"],
        ),
        (
            json!({ "estimate": { "gte": 1.5, "lt": 8 } }),
            vec!["alpha", "beta", "epsilon"],
        ),
        (
            json!({ "estimate": { "lte": 3 } }),
            vec!["alpha", "beta", "epsilon"],
        ),
        (
            json!({ "due": { "lt": "2026-01-15" } }),
            vec!["alpha", "Delta"],
        ),
        (
            json!({ "closedAt": { "gt": "2026-01-01T00:00:00Z" } }),
            vec!["beta"],
        ),
        (
            json!({ "closedAt": { "lte": "2025-12-24T06:30:00Z" } }),
            vec!["Delta"],
        ),
        (json!({ "title": { "lt": "a" } }), vec!["Delta"]),
        (
            json!({ "title": { "gte": "delta" } }),
            vec!["epsilon", "gamma"],
        ),
        (
            json!({ "labels": { "contains": "label://bug" } }),
            vec!["alpha", "beta"],
        ),
        (
            json!({ "labels": { "contains": ["label://bug", "label://ui"] } }),
            vec!["alpha"],
        ),
        (json!({ "meta": { "eq": { "a": 1.0 } } }), vec!["alpha"]),
        (json!({ "meta": { "eq": [1, 2] } }), vec!["epsilon"]),
        (
            json!({ "or": [{ "state": "done" }, { "estimate": 1.5 }] }),
            vec!["alpha", "beta", "Delta"],
        ),
        (
            json!({ "and": [{ "state": "open" }, { "not": { "estimate": 3 } }] }),
            vec!["alpha", "gamma"],
        ),
        (
            json!({ "not": { "estimate": { "gt": 2 } } }),
            vec!["alpha", "gamma"],
        ),
        (
            json!({ "not": { "or": [{ "assignee": null }, { "state": "done" }] } }),
            vec!["alpha", "gamma"],
        ),
        (
            json!({ "not": { "not": { "estimate": { "lt": 4 } } } }),
            vec!["alpha", "beta", "epsilon"],
        ),
        (
            json!({ "and": [] }),
            vec!["alpha", "beta", "gamma", "Delta", "epsilon"],
        ),
        (json!({ "or": [] }), vec![]),
        (
            json!({ "rev": 1, "createdBy": w.owner.to_string() }),
            vec!["alpha", "beta", "gamma", "Delta", "epsilon"],
        ),
        (json!({ "id": rows[1]["id"] }), vec!["beta"]),
        (json!({ "number": { "gt": 3 } }), vec!["Delta", "epsilon"]),
        (
            json!({ "project": null }),
            vec!["alpha", "beta", "gamma", "Delta", "epsilon"],
        ),
    ];
    let mut vectors = Vec::new();
    for (filter, want) in cases {
        vectors.push(json!({ "where": filter, "matches": want }));
        let q = Query {
            filter: Some(filter.clone()),
            limit: Some(100),
            ..Default::default()
        };
        let got: HashSet<String> = titles(&w.data().find(&c, "issues", &q).await.unwrap().rows)
            .into_iter()
            .collect();
        let want: HashSet<String> = want.into_iter().map(String::from).collect();
        assert_eq!(got, want, "SQL for {filter}");
        let f = w.data().filter("issues", &filter).unwrap();
        let mem: HashSet<String> = rows
            .iter()
            .filter(|r| f.matches(r))
            .map(|r| r["title"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(mem, want, "in memory for {filter}");
        assert_eq!(
            w.data().count(&c, "issues", &q).await.unwrap() as usize,
            want.len(),
            "count for {filter}"
        );
    }
    let mut invalid = Vec::new();
    for bad in [
        json!({ "nosuch": 1 }),
        json!({ "state": { "like": "o%" } }),
        json!({ "meta": { "a": 1 } }),
        json!({ "labels": "label://bug" }),
        json!({ "title": { "contains": "a" } }),
        json!({ "meta": { "lt": 1 } }),
        json!({ "estimate": { "lt": null } }),
        json!({ "state": ["open"] }),
        json!({ "or": { "state": "open" } }),
        json!("state = 'open'"),
        json!({ "title": "x'); DROP TABLE p_issues.issues; --" }),
    ] {
        if w.data().filter("issues", &bad).is_err() {
            invalid.push(bad.clone());
        }
        let q = Query::filter(bad.clone());
        match w.data().find(&c, "issues", &q).await {
            Err(e) => assert_eq!(e.code(), ErrorCode::Invalid, "{bad}"),
            Ok(p) => assert!(p.rows.is_empty(), "{bad}"),
        }
    }
    assert_eq!(
        w.data()
            .count(&c, "issues", &Query::default())
            .await
            .unwrap(),
        5
    );

    // Orders, as SQL returns them, for the client's ordering of live query results. Text keys are left
    // out: the database orders text by its collation, which a client cannot reproduce exactly.
    let mut orders = Vec::new();
    let desc = |f: &str| OrderBy::Pair(f.into(), Direction::Desc);
    let asc = |f: &str| OrderBy::Field(f.into());
    for order in [
        vec![desc("number")],
        vec![asc("estimate")],
        vec![desc("estimate")],
        vec![asc("state"), desc("due")],
        vec![],
    ] {
        let q = Query {
            order_by: Some(order.clone()),
            limit: Some(100),
            ..Default::default()
        };
        let got = titles(&w.data().find(&c, "issues", &q).await.unwrap().rows);
        orders.push(json!({ "orderBy": order, "titles": got }));
    }
    write_vectors(&w, &rows, vectors, invalid, orders);
}

/// Writes `crates/rookery-filter/vectors/filter.json`, the reference cases `rookery-filter` replays without a
/// database. Ids are replaced by stable ones in order of appearance, and the
/// write times by a constant, so the file only changes when the cases do.
fn write_vectors(
    w: &World,
    rows: &[Value],
    cases: Vec<Value>,
    invalid: Vec<Value>,
    orders: Vec<Value>,
) {
    let coll = w.db.store.collections().get("issues", "issues").unwrap();
    let mut doc = json!({
        "comment": "Generated by crates/rookery-store/tests/data/queries.rs (where_operators_and_in_memory_matching_agree). Do not edit.",
        "collection": coll.def,
        "rows": rows,
        "cases": cases,
        "invalid": invalid,
        "orders": orders,
    });
    for r in doc["rows"].as_array_mut().unwrap() {
        r["createdAt"] = json!("2026-01-01T00:00:00+00:00");
        r["updatedAt"] = json!("2026-01-01T00:00:00+00:00");
    }
    let mut text = serde_json::to_string_pretty(&doc).unwrap();
    let re_ids: Vec<String> = {
        let mut seen = Vec::new();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i + 36 <= bytes.len() {
            let cand = &text[i..i + 36];
            if Uuid::parse_str(cand).is_ok() && cand.chars().filter(|c| *c == '-').count() == 4 {
                if !seen.contains(&cand.to_string()) {
                    seen.push(cand.to_string());
                }
                i += 36;
            } else {
                i += 1;
            }
        }
        seen
    };
    for (n, old) in re_ids.iter().enumerate() {
        text = text.replace(
            old.as_str(),
            &format!("00000000-0000-7000-8000-{:012}", n + 1),
        );
    }
    text.push('\n');
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rookery-filter/vectors/filter.json");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
}

#[tokio::test]
async fn order_and_cursor_pagination() {
    let Some(w) = World::new().await else { return };
    seed(&w).await;
    let c = w.as_user(w.owner);
    let page_through = |order: Vec<OrderBy>, limit: i64| {
        let w = &w;
        let c = c.clone();
        async move {
            let mut all = Vec::new();
            let mut after = None;
            loop {
                let q = Query {
                    order_by: Some(order.clone()),
                    limit: Some(limit),
                    after: after.clone(),
                    ..Default::default()
                };
                let p = w.data().find(&c, "issues", &q).await.unwrap();
                assert!(p.rows.len() as i64 <= limit);
                all.extend(titles(&p.rows));
                match p.next {
                    Some(n) => after = Some(n),
                    None => break,
                }
            }
            all
        }
    };
    let desc = |f: &str| OrderBy::Pair(f.into(), Direction::Desc);
    let asc = |f: &str| OrderBy::Field(f.into());
    assert_eq!(
        page_through(vec![desc("number")], 2).await,
        ["epsilon", "Delta", "gamma", "beta", "alpha"]
    );
    // Nulls last ascending, first descending; ties broken by id (insertion order).
    assert_eq!(
        page_through(vec![asc("estimate")], 1).await,
        ["alpha", "beta", "epsilon", "Delta", "gamma"]
    );
    assert_eq!(
        page_through(vec![desc("estimate")], 2).await,
        ["gamma", "Delta", "beta", "epsilon", "alpha"]
    );
    assert_eq!(
        page_through(vec![asc("state"), desc("due")], 1).await,
        ["beta", "Delta", "gamma", "epsilon", "alpha"]
    );
    assert_eq!(
        page_through(vec![], 3).await,
        ["alpha", "beta", "gamma", "Delta", "epsilon"]
    );
    // A cursor from another query is refused.
    let p = w
        .data()
        .find(
            &c,
            "issues",
            &Query {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let q = Query {
        order_by: Some(vec![desc("estimate"), asc("title")]),
        after: p.next,
        ..Default::default()
    };
    assert_eq!(
        w.data().find(&c, "issues", &q).await.unwrap_err().code(),
        ErrorCode::Invalid
    );
    let q = Query {
        after: Some("garbage".into()),
        ..Default::default()
    };
    assert_eq!(
        w.data().find(&c, "issues", &q).await.unwrap_err().code(),
        ErrorCode::Invalid
    );
    for bad in [asc("body"), asc("labels"), asc("nosuch")] {
        let q = Query {
            order_by: Some(vec![bad]),
            ..Default::default()
        };
        assert_eq!(
            w.data().find(&c, "issues", &q).await.unwrap_err().code(),
            ErrorCode::Invalid
        );
    }
}

#[tokio::test]
async fn search_ranks_titles_above_bodies() {
    let Some(w) = World::new().await else { return };
    let c = w.as_user(w.owner);
    let ins = |title: &str, body: &str| {
        let w = &w;
        let c = c.clone();
        let v = json!({ "title": title, "body": doc_with_mentions(body, &[]) });
        async move { w.data().insert(&c, "issues", v).await.unwrap() }
    };
    ins("Unrelated", "the deploy pipeline is slow").await;
    ins("Deploy pipeline broken", "nothing works").await;
    ins("Flaky tests", "see the deployment logs").await;
    ins("Other", "nothing here").await;
    let hits = w
        .data()
        .search(&c, "issues", "deploy", None, 10)
        .await
        .unwrap()
        .rows;
    assert_eq!(
        titles(&hits),
        ["Deploy pipeline broken", "Unrelated", "Flaky tests"]
    );
    // Every word must match (as a prefix); filters combine with search.
    let hits = w
        .data()
        .search(&c, "issues", "deploy pipe", None, 10)
        .await
        .unwrap()
        .rows;
    assert_eq!(titles(&hits), ["Deploy pipeline broken", "Unrelated"]);
    let hits = w
        .data()
        .search(
            &c,
            "issues",
            "deploy",
            Some(json!({ "title": { "ne": "Unrelated" } })),
            10,
        )
        .await
        .unwrap()
        .rows;
    assert_eq!(titles(&hits), ["Deploy pipeline broken", "Flaky tests"]);
    assert!(
        w.data()
            .search(&c, "issues", "!!!", None, 10)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    // Paging through ranked results.
    let p1 = w
        .data()
        .find(
            &c,
            "issues",
            &Query {
                search: Some("deploy".into()),
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(titles(&p1.rows), ["Deploy pipeline broken", "Unrelated"]);
    let p2 = w
        .data()
        .find(
            &c,
            "issues",
            &Query {
                search: Some("deploy".into()),
                limit: Some(2),
                after: p1.next,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(titles(&p2.rows), ["Flaky tests"]);
    assert!(p2.next.is_none());
    assert_eq!(
        w.data()
            .count(
                &c,
                "issues",
                &Query {
                    search: Some("deploy".into()),
                    ..Default::default()
                }
            )
            .await
            .unwrap(),
        3
    );
}

#[tokio::test]
async fn updates_check_rev_and_record_history() {
    let Some(w) = World::new().await else { return };
    let c = w.as_user(w.owner);
    let row = w
        .data()
        .insert(&c, "issues", json!({ "title": "t", "estimate": 1 }))
        .await
        .unwrap()
        .row
        .unwrap();
    let agent = Caller::on_behalf_of("plugin:agent-s1", w.member).in_space(w.space);
    let up = w
        .data()
        .update(
            &agent,
            "issues",
            id(&row),
            json!({ "title": "t2", "estimate": 1 }),
            Some(1),
        )
        .await
        .unwrap();
    let r2 = up.row.unwrap();
    assert_eq!(
        (r2["rev"].clone(), r2["title"].clone()),
        (json!(2), json!("t2"))
    );
    assert_eq!(r2["updatedBy"], json!(w.member.to_string()));
    assert!(up.change.is_some());
    // A stale rev conflicts, with details.
    let e = w
        .data()
        .update(&c, "issues", id(&row), json!({ "title": "t3" }), Some(1))
        .await
        .unwrap_err();
    assert!(matches!(
        e,
        Error::RevConflict {
            expected: 1,
            actual: 2
        }
    ));
    assert_eq!(e.code(), ErrorCode::Conflict);
    assert_eq!(e.details().unwrap(), json!({ "expected": 1, "actual": 2 }));
    // A patch that changes nothing writes nothing.
    let same = w
        .data()
        .update(&c, "issues", id(&row), json!({ "title": "t2" }), Some(2))
        .await
        .unwrap();
    assert!(same.change.is_none());
    assert_eq!(same.row.unwrap()["rev"], json!(2));
    w.data()
        .update(&c, "issues", id(&row), json!({ "estimate": null }), None)
        .await
        .unwrap();

    let h = w.data().history(&c, "issues", id(&row), 10).await.unwrap();
    assert_eq!(
        h.iter().map(|e| e.op.as_str()).collect::<Vec<_>>(),
        ["update", "update", "insert"]
    );
    assert_eq!(
        h[1].changes,
        json!({ "title": ["t", "t2"] }),
        "only changed fields"
    );
    assert_eq!(h[1].principal, "plugin:agent-s1");
    assert_eq!(h[1].on_behalf_of, Some(w.member));
    assert_eq!(h[0].changes, json!({ "estimate": [1, null] }));
    assert_eq!(h[0].on_behalf_of, None);
    assert_eq!(
        w.data()
            .history(&w.as_user(w.outsider), "issues", id(&row), 10)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn concurrent_rev_checked_updates_admit_one_winner() {
    let Some(w) = World::new().await else { return };
    let c = w.as_user(w.owner);
    let row = w
        .data()
        .insert(&c, "issues", json!({ "title": "race" }))
        .await
        .unwrap()
        .row
        .unwrap();
    let rid = id(&row);
    let mut tasks = Vec::new();
    for i in 0..8 {
        let store = w.db.store.clone();
        let c = c.clone();
        tasks.push(tokio::spawn(async move {
            store
                .data("issues")
                .update(
                    &c,
                    "issues",
                    rid,
                    json!({ "title": format!("w{i}") }),
                    Some(1),
                )
                .await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => assert_eq!(e.code(), ErrorCode::Conflict),
        }
    }
    assert_eq!(ok, 1);
}

#[tokio::test]
async fn unique_and_sequence_fields_under_concurrency() {
    let Some(w) = World::new().await else { return };
    let c = w.as_user(w.owner);
    // 40 concurrent inserts: numbers are 1..=40 without gaps or duplicates.
    let mut tasks = Vec::new();
    for i in 0..40 {
        let store = w.db.store.clone();
        let c = c.clone();
        tasks.push(tokio::spawn(async move {
            store
                .data("issues")
                .insert(&c, "issues", json!({ "title": format!("n{i}") }))
                .await
                .unwrap()
                .row
                .unwrap()
        }));
    }
    let mut numbers: Vec<i64> = Vec::new();
    for t in tasks {
        numbers.push(t.await.unwrap()["number"].as_i64().unwrap());
    }
    numbers.sort();
    assert_eq!(numbers, (1..=40).collect::<Vec<_>>());
    // Sequences are per space.
    let other = w.db.store.spaces().personal(w.owner).await.unwrap().id;
    let r = w
        .data()
        .insert(
            &Caller::user(w.owner).in_space(other),
            "issues",
            json!({ "title": "x" }),
        )
        .await
        .unwrap();
    assert_eq!(r.row.unwrap()["number"], json!(1));

    // Ten concurrent inserts of the same case-insensitive key: exactly one wins.
    let mut tasks = Vec::new();
    for i in 0..10 {
        let store = w.db.store.clone();
        let c = c.clone();
        let key = if i % 2 == 0 { "CROW" } else { "crow" };
        tasks.push(tokio::spawn(async move {
            store
                .data("issues")
                .insert(
                    &c,
                    "projects",
                    json!({ "name": format!("p{i}"), "key": key }),
                )
                .await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => assert_eq!(e.code(), ErrorCode::Conflict, "{e}"),
        }
    }
    assert_eq!(ok, 1);
    // The same key is fine in another space.
    w.data()
        .insert(
            &Caller::user(w.owner).in_space(other),
            "projects",
            json!({ "name": "p", "key": "crow" }),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn trash_restore_and_purge() {
    let Some(w) = World::new().await else { return };
    let c = w.as_user(w.owner);
    let row = w
        .data()
        .insert(&c, "issues", json!({ "title": "bin me" }))
        .await
        .unwrap()
        .row
        .unwrap();
    let del = w.data().delete(&c, "issues", id(&row)).await.unwrap();
    assert!(!del.row.as_ref().unwrap()["deletedAt"].is_null());
    assert!(
        w.data()
            .get(&c, "issues", id(&row))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        w.data()
            .get_trashed(&c, "issues", id(&row))
            .await
            .unwrap()
            .is_some()
    );
    let trash = w
        .data()
        .find(
            &c,
            "issues",
            &Query {
                trashed: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(trash.rows.len(), 1);
    assert_eq!(
        w.data()
            .update(&c, "issues", id(&row), json!({ "title": "x" }), None)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
    let back = w
        .data()
        .restore(&c, "issues", id(&row))
        .await
        .unwrap()
        .row
        .unwrap();
    assert!(back["deletedAt"].is_null());
    assert_eq!(back["rev"], json!(3));
    w.data().delete(&c, "issues", id(&row)).await.unwrap();
    // Members can delete (write level) but purging needs manage.
    assert_eq!(
        w.data()
            .purge(&w.as_user(w.member), "issues", id(&row))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    w.data().purge(&c, "issues", id(&row)).await.unwrap();
    assert!(
        w.data()
            .get_trashed(&c, "issues", id(&row))
            .await
            .unwrap()
            .is_none()
    );
    // Without a trash, deletes are final.
    let cm = w
        .data()
        .insert(&c, "comments", json!({}))
        .await
        .unwrap()
        .row
        .unwrap();
    assert!(
        w.data()
            .delete(&c, "comments", id(&cm))
            .await
            .unwrap()
            .row
            .is_none()
    );
    assert_eq!(
        w.data()
            .restore(&c, "comments", id(&cm))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Invalid
    );
}

#[tokio::test]
async fn scope_isolates_spaces_and_users() {
    let Some(w) = World::new().await else { return };
    let mine = w.db.store.spaces().personal(w.member).await.unwrap().id;
    let in_project = w.as_user(w.member);
    let in_personal = Caller::user(w.member).in_space(mine);
    let a = w
        .data()
        .insert(&in_project, "issues", json!({ "title": "project issue" }))
        .await
        .unwrap()
        .row
        .unwrap();
    w.data()
        .insert(&in_personal, "issues", json!({ "title": "personal issue" }))
        .await
        .unwrap();
    assert_eq!(
        titles(
            &w.data()
                .find(&in_project, "issues", &Query::default())
                .await
                .unwrap()
                .rows
        ),
        ["project issue"]
    );
    assert_eq!(
        titles(
            &w.data()
                .find(&in_personal, "issues", &Query::default())
                .await
                .unwrap()
                .rows
        ),
        ["personal issue"]
    );
    // A row of one space cannot be read or changed from another.
    assert!(
        w.data()
            .get(&in_personal, "issues", id(&a))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        w.data()
            .update(
                &in_personal,
                "issues",
                id(&a),
                json!({ "title": "x" }),
                None
            )
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
    // Querying several spaces at once checks each.
    let both = Query {
        spaces: Some(vec![w.space, mine]),
        ..Default::default()
    };
    assert_eq!(
        w.data().count(&in_project, "issues", &both).await.unwrap(),
        2
    );
    assert_eq!(
        w.data()
            .count(&w.as_user(w.owner), "issues", &both)
            .await
            .unwrap(),
        1,
        "the owner cannot see member's personal space"
    );
    // No space: refused.
    assert_eq!(
        w.data()
            .find(&Caller::user(w.member), "issues", &Query::default())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Invalid
    );

    // User scope: each user sees only their own rows, whatever space they are in.
    w.data()
        .insert(&in_project, "drafts", json!({ "text": "member draft" }))
        .await
        .unwrap();
    w.data()
        .insert(
            &w.as_user(w.owner),
            "drafts",
            json!({ "text": "owner draft" }),
        )
        .await
        .unwrap();
    let d = w
        .data()
        .find(&Caller::user(w.member), "drafts", &Query::default())
        .await
        .unwrap()
        .rows;
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["text"], json!("member draft"));
    assert_eq!(d[0]["userId"], json!(w.member.to_string()));
    assert!(d[0].get("spaceId").is_none());
    assert_eq!(
        w.data()
            .update(
                &Caller::user(w.owner),
                "drafts",
                id(&d[0]),
                json!({ "text": "x" }),
                None
            )
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn access_levels_are_enforced_with_inheritance() {
    let Some(w) = World::new().await else { return };
    let (owner, member, outsider) = (
        w.as_user(w.owner),
        w.as_user(w.member),
        w.as_user(w.outsider),
    );
    let open = w
        .data()
        .insert(&owner, "projects", json!({ "name": "Open" }))
        .await
        .unwrap()
        .row
        .unwrap();
    let hidden = w
        .data()
        .insert(
            &owner,
            "projects",
            json!({ "name": "Hidden", "private": true }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let open_uri = format!("project://{}", id(&open));
    let hidden_uri = format!("project://{}", id(&hidden));
    let i1 = w
        .data()
        .insert(
            &owner,
            "issues",
            json!({ "title": "in open", "project": id(&open).to_string() }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let i2 = w
        .data()
        .insert(
            &owner,
            "issues",
            json!({ "title": "in hidden", "project": id(&hidden).to_string() }),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    w.data()
        .insert(&owner, "issues", json!({ "title": "loose" }))
        .await
        .unwrap();

    // The member sees what inherits from the space, not the private project or its issues.
    let seen = titles(
        &w.data()
            .find(&member, "issues", &Query::default())
            .await
            .unwrap()
            .rows,
    );
    assert_eq!(seen, ["in open", "loose"]);
    assert_eq!(
        w.data()
            .count(&member, "issues", &Query::default())
            .await
            .unwrap(),
        2
    );
    assert!(
        w.data()
            .get(&member, "issues", id(&i2))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        w.data()
            .count(&member, "projects", &Query::default())
            .await
            .unwrap(),
        1
    );
    // Writing into the private project is forbidden; into the open one allowed (use).
    let e = w
        .data()
        .insert(
            &member,
            "issues",
            json!({ "title": "x", "project": hidden_uri }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Forbidden);
    w.data()
        .insert(
            &member,
            "issues",
            json!({ "title": "by member", "project": open_uri }),
        )
        .await
        .unwrap();
    // Moving an issue into the private project is forbidden too.
    let e = w
        .data()
        .update(
            &member,
            "issues",
            id(&i1),
            json!({ "project": hidden_uri }),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Forbidden);

    // The outsider sees nothing and cannot write; a direct grant on one issue lets them see it.
    assert_eq!(
        w.data()
            .count(&outsider, "issues", &Query::default())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        w.data()
            .insert(&outsider, "issues", json!({ "title": "x" }))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    assert_eq!(
        w.data()
            .find(&outsider, "comments", &Query::default())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    w.db.store
        .access()
        .grant(
            &format!("issue://{}", id(&i2)),
            &format!("user:{}", w.outsider),
            Level::View.as_i16(),
            &owner.actor(),
        )
        .await
        .unwrap();
    assert_eq!(
        titles(
            &w.data()
                .find(&outsider, "issues", &Query::default())
                .await
                .unwrap()
                .rows
        ),
        ["in hidden"]
    );
    let e = w
        .data()
        .update(&outsider, "issues", id(&i2), json!({ "title": "y" }), None)
        .await
        .unwrap_err();
    assert_eq!(
        e.code(),
        ErrorCode::Forbidden,
        "view is not enough to write"
    );

    // A viewer of the space can read but not write.
    w.db.store
        .spaces()
        .add_member(
            &owner,
            w.space,
            &format!("user:{}", w.outsider),
            Level::View,
        )
        .await
        .unwrap();
    assert_eq!(
        w.data()
            .count(&outsider, "issues", &Query::default())
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        w.data()
            .insert(&outsider, "comments", json!({}))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    // Admins can do everything.
    let admin = Caller::user(w.outsider).in_space(w.space).as_admin();
    assert_eq!(
        w.data()
            .count(&admin, "issues", &Query::default())
            .await
            .unwrap(),
        4
    );
    w.data()
        .update(&admin, "issues", id(&i2), json!({ "title": "admin" }), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn batches_are_one_transaction() {
    let Some(w) = World::new().await else { return };
    let c = w.as_user(w.owner);
    let ops: Vec<BatchOp> = serde_json::from_value(json!([
        { "op": "insert", "collection": "issues", "values": { "title": "b1" } },
        { "op": "insert", "collection": "projects", "values": { "name": "P", "key": "P1" } },
        { "op": "insert", "collection": "issues", "values": { "title": "b2" } },
    ]))
    .unwrap();
    let out = w.data().batch(&c, &ops).await.unwrap();
    assert_eq!(out.rows.len(), 3);
    assert_eq!(out.changes.len(), 3);
    assert!(out.changes.windows(2).all(|p| p[1].seq == p[0].seq + 1));
    let b1 = id(out.rows[0].as_ref().unwrap());
    // A failing operation rolls back the whole batch.
    let ops = vec![
        BatchOp::Update {
            ops: Default::default(),
            collection: "issues".into(),
            id: b1,
            patch: json!({ "title": "changed" }).as_object().unwrap().clone(),
            rev: None,
        },
        BatchOp::Insert {
            collection: "projects".into(),
            values: json!({ "name": "Q", "key": "p1" })
                .as_object()
                .unwrap()
                .clone(),
        },
    ];
    assert_eq!(
        w.data().batch(&c, &ops).await.unwrap_err().code(),
        ErrorCode::Conflict
    );
    assert_eq!(
        w.data().get(&c, "issues", b1).await.unwrap().unwrap()["title"],
        json!("b1")
    );
    assert_eq!(
        w.data()
            .count(&c, "issues", &Query::default())
            .await
            .unwrap(),
        2
    );
    let ops = vec![
        BatchOp::Delete {
            collection: "issues".into(),
            id: b1,
        },
        BatchOp::Restore {
            collection: "issues".into(),
            id: b1,
        },
    ];
    let out = w.data().batch(&c, &ops).await.unwrap();
    assert_eq!(out.changes.len(), 2);
}

#[tokio::test]
async fn field_constraints_and_transitions_are_enforced_on_writes_and_batches() {
    let Some(w) = World::new().await else { return };
    let defs: Vec<rookery_store::collections::CollectionDef> = serde_json::from_value(json!([{
        "name": "deals",
        "scope": "space",
        "fields": [
            {
                "name": "code",
                "type": "text",
                "required": true,
                "minLength": 2,
                "maxLength": 6,
                "pattern": "^[A-Z0-9]+$"
            },
            {
                "name": "budget",
                "type": "number",
                "format": "currency",
                "min": 0,
                "max": 1000
            },
            {
                "name": "state",
                "type": "enum",
                "values": [
                    { "value": "open", "title": "Open", "tone": "neutral" },
                    { "value": "review", "title": "In review", "tone": "accent" },
                    { "value": "won", "title": "Won", "tone": "positive" }
                ],
                "default": "open",
                "transitions": {
                    "open": ["review"],
                    "review": ["open", "won"]
                }
            }
        ]
    }]))
    .unwrap();
    w.db.store.collections().apply("crm", &defs).await.unwrap();
    let data = w.db.store.data("crm");
    let c = w.as_user(w.owner);

    for (payload, field, constraint) in [
        (json!({ "budget": 10 }), "code", "required"),
        (json!({ "code": "A" }), "code", "minLength"),
        (json!({ "code": "TOOLONG" }), "code", "maxLength"),
        (json!({ "code": "ab1" }), "code", "pattern"),
        (json!({ "code": "D1", "budget": -1 }), "budget", "min"),
        (json!({ "code": "D1", "budget": 1001 }), "budget", "max"),
        (json!({ "code": "D1", "state": "nope" }), "state", "enum"),
    ] {
        let e = data.insert(&c, "deals", payload).await.unwrap_err();
        assert_eq!(e.code(), ErrorCode::Invalid, "{field} {constraint}: {e}");
        assert_eq!(
            e.details(),
            Some(json!({ "collection": "deals", "field": field, "constraint": constraint })),
            "{field} {constraint}"
        );
    }

    let created = data
        .insert(&c, "deals", json!({ "code": "D1", "budget": 500 }))
        .await
        .unwrap()
        .row
        .unwrap();
    assert_eq!(created["state"], json!("open"));
    let row_id = id(&created);

    // Disallowed transition open -> won.
    let e = data
        .update(&c, "deals", row_id, json!({ "state": "won" }), None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid);
    assert_eq!(
        e.details(),
        Some(json!({ "collection": "deals", "field": "state", "constraint": "transitions" }))
    );

    // Allowed transitions open -> review -> won.
    let r1 = data
        .update(&c, "deals", row_id, json!({ "state": "review" }), None)
        .await
        .unwrap()
        .row
        .unwrap();
    assert_eq!(r1["state"], json!("review"));
    let r2 = data
        .update(&c, "deals", row_id, json!({ "state": "won" }), None)
        .await
        .unwrap()
        .row
        .unwrap();
    assert_eq!(r2["state"], json!("won"));

    // No-op update on terminal state passes.
    let noop = data
        .update(&c, "deals", row_id, json!({ "state": "won" }), None)
        .await
        .unwrap()
        .row
        .unwrap();
    assert_eq!(noop["state"], json!("won"));

    // Leaving terminal state fails.
    let e = data
        .update(&c, "deals", row_id, json!({ "state": "open" }), None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid);
    assert_eq!(
        e.details(),
        Some(json!({ "collection": "deals", "field": "state", "constraint": "transitions" }))
    );

    // Batch with one failing write rolls back the valid earlier operation.
    let ops = vec![
        BatchOp::Insert {
            collection: "deals".into(),
            values: json!({ "code": "D2", "budget": 100 })
                .as_object()
                .unwrap()
                .clone(),
        },
        BatchOp::Update {
            ops: Default::default(),
            collection: "deals".into(),
            id: row_id,
            patch: json!({ "budget": -5 }).as_object().unwrap().clone(),
            rev: None,
        },
    ];
    let e = data.batch(&c, &ops).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid);
    assert_eq!(
        e.details(),
        Some(json!({ "collection": "deals", "field": "budget", "constraint": "min" }))
    );
    assert_eq!(data.count(&c, "deals", &Query::default()).await.unwrap(), 1);
}
