//! The reads added after `find`: `select`, `nearest`, `include`, aggregates, `scan`, `import` and log collections.

use super::common::{World, defs, docs_defs, queries_of};
use rookery_proto::ErrorCode;
use rookery_store::Caller;
use rookery_store::collections::{CollectionDef, Query};
use rookery_store::testing::TestDb;
use serde_json::{Value, json};

fn admin() -> Caller {
    Caller::plugin("fixture").as_admin()
}

fn query(v: Value) -> Query {
    serde_json::from_value(v).expect("a query")
}

fn titles(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|r| r["title"].as_str().unwrap().to_string())
        .collect()
}

fn asset_defs() -> Vec<CollectionDef> {
    defs(json!([
        {"name":"assets","scope":"global","fields":[
            {"name":"title","type":"text"},
            {"name":"kind","type":"text","index":true},
            {"name":"n","type":"integer"},
            {"name":"raw","type":"bytes"},
            {"name":"embedding","type":"vector","dimensions":4,"metric":"cosine","index":"hnsw"},
            {"name":"flat","type":"vector","dimensions":2,"metric":"l2","index":"none"},
            {"name":"product","type":"vector","dimensions":2,"metric":"ip","index":"hnsw"}
        ]}
    ]))
}

// ── select ───────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn select_returns_the_named_fields_and_the_system_ones() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    if !super::common::has_pgvector(&db.store).await {
        return;
    }
    let s = db.store.clone();
    s.collections()
        .apply("datafix", &asset_defs())
        .await
        .unwrap();
    let data = s.data("datafix");
    for (i, title) in ["c", "a", "b"].iter().enumerate() {
        data.insert(
            &admin(),
            "assets",
            json!({"title": title, "kind": "x", "n": i, "raw": "AAEC", "embedding": [1, 0, 0, i]}),
        )
        .await
        .unwrap();
    }
    // By default the binary and vector fields are left out, everything else is there.
    let page = data
        .find(&admin(), "assets", &Query::default())
        .await
        .unwrap();
    let row = &page.rows[0];
    assert!(
        row.get("raw").is_none() && row.get("embedding").is_none() && row.get("flat").is_none()
    );
    assert!(row.get("title").is_some() && row.get("kind").is_some() && row.get("n").is_some());

    // `select` returns the named fields; the system fields are always there.
    let page = data
        .find(
            &admin(),
            "assets",
            &query(json!({"select": ["title"], "orderBy": ["title"]})),
        )
        .await
        .unwrap();
    assert_eq!(titles(&page.rows), ["a", "b", "c"]);
    let keys: Vec<&str> = page.rows[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for k in [
        "id",
        "createdAt",
        "updatedAt",
        "createdBy",
        "updatedBy",
        "rev",
        "title",
    ] {
        assert!(keys.contains(&k), "{k} in {keys:?}");
    }
    for k in ["kind", "n", "raw", "embedding"] {
        assert!(!keys.contains(&k), "{k} not in {keys:?}");
    }

    // Named binary fields come back, in their API form.
    let page = data
        .find(
            &admin(),
            "assets",
            &query(json!({"select": ["raw", "embedding"], "limit": 1})),
        )
        .await
        .unwrap();
    assert_eq!(page.rows[0]["raw"], json!("AAEC"));
    assert_eq!(page.rows[0]["embedding"].as_array().unwrap().len(), 4);
    assert!(page.rows[0].get("title").is_none());

    // An empty selection is the system fields alone; system names are accepted and change nothing.
    let page = data
        .find(&admin(), "assets", &query(json!({"select": ["id", "rev"]})))
        .await
        .unwrap();
    assert!(page.rows[0].get("title").is_none() && page.rows[0]["rev"] == json!(1));

    // The order of the selection does not matter, and a field not selected can still order the page and
    // carry the cursor.
    let first = data
        .find(
            &admin(),
            "assets",
            &query(json!({"select": ["title"], "orderBy": [["n", "desc"]], "limit": 2})),
        )
        .await
        .unwrap();
    assert_eq!(titles(&first.rows), ["b", "a"]);
    assert!(first.rows[0].get("n").is_none());
    let rest = data
        .find(
            &admin(),
            "assets",
            &query(json!({"select": ["title"], "orderBy": [["n", "desc"]], "limit": 2, "after": first.next})),
        )
        .await
        .unwrap();
    assert_eq!(titles(&rest.rows), ["c"]);

    // An unknown name is refused.
    let e = data
        .find(&admin(), "assets", &query(json!({"select": ["nosuch"]})))
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid);
    assert!(e.to_string().contains("nosuch"));
    db.close().await;
}

#[tokio::test]
async fn select_applies_after_access_and_scope() {
    let Some(w) = World::new().await else { return };
    let data = w.data();
    let secret = data
        .insert(
            &w.as_user(w.owner),
            "projects",
            json!({"name": "secret", "private": true}),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let open = data
        .insert(&w.as_user(w.owner), "projects", json!({"name": "open"}))
        .await
        .unwrap()
        .row
        .unwrap();
    // Selecting fields does not widen what a caller may read: the member sees only the open project, and the
    // outsider, who is not in the space, sees none.
    let q = query(json!({"select": ["name"], "orderBy": ["name"]}));
    let page = data
        .find(&w.as_user(w.member), "projects", &q)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0]["id"], open["id"]);
    assert!(page.rows[0].get("private").is_none());
    assert_ne!(page.rows[0]["id"], secret["id"]);
    let page = data
        .find(&w.as_user(w.outsider), "projects", &q)
        .await
        .unwrap();
    assert!(page.rows.is_empty());
    w.db.close().await;
}

// ── nearest ──────────────────────────────────────────────────────────────────────────────────────

async fn seed_assets(data: &rookery_store::collections::Data<'_>) {
    // Cosine distance from [1, 0, 0, 0]: a < b < c < d; e has no vector.
    for (title, kind, v) in [
        ("a", "image", Some(json!([1, 0, 0, 0]))),
        ("b", "image", Some(json!([1, 1, 0, 0]))),
        ("c", "doc", Some(json!([1, 2, 0, 0]))),
        ("d", "image", Some(json!([0, 1, 0, 0]))),
        ("e", "image", None),
    ] {
        let mut row = json!({"title": title, "kind": kind});
        if let Some(v) = v {
            row["embedding"] = v;
        }
        data.insert(&admin(), "assets", row).await.unwrap();
    }
}

#[tokio::test]
async fn nearest_orders_by_distance_and_combines_with_filters_and_limits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    if !super::common::has_pgvector(&db.store).await {
        return;
    }
    let s = db.store.clone();
    s.collections()
        .apply("datafix", &asset_defs())
        .await
        .unwrap();
    let data = s.data("datafix");
    seed_assets(&data).await;
    let near = |extra: Value| {
        let mut q = json!({"nearest": {"field": "embedding", "to": [1, 0, 0, 0]}});
        for (k, v) in extra.as_object().unwrap() {
            q[k] = v.clone();
        }
        query(q)
    };
    let page = data
        .find(&admin(), "assets", &near(json!({})))
        .await
        .unwrap();
    assert_eq!(
        titles(&page.rows),
        ["a", "b", "c", "d"],
        "the row without a vector is not a candidate"
    );
    assert!(page.next.is_none());
    let d: Vec<f64> = page
        .rows
        .iter()
        .map(|r| r["_distance"].as_f64().unwrap())
        .collect();
    assert!(d[0].abs() < 1e-6, "{d:?}");
    assert!((d[1] - (1.0 - 1.0 / 2f64.sqrt())).abs() < 1e-5, "{d:?}");
    assert!(d.windows(2).all(|w| w[0] <= w[1]));
    // The vector itself is not returned unless selected.
    assert!(page.rows[0].get("embedding").is_none());

    let page = data
        .find(&admin(), "assets", &near(json!({"limit": 2})))
        .await
        .unwrap();
    assert_eq!(titles(&page.rows), ["a", "b"]);
    // maxDistance: cosine distance 0.5 admits a (0) and b (0.29), not c (0.55) or d (1).
    let page = data
        .find(
            &admin(),
            "assets",
            &near(
                json!({"nearest": {"field": "embedding", "to": [1, 0, 0, 0], "maxDistance": 0.5}}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(titles(&page.rows), ["a", "b"]);
    // With a filter: the nearest rows that match it, not the matches among the nearest.
    let page = data
        .find(&admin(), "assets", &near(json!({"where": {"kind": "doc"}})))
        .await
        .unwrap();
    assert_eq!(titles(&page.rows), ["c"]);
    let page = data
        .find(
            &admin(),
            "assets",
            &near(json!({"where": {"kind": "image"}, "limit": 2})),
        )
        .await
        .unwrap();
    assert_eq!(titles(&page.rows), ["a", "b"]);
    // select applies, and may name the vector.
    let page = data
        .find(
            &admin(),
            "assets",
            &near(json!({"select": ["title", "embedding"], "limit": 1})),
        )
        .await
        .unwrap();
    assert_eq!(page.rows[0]["embedding"], json!([1.0, 0.0, 0.0, 0.0]));
    assert!(page.rows[0].get("kind").is_none() && page.rows[0]["_distance"].is_number());
    db.close().await;
}

#[tokio::test]
async fn nearest_uses_the_fields_metric_and_refuses_what_it_cannot_do() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    if !super::common::has_pgvector(&db.store).await {
        return;
    }
    let s = db.store.clone();
    s.collections()
        .apply("datafix", &asset_defs())
        .await
        .unwrap();
    let data = s.data("datafix");
    for (title, flat, product) in [
        ("p", [0, 0], [1, 0]),
        ("q", [3, 4], [2, 0]),
        ("r", [1, 1], [-1, 0]),
    ] {
        data.insert(
            &admin(),
            "assets",
            json!({"title": title, "flat": flat, "product": product}),
        )
        .await
        .unwrap();
    }
    // l2: Euclidean distance.
    let page = data
        .find(
            &admin(),
            "assets",
            &query(json!({"nearest": {"field": "flat", "to": [0, 0]}})),
        )
        .await
        .unwrap();
    assert_eq!(titles(&page.rows), ["p", "r", "q"]);
    assert!((page.rows[2]["_distance"].as_f64().unwrap() - 5.0).abs() < 1e-5);
    // ip: the largest inner product first, reported as pgvector does, negated.
    let page = data
        .find(
            &admin(),
            "assets",
            &query(json!({"nearest": {"field": "product", "to": [1, 0]}})),
        )
        .await
        .unwrap();
    assert_eq!(titles(&page.rows), ["q", "p", "r"]);
    assert!((page.rows[0]["_distance"].as_f64().unwrap() + 2.0).abs() < 1e-5);

    let near = json!({"field": "embedding", "to": [1, 0, 0, 0]});
    for (bad, why) in [
        (
            json!({"nearest": {"field": "title", "to": [1]}}),
            "not a vector",
        ),
        (
            json!({"nearest": {"field": "nosuch", "to": [1]}}),
            "unknown",
        ),
        (
            json!({"nearest": {"field": "embedding", "to": [1, 0]}}),
            "dimensions",
        ),
        (json!({"nearest": near, "orderBy": ["title"]}), "orderBy"),
        (json!({"nearest": near, "after": "x"}), "after"),
        (json!({"nearest": near, "search": "x"}), "search"),
        (json!({"nearest": near, "limit": 201}), "limit"),
        (json!({"nearest": near, "limit": 0}), "limit"),
    ] {
        let e = data
            .find(&admin(), "assets", &query(bad))
            .await
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Invalid, "{why}: {e}");
    }
    // 200 is allowed.
    data.find(
        &admin(),
        "assets",
        &query(json!({"nearest": near, "limit": 200})),
    )
    .await
    .unwrap();
    // A collection with no vector field has nothing to search.
    s.collections()
        .apply(
            "plain",
            &defs(json!([{"name":"notes","scope":"global","fields":[{"name":"t","type":"text"}]}])),
        )
        .await
        .unwrap();
    let e = s
        .data("plain")
        .find(
            &admin(),
            "notes",
            &query(json!({"nearest": {"field": "t", "to": [1]}})),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid);
    db.close().await;
}

#[tokio::test]
async fn nearest_is_scoped_and_access_checked() {
    let Some(w) = World::new().await else { return };
    if !super::common::has_pgvector(&w.db.store).await {
        return;
    }
    w.db.store
        .collections()
        .apply(
            "vecs",
            &defs(json!([
                {"name":"notes","scope":"space","entity":{"scheme":"vnote","title":"title","private":"private"},
                 "fields":[
                    {"name":"title","type":"text"},{"name":"private","type":"boolean"},
                    {"name":"embedding","type":"vector","dimensions":2,"metric":"cosine","index":"hnsw"}]}
            ])),
        )
        .await
        .unwrap();
    let data = w.db.store.data("vecs");
    let owner = w.as_user(w.owner);
    for (title, private, v) in [
        ("near-private", true, [1, 0]),
        ("mid-open", false, [1, 1]),
        ("far-open", false, [0, 1]),
    ] {
        data.insert(
            &owner,
            "notes",
            json!({"title": title, "private": private, "embedding": v}),
        )
        .await
        .unwrap();
    }
    let q = query(json!({"nearest": {"field": "embedding", "to": [1, 0]}}));
    let all = data.find(&owner, "notes", &q).await.unwrap();
    assert_eq!(titles(&all.rows), ["near-private", "mid-open", "far-open"]);
    // The member may not read the private row, and it does not take a slot from the page.
    let member = data
        .find(
            &w.as_user(w.member),
            "notes",
            &query(json!({"nearest": {"field": "embedding", "to": [1, 0]}, "limit": 1})),
        )
        .await
        .unwrap();
    assert_eq!(titles(&member.rows), ["mid-open"]);
    // Another space has none of the rows.
    let other =
        w.db.store
            .spaces()
            .create(
                &Caller::user(w.owner),
                &rookery_store::spaces::NewSpace {
                    kind: "project".into(),
                    name: "Other".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .id;
    let elsewhere = data
        .find(&Caller::user(w.owner).in_space(other), "notes", &q)
        .await
        .unwrap();
    assert!(elsewhere.rows.is_empty());
    w.db.close().await;
}

#[tokio::test]
async fn nearest_uses_the_hnsw_index() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    if !super::common::has_pgvector(&db.store).await {
        return;
    }
    let s = db.store.clone();
    s.collections()
        .apply(
            "datafix",
            &defs(json!([{"name":"assets","scope":"global","fields":[
                {"name":"title","type":"text"},{"name":"kind","type":"text","index":true},
                {"name":"embedding","type":"vector","dimensions":8,"metric":"cosine","index":"hnsw"},
                {"name":"unindexed","type":"vector","dimensions":8,"metric":"cosine","index":"none"}]}])),
        )
        .await
        .unwrap();
    // Enough rows that Postgres prefers the index to scanning and sorting everything.
    sqlx::query(
        "INSERT INTO p_datafix.assets (id, title, kind, embedding, unindexed) \
         SELECT gen_random_uuid(), 'a' || g, CASE WHEN g % 10 = 0 THEN 'rare' ELSE 'common' END, v, v \
         FROM (SELECT g, ('[' || (SELECT string_agg(random()::text, ',') FROM generate_series(1, 8) WHERE g > 0) || ']')::vector AS v \
               FROM generate_series(1, 5000) g) rows",
    )
    .execute(s.pool())
    .await
    .unwrap();
    sqlx::query("ANALYZE p_datafix.assets")
        .execute(s.pool())
        .await
        .unwrap();
    let to: Vec<f64> = vec![0.5; 8];
    let plan = |extra: Value| {
        let mut q = json!({"nearest": {"field": "embedding", "to": to}, "limit": 10});
        for (k, v) in extra.as_object().unwrap() {
            q[k] = v.clone();
        }
        let data = s.data("datafix");
        async move {
            data.explain_nearest(&admin(), "assets", &query(q), &[])
                .await
                .unwrap()
                .join("\n")
        }
    };
    let p = plan(json!({})).await;
    assert!(p.contains("assets_") || p.contains("Index Scan"), "{p}");
    assert!(
        p.contains("Index Scan using") && p.contains("embedding"),
        "the index is scanned:\n{p}"
    );
    assert!(!p.contains("Seq Scan"), "{p}");
    let p = plan(json!({"where": {"kind": "rare"}})).await;
    assert!(
        p.contains("Index Scan using"),
        "a filter is applied to what the index yields:\n{p}"
    );
    // Without an index the same query reads the table.
    let p = plan(json!({"nearest": {"field": "unindexed", "to": to}})).await;
    assert!(p.contains("Seq Scan") || p.contains("Sort"), "{p}");
    // The index is the declared one.
    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE schemaname = 'p_datafix'")
            .fetch_all(s.pool())
            .await
            .unwrap();
    assert!(
        indexes
            .iter()
            .any(|i| i.contains("USING hnsw") && i.contains("vector_cosine_ops"))
    );

    // The results are what exact search gives for the nearest rows that match.
    let data = s.data("datafix");
    let got = data
        .find(
            &admin(),
            "assets",
            &query(
                json!({"nearest": {"field": "embedding", "to": to}, "where": {"kind": "rare"}, "limit": 20}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        got.rows.len(),
        20,
        "iterative scanning fills the page despite the filter"
    );
    let exact: Vec<String> = sqlx::query_scalar(
        "SELECT title FROM p_datafix.assets WHERE kind = 'rare' ORDER BY unindexed <=> $1::vector LIMIT 20",
    )
    .bind(format!("[{}]", ["0.5"; 8].join(",")))
    .fetch_all(s.pool())
    .await
    .unwrap();
    let got_titles = titles(&got.rows);
    let overlap = got_titles.iter().filter(|t| exact.contains(t)).count();
    assert!(
        overlap >= 18,
        "the approximate index finds nearly the exact set: {overlap}/20"
    );
    db.close().await;
}

#[tokio::test]
async fn nearest_says_when_pgvector_is_missing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    if !super::common::has_pgvector(&db.store).await {
        return;
    }
    let s = db.store.clone();
    s.collections()
        .apply("datafix", &asset_defs())
        .await
        .unwrap();
    let data = s.data("datafix");
    seed_assets(&data).await;
    sqlx::query("DROP EXTENSION vector CASCADE")
        .execute(s.pool())
        .await
        .unwrap();
    let e = data
        .find(
            &admin(),
            "assets",
            &query(json!({"nearest": {"field": "embedding", "to": [1, 0, 0, 0]}})),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Unavailable, "{e}");
    assert!(e.to_string().contains("pgvector"), "{e}");
    db.close().await;
}

// ── include ──────────────────────────────────────────────────────────────────────────────────────

async fn issue(w: &World, c: &Caller, title: &str, project: Option<&Value>) -> Value {
    let mut v = json!({"title": title});
    if let Some(p) = project {
        v["project"] = p["id"].clone();
    }
    w.data().insert(c, "issues", v).await.unwrap().row.unwrap()
}

#[tokio::test]
async fn include_returns_referenced_rows_by_their_uri() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    let open = w
        .data()
        .insert(&owner, "projects", json!({"name": "open"}))
        .await
        .unwrap()
        .row
        .unwrap();
    let secret = w
        .data()
        .insert(
            &owner,
            "projects",
            json!({"name": "secret", "private": true}),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    issue(&w, &owner, "one", Some(&open)).await;
    issue(&w, &owner, "two", Some(&open)).await;
    issue(&w, &owner, "three", Some(&secret)).await;
    issue(&w, &owner, "none", None).await;
    let q = query(json!({"include": ["project"], "orderBy": ["number"]}));

    let page = w.data().find(&owner, "issues", &q).await.unwrap();
    assert_eq!(page.rows.len(), 4);
    let included = page.included.unwrap();
    let open_uri = format!("project://{}", open["id"].as_str().unwrap());
    let secret_uri = format!("project://{}", secret["id"].as_str().unwrap());
    assert_eq!(included.len(), 2);
    assert_eq!(included[&open_uri]["name"], json!("open"));
    assert_eq!(included[&secret_uri]["name"], json!("secret"));
    // Without `include` there is nothing beside the page.
    assert!(
        w.data()
            .find(&owner, "issues", &Query::default())
            .await
            .unwrap()
            .included
            .is_none()
    );

    // A member may not read the private project, nor the issue below it: neither is returned.
    let page = w
        .data()
        .find(&w.as_user(w.member), "issues", &q)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 3);
    let included = page.included.unwrap();
    assert_eq!(included.keys().collect::<Vec<_>>(), [&open_uri]);

    // The page decides what is included: a filter that leaves out the issues leaves out their projects.
    let page = w
        .data()
        .find(
            &owner,
            "issues",
            &query(json!({"include": ["project"], "where": {"title": "three"}})),
        )
        .await
        .unwrap();
    assert_eq!(
        page.included.unwrap().keys().collect::<Vec<_>>(),
        [&secret_uri]
    );
    // Included rows leave out binary and vector fields like any read, and are one level: no `included` inside.
    assert!(page.rows[0].get("included").is_none());
    w.db.close().await;
}

#[tokio::test]
async fn include_reaches_other_plugins_collections_and_refs_fields() {
    let Some(w) = World::new().await else { return };
    w.db.store
        .collections()
        .apply("docs", &docs_defs())
        .await
        .unwrap();
    let owner = w.as_user(w.owner);
    let secret = w
        .data()
        .insert(
            &owner,
            "projects",
            json!({"name": "secret", "private": true}),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let one = issue(&w, &owner, "one", None).await;
    let two = issue(&w, &owner, "two", Some(&secret)).await;
    let uri = |r: &Value| format!("issue://{}", r["id"].as_str().unwrap());
    let docs = w.db.store.data("docs");
    docs.insert(
        &owner,
        "pages",
        json!({"title": "page", "related": [uri(&one), uri(&two), "user://not-a-collection", "issue://not-a-uuid"]}),
    )
    .await
    .unwrap();
    let page = docs
        .find(&owner, "pages", &query(json!({"include": ["related"]})))
        .await
        .unwrap();
    // The docs plugin reads the issues plugin's rows, as the caller may.
    let included = page.included.unwrap();
    assert_eq!(included.len(), 2, "{included:?}");
    assert_eq!(included[&uri(&one)]["title"], json!("one"));
    assert_eq!(included[&uri(&two)]["title"], json!("two"));
    // A member may read both issues (the second one's parent is private, so it needs a grant it lacks).
    let page = docs
        .find(
            &w.as_user(w.member),
            "pages",
            &query(json!({"include": ["related"]})),
        )
        .await
        .unwrap();
    let included = page.included.unwrap();
    assert!(included.contains_key(&uri(&one)));
    assert!(
        !included.contains_key(&uri(&two)),
        "an issue below a private project is not readable"
    );
    w.db.close().await;
}

#[tokio::test]
async fn include_checks_its_fields_and_costs_a_fixed_number_of_queries() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    let mut projects = Vec::new();
    for n in 0..12 {
        projects.push(
            w.data()
                .insert(&owner, "projects", json!({"name": format!("p{n}")}))
                .await
                .unwrap()
                .row
                .unwrap(),
        );
    }
    for n in 0..40 {
        issue(&w, &owner, &format!("i{n}"), Some(&projects[n % 12])).await;
    }
    for (bad, why) in [
        (json!({"include": ["title"]}), "not a ref"),
        (json!({"include": ["nosuch"]}), "unknown"),
        (json!({"include": ["project", "project"]}), "twice"),
        (
            json!({"include": ["project"], "select": ["title"]}),
            "not selected",
        ),
        (
            json!({"include": ["project", "assignee", "labels", "project2", "x", "y"]}),
            "six",
        ),
    ] {
        let e = w
            .data()
            .find(&owner, "issues", &query(bad))
            .await
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Invalid, "{why}: {e}");
    }
    w.data()
        .find(&owner, "issues", &query(json!({"include": ["project", "assignee", "labels"], "select": ["project", "assignee", "labels"]})))
        .await
        .unwrap();

    // The number of statements does not grow with the rows or the references.
    let (small, page) = queries_of(w.data().find(
        &owner,
        "issues",
        &query(json!({"include": ["project"], "limit": 2})),
    ))
    .await;
    assert_eq!(page.unwrap().included.unwrap().len(), 2);
    let (large, page) = queries_of(w.data().find(
        &owner,
        "issues",
        &query(json!({"include": ["project"], "limit": 40})),
    ))
    .await;
    let page = page.unwrap();
    assert_eq!(page.rows.len(), 40);
    assert_eq!(page.included.unwrap().len(), 12);
    let (plain, _) = queries_of(
        w.data()
            .find(&owner, "issues", &query(json!({"limit": 40}))),
    )
    .await;
    assert!(!small.is_empty() && !plain.is_empty());
    assert_eq!(
        small.len(),
        large.len(),
        "{} statements for 2 rows, {} for 40:\n{small:#?}\n{large:#?}",
        small.len(),
        large.len()
    );
    assert!(
        large.len() <= plain.len() + 3,
        "the references are read with a few more statements ({} against {}), not one per row:\n{large:#?}\n{plain:#?}",
        large.len(),
        plain.len()
    );
    w.db.close().await;
}

// ── aggregates ───────────────────────────────────────────────────────────────────────────────────

fn aggregate_query(v: Value) -> rookery_store::collections::AggregateQuery {
    serde_json::from_value(v).expect("an aggregate")
}

async fn seed_issues(w: &World, c: &Caller, project: Option<&Value>) {
    for (title, state, estimate, due, closed) in [
        (
            "a",
            "open",
            Some(2.0),
            Some("2026-11-09"),
            Some("2026-11-09T23:30:00-05:00"),
        ),
        (
            "b",
            "open",
            Some(3.5),
            Some("2026-11-11"),
            Some("2026-11-10T08:00:00Z"),
        ),
        ("c", "done", Some(5.0), Some("2026-11-16"), None),
        ("d", "done", None, Some("2026-12-01"), None),
        ("e", "open", Some(1.0), None, None),
    ] {
        let mut v = json!({"title": title, "state": state, "estimate": estimate, "due": due, "closedAt": closed});
        if let Some(p) = project {
            v["project"] = p["id"].clone();
        }
        w.data().insert(c, "issues", v).await.unwrap();
    }
}

#[tokio::test]
async fn aggregates_group_and_measure() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    seed_issues(&w, &owner, None).await;
    let data = w.data();

    // Counts and sums by a group key, ordered by a measure.
    let rows = data
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(json!({
                "groupBy": ["state"],
                "measures": {
                    "n": {"count": true}, "points": {"sum": "estimate"}, "mean": {"avg": "estimate"},
                    "first": {"min": "title"}, "last": {"max": "title"}, "dues": {"countDistinct": "due"},
                    "latest": {"max": "due"}
                },
                "orderBy": [["n", "desc"]]
            })),
        )
        .await
        .unwrap();
    assert_eq!(
        rows,
        [
            json!({"state": "open", "n": 3, "points": 6.5, "mean": 6.5 / 3.0, "first": "a", "last": "e", "dues": 2, "latest": "2026-11-11"}),
            json!({"state": "done", "n": 2, "points": 5, "mean": 5, "first": "c", "last": "d", "dues": 2, "latest": "2026-12-01"}),
        ]
    );

    // Buckets: weeks start on Monday, in UTC; the key keeps the field's name.
    let rows = data
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(json!({
                "groupBy": [{"field": "due", "by": "week"}],
                "measures": {"n": {"count": true}}
            })),
        )
        .await
        .unwrap();
    assert_eq!(
        rows,
        [
            json!({"due": "2026-11-09", "n": 2}),
            json!({"due": "2026-11-16", "n": 1}),
            json!({"due": "2026-11-30", "n": 1}),
            json!({"due": null, "n": 1}),
        ]
    );
    let months = data
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(json!({"groupBy": [{"field": "due", "by": "month"}], "measures": {"n": {"count": true}}, "where": {"due": {"ne": null}}})),
        )
        .await
        .unwrap();
    assert_eq!(
        months,
        [
            json!({"due": "2026-11-01", "n": 3}),
            json!({"due": "2026-12-01", "n": 1})
        ]
    );
    // A datetime is bucketed in UTC: 23:30 at -05:00 on the 9th is the 10th.
    let days = data
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(json!({"groupBy": [{"field": "closedAt", "by": "day"}], "measures": {"n": {"count": true}}, "where": {"closedAt": {"ne": null}}})),
        )
        .await
        .unwrap();
    assert_eq!(days, [json!({"closedAt": "2026-11-10", "n": 2})]);
    // Two keys, and the filter applies before grouping.
    let rows = data
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(json!({
                "where": {"estimate": {"gte": 2}},
                "groupBy": ["state", {"field": "due", "by": "month"}],
                "measures": {"n": {"count": true}},
                "orderBy": [["n", "desc"], "state"]
            })),
        )
        .await
        .unwrap();
    assert_eq!(
        rows,
        [
            json!({"state": "open", "due": "2026-11-01", "n": 2}),
            json!({"state": "done", "due": "2026-11-01", "n": 1}),
        ]
    );

    // No keys: one row, even over nothing.
    let rows = data
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(json!({"measures": {"n": {"count": true}, "s": {"sum": "estimate"}}})),
        )
        .await
        .unwrap();
    assert_eq!(rows, [json!({"n": 5, "s": 11.5})]);
    let rows = data
        .aggregate(&owner, "issues", &aggregate_query(json!({"where": {"title": "zzz"}, "measures": {"n": {"count": true}, "s": {"sum": "estimate"}}})))
        .await
        .unwrap();
    assert_eq!(rows, [json!({"n": 0, "s": null})]);
    // System fields work as keys and measures.
    let rows = data
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(
                json!({"groupBy": ["createdBy"], "measures": {"newest": {"max": "rev"}}}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        rows,
        [json!({"createdBy": w.owner.to_string(), "newest": 1})]
    );
    w.db.close().await;
}

#[tokio::test]
async fn aggregates_see_only_what_the_caller_may_read() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    let open = w
        .data()
        .insert(&owner, "projects", json!({"name": "open"}))
        .await
        .unwrap()
        .row
        .unwrap();
    let secret = w
        .data()
        .insert(
            &owner,
            "projects",
            json!({"name": "secret", "private": true}),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    seed_issues(&w, &owner, Some(&open)).await;
    seed_issues(&w, &owner, Some(&secret)).await;
    let by_project = aggregate_query(json!({
        "groupBy": ["project"],
        "measures": {"n": {"count": true}, "points": {"sum": "estimate"}}
    }));
    let all = w
        .data()
        .aggregate(&owner, "issues", &by_project)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|g| g["n"] == json!(5)));
    // The member may not read the issues below the private project: they are in no group and no total.
    let member = w.as_user(w.member);
    let seen = w
        .data()
        .aggregate(&member, "issues", &by_project)
        .await
        .unwrap();
    assert_eq!(
        seen,
        [
            json!({"project": format!("project://{}", open["id"].as_str().unwrap()), "n": 5, "points": 11.5})
        ]
    );
    let total = w
        .data()
        .aggregate(
            &member,
            "issues",
            &aggregate_query(json!({"measures": {"n": {"count": true}}})),
        )
        .await
        .unwrap();
    assert_eq!(total, [json!({"n": 5})]);
    // Nor does another space: its rows are not counted.
    let other =
        w.db.store
            .spaces()
            .create(
                &Caller::user(w.owner),
                &rookery_store::spaces::NewSpace {
                    kind: "project".into(),
                    name: "Other".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .id;
    let elsewhere = w
        .data()
        .aggregate(
            &Caller::user(w.owner).in_space(other),
            "issues",
            &aggregate_query(json!({"measures": {"n": {"count": true}}})),
        )
        .await
        .unwrap();
    assert_eq!(elsewhere, [json!({"n": 0})]);
    // A user outside the space reads nothing.
    let none = w
        .data()
        .aggregate(
            &w.as_user(w.outsider),
            "issues",
            &aggregate_query(json!({"measures": {"n": {"count": true}}})),
        )
        .await
        .unwrap();
    assert_eq!(none, [json!({"n": 0})]);
    w.db.close().await;
}

#[tokio::test]
async fn aggregates_refuse_what_they_cannot_do_and_cap_the_groups() {
    let Some(w) = World::new().await else { return };
    let owner = w.as_user(w.owner);
    seed_issues(&w, &owner, None).await;
    let count = json!({"n": {"count": true}});
    for (bad, why) in [
        (json!({"measures": {}}), "no measures"),
        (
            json!({"groupBy": ["a", "b", "c", "d"], "measures": count}),
            "four keys",
        ),
        (
            json!({"groupBy": ["nosuch"], "measures": count}),
            "unknown key",
        ),
        (json!({"groupBy": ["meta"], "measures": count}), "json key"),
        (
            json!({"groupBy": ["labels"], "measures": count}),
            "refs key",
        ),
        (
            json!({"groupBy": [{"field": "title", "by": "week"}], "measures": count}),
            "bucket on text",
        ),
        (
            json!({"groupBy": ["state"], "measures": {"state": {"count": true}}}),
            "name clash",
        ),
        (json!({"measures": {"x": {"sum": "title"}}}), "sum of text"),
        (json!({"measures": {"x": {"avg": "due"}}}), "avg of date"),
        (json!({"measures": {"x": {"min": "meta"}}}), "min of json"),
        (
            json!({"measures": {"x": {"sum": "estimate", "count": true}}}),
            "two functions",
        ),
        (json!({"measures": {"x": {}}}), "no function"),
        (
            json!({"measures": {"x": {"median": "estimate"}}}),
            "unknown function",
        ),
        (
            json!({"measures": {"bad name": {"count": true}}}),
            "bad name",
        ),
        (
            json!({"measures": count, "orderBy": ["nosuch"]}),
            "order by unknown",
        ),
        (json!({"measures": count, "limit": 0}), "limit"),
        (json!({"measures": count, "limit": 1001}), "limit"),
    ] {
        let parsed = serde_json::from_value::<rookery_store::collections::AggregateQuery>(bad);
        let e = match parsed {
            Err(_) => continue, // refused by the shape of the request
            Ok(q) => w.data().aggregate(&owner, "issues", &q).await.unwrap_err(),
        };
        assert_eq!(e.code(), ErrorCode::Invalid, "{why}: {e}");
    }
    // More than 1,000 groups is an error, unless the groups are cut with a limit.
    for chunk in (0..1001).collect::<Vec<i32>>().chunks(500) {
        let ops: Vec<_> = chunk
            .iter()
            .map(|i| rookery_store::collections::BatchOp::Insert {
                collection: "issues".into(),
                values: json!({"title": format!("t{i:04}")})
                    .as_object()
                    .unwrap()
                    .clone(),
            })
            .collect();
        w.data().batch(&owner, &ops).await.unwrap();
    }
    let q = aggregate_query(json!({"groupBy": ["title"], "measures": count}));
    let e = w.data().aggregate(&owner, "issues", &q).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid);
    assert!(e.to_string().contains("1000"), "{e}");
    let cut = w
        .data()
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(
                json!({"groupBy": ["title"], "measures": count, "orderBy": [["title", "desc"]], "limit": 3}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        cut.iter()
            .map(|g| g["title"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["t1000", "t0999", "t0998"]
    );
    let exactly = w
        .data()
        .aggregate(
            &owner,
            "issues",
            &aggregate_query(json!({"groupBy": ["title"], "measures": count, "limit": 1000})),
        )
        .await
        .unwrap();
    assert_eq!(exactly.len(), 1000);
    w.db.close().await;
}

// ── scan and import ──────────────────────────────────────────────────────────────────────────────

fn import_options(v: Value) -> rookery_store::collections::ImportOptions {
    serde_json::from_value(v).expect("import options")
}

fn note_defs() -> Vec<CollectionDef> {
    defs(json!([
        {"name":"notes","scope":"global","history":true,"fields":[
            {"name":"key","type":"text","unique":true},
            {"name":"title","type":"text","required":true,"maxLength":8},
            {"name":"n","type":"integer","min":0},
            {"name":"seq","type":"sequence"},
            {"name":"raw","type":"bytes","maxBytes":4}
        ]}
    ]))
}

#[tokio::test]
async fn scan_pages_in_id_order_and_survives_inserts_and_deletes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    s.collections()
        .apply("scanfix", &note_defs())
        .await
        .unwrap();
    let data = s.data("scanfix");
    let c = admin();
    let mut ids = Vec::new();
    for n in 0..25 {
        let row = data
            .insert(
                &c,
                "notes",
                json!({"key": format!("k{n:02}"), "title": "t", "n": n}),
            )
            .await
            .unwrap()
            .row
            .unwrap();
        ids.push(row["id"].as_str().unwrap().to_string());
    }
    // A pass in pages of 10: the rows come in id order, each once.
    let first = data
        .scan(&c, "notes", &query(json!({"limit": 10, "select": ["key"]})))
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 10);
    assert!(
        first.rows[0].get("title").is_none(),
        "select applies to a scan"
    );
    let mut seen: Vec<String> = first
        .rows
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();

    // Between pages: rows are deleted (one already read, two not yet) and inserted (newer ids).
    data.delete(&c, "notes", ids[3].parse().unwrap())
        .await
        .unwrap();
    data.delete(&c, "notes", ids[12].parse().unwrap())
        .await
        .unwrap();
    data.delete(&c, "notes", ids[20].parse().unwrap())
        .await
        .unwrap();
    let mut added = Vec::new();
    for n in 0..3 {
        let row = data
            .insert(&c, "notes", json!({"key": format!("new{n}"), "title": "t"}))
            .await
            .unwrap()
            .row
            .unwrap();
        added.push(row["id"].as_str().unwrap().to_string());
    }
    let mut after = first.next;
    while let Some(cursor) = after {
        let page = data
            .scan(
                &c,
                "notes",
                &query(json!({"limit": 10, "select": ["key"], "after": cursor})),
            )
            .await
            .unwrap();
        seen.extend(
            page.rows
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_string()),
        );
        after = page.next;
    }
    // Every row that existed throughout is there once; the deleted rows not yet read are not; the rows
    // inserted after the pass began (newer ids) are.
    let mut expected: Vec<String> = ids.clone();
    expected.retain(|id| id != &ids[12] && id != &ids[20]);
    expected.extend(added);
    assert_eq!(seen, expected);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(sorted, seen, "id order");

    // The cursor can be resumed from later, and the default page is 1,000 rows.
    let all = data.scan(&c, "notes", &Query::default()).await.unwrap();
    assert_eq!(all.rows.len(), 25);
    assert!(all.next.is_none());
    for bad in [
        json!({"orderBy": ["key"]}),
        json!({"search": "x"}),
        json!({"include": ["x"]}),
        json!({"limit": 1001}),
    ] {
        let e = data.scan(&c, "notes", &query(bad)).await.unwrap_err();
        assert_eq!(e.code(), ErrorCode::Invalid);
    }
    // A filter narrows the pass.
    let some = data
        .scan(&c, "notes", &query(json!({"where": {"n": {"gte": 20}}})))
        .await
        .unwrap();
    assert_eq!(some.rows.len(), 4, "20..=24 less the deleted 20");
    db.close().await;
}

#[tokio::test]
async fn import_writes_rows_in_one_transaction_and_reports_the_rows_that_fail() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    s.collections()
        .apply("scanfix", &note_defs())
        .await
        .unwrap();
    let data = s.data("scanfix");
    let c = admin();
    let mut feed = s.feed().subscribe().await.unwrap();
    let rows: Vec<Value> = (0..20)
        .map(|n| match n {
            3 => json!({"key": "k3", "title": "far too long a title"}),
            7 => json!({"key": "k7"}),
            11 => json!({"key": "k0", "title": "dup"}),
            13 => json!({"key": "k13", "title": "t", "raw": "AAECAwQ="}),
            15 => json!("not an object"),
            17 => json!({"key": "k17", "title": "t", "nosuch": 1}),
            _ => json!({"key": format!("k{n}"), "title": "t", "n": n}),
        })
        .collect();
    let out = data
        .import(
            &c,
            "notes",
            &rows,
            &import_options(json!({"mode": "insert"})),
        )
        .await
        .unwrap();
    assert_eq!(out.inserted, 14);
    assert_eq!(out.updated, 0);
    let failed: Vec<_> = out
        .failed
        .iter()
        .map(|f| (f.index, f.code.as_str()))
        .collect();
    assert_eq!(
        failed,
        [
            (3, "INVALID"),
            (7, "INVALID"),
            (11, "CONFLICT"),
            (13, "INVALID"),
            (15, "INVALID"),
            (17, "INVALID")
        ]
    );
    assert_eq!(
        out.failed[0].details.as_ref().unwrap()["constraint"],
        json!("maxLength")
    );
    assert_eq!(
        out.failed[0].details.as_ref().unwrap()["field"],
        json!("title")
    );
    assert_eq!(
        out.failed[3].details.as_ref().unwrap()["constraint"],
        json!("maxBytes")
    );

    // The failed rows left nothing: not a row, not a sequence number, not history, not a change.
    let all = data
        .scan(&c, "notes", &query(json!({"select": ["key", "seq"]})))
        .await
        .unwrap();
    assert_eq!(all.rows.len(), 14);
    let seqs: Vec<i64> = all
        .rows
        .iter()
        .map(|r| r["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(
        seqs,
        (1..=14).collect::<Vec<_>>(),
        "no number was spent on a failed row"
    );
    let history: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rookery.history WHERE plugin = 'scanfix'")
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!(history, 14);
    // One change per written row, numbered in one block, in the order of the rows.
    let mut changes = Vec::new();
    for _ in 0..14 {
        match tokio::time::timeout(std::time::Duration::from_secs(5), feed.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            rookery_store::feed::FeedEvent::Change(c) => changes.push(c),
            other => panic!("{other:?}"),
        }
    }
    assert!(changes.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    assert!(
        changes
            .iter()
            .all(|c| c.op == rookery_store::feed::ChangeOp::Insert && c.collection == "notes")
    );

    // upsert: existing rows are changed, new ones inserted, and the two are counted apart.
    let again = vec![
        json!({"key": "k0", "title": "changed"}),
        json!({"key": "fresh", "title": "new"}),
        json!({"key": "k1", "title": "x".repeat(20)}),
        json!({"title": "no key"}),
    ];
    let out = data
        .import(
            &c,
            "notes",
            &again,
            &import_options(json!({"mode": "upsert", "on": ["key"]})),
        )
        .await
        .unwrap();
    assert_eq!((out.inserted, out.updated), (1, 1));
    assert_eq!(
        out.failed.iter().map(|f| f.index).collect::<Vec<_>>(),
        [2, 3]
    );
    let k0 = data
        .scan(&c, "notes", &query(json!({"where": {"key": "k0"}})))
        .await
        .unwrap();
    assert_eq!(k0.rows[0]["title"], json!("changed"));
    assert_eq!(k0.rows[0]["rev"], json!(2));

    // Options are checked before anything is written.
    for (options, count) in [
        (json!({"mode": "upsert"}), 1),
        (json!({"mode": "upsert", "on": ["title"]}), 1),
        (json!({"mode": "insert", "on": ["key"]}), 1),
        (json!({"mode": "insert"}), 501),
    ] {
        let rows: Vec<Value> = (0..count)
            .map(|n| json!({"key": format!("o{n}"), "title": "t"}))
            .collect();
        let e = data
            .import(&c, "notes", &rows, &import_options(options))
            .await
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Invalid);
    }
    assert_eq!(
        data.count(&c, "notes", &Query::default()).await.unwrap(),
        15
    );
    // 500 rows are one call.
    let rows: Vec<Value> = (0..500)
        .map(|n| json!({"key": format!("big{n}"), "title": "t"}))
        .collect();
    let out = data
        .import(
            &c,
            "notes",
            &rows,
            &import_options(json!({"mode": "insert"})),
        )
        .await
        .unwrap();
    assert_eq!((out.inserted, out.failed.len()), (500, 0));
    db.close().await;
}

#[tokio::test]
async fn import_needs_write_access_and_commits_or_fails_as_one() {
    let Some(w) = World::new().await else { return };
    let rows: Vec<Value> = (0..5).map(|n| json!({"title": format!("i{n}")})).collect();
    let options = import_options(json!({"mode": "insert"}));
    // A caller who cannot write fails the call, not each row.
    let e = w
        .data()
        .import(&w.as_user(w.outsider), "issues", &rows, &options)
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Forbidden);
    let owner = w.as_user(w.owner);
    assert_eq!(
        w.data()
            .count(&owner, "issues", &Query::default())
            .await
            .unwrap(),
        0
    );
    // A fatal error in the middle of the transaction leaves none of the rows: the 250th names a private project the
    // member may not write below, which fails the call (it is not a fault of that row's values), and the 249 rows
    // before it, already written inside the transaction, are rolled back with it.
    let secret = w
        .data()
        .insert(
            &owner,
            "projects",
            json!({"name": "secret", "private": true}),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    let mut many: Vec<Value> = (0..400)
        .map(|n| json!({"title": format!("i{n}")}))
        .collect();
    many[249] = json!({"title": "forbidden", "project": secret["id"]});
    let member = w.as_user(w.member);
    let e = w
        .data()
        .import(&member, "issues", &many, &options)
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Forbidden);
    assert_eq!(
        w.data()
            .count(&owner, "issues", &Query::default())
            .await
            .unwrap(),
        0
    );
    let changes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rookery.changes WHERE collection = 'issues'")
            .fetch_one(w.db.store.pool())
            .await
            .unwrap();
    assert_eq!(changes, 0, "no change was logged");
    // The same rows with an idempotency key are written once.
    let key = rookery_store::idempotency::Key {
        scope: "svc.importer".into(),
        key: "k1".into(),
        request_hash: "h".into(),
    };
    let first = rookery_store::idempotency::scope(
        key.clone(),
        w.data().import(&owner, "issues", &rows, &options),
    )
    .await
    .unwrap();
    let again =
        rookery_store::idempotency::scope(key, w.data().import(&owner, "issues", &rows, &options))
            .await
            .unwrap();
    assert_eq!(first, again);
    assert_eq!(first.inserted, 5);
    assert_eq!(
        w.data()
            .count(&owner, "issues", &Query::default())
            .await
            .unwrap(),
        5
    );
    w.db.close().await;
}

// ── collections for admins ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_collection_for_admins_refuses_every_caller_who_is_not_one() {
    let Some(w) = World::new().await else { return };
    w.db.store
        .collections()
        .apply(
            "gate",
            &defs(json!([
                {"name": "secrets", "scope": "global", "access": {"admin": true},
                 "fields": [{"name": "title", "type": "text", "search": true}], "trash": true},
                {"name": "audit", "kind": "log", "scope": "global", "access": {"admin": true},
                 "fields": [{"name": "what", "type": "text"}]},
                {"name": "open", "scope": "global", "fields": [{"name": "title", "type": "text"}]}
            ])),
        )
        .await
        .unwrap();
    let data = w.db.store.data("gate");
    let (owner, root) = (w.as_user(w.owner), admin());
    let row = data
        .insert(&root, "secrets", json!({"title": "one"}))
        .await
        .unwrap()
        .row
        .unwrap();
    let id: uuid::Uuid = row["id"].as_str().unwrap().parse().unwrap();
    data.insert(&root, "audit", json!({"what": "x"}))
        .await
        .unwrap();

    // The owner of the space, and a service principal that is not an admin, are refused, whatever grants they hold.
    for who in [&owner, &Caller::plugin("gate")] {
        let q = Query::default();
        let all = [
            data.find(who, "secrets", &q).await.err(),
            data.count(who, "secrets", &q).await.err(),
            data.get(who, "secrets", id).await.err(),
            data.search(who, "secrets", "one", None, 10).await.err(),
            data.history(who, "secrets", id, 10).await.err(),
            data.insert(who, "secrets", json!({"title": "x"}))
                .await
                .err(),
            data.update(who, "secrets", id, json!({"title": "x"}), None)
                .await
                .err(),
            data.delete(who, "secrets", id).await.err(),
            data.insert(who, "audit", json!({"what": "y"})).await.err(),
            data.tail(who, "audit", &Default::default()).await.err(),
        ];
        for (i, e) in all.into_iter().enumerate() {
            let e = e.unwrap_or_else(|| panic!("call {i} was not refused"));
            assert_eq!(e.code(), ErrorCode::Forbidden, "call {i}: {e}");
            assert!(e.to_string().contains("for admins"), "call {i}: {e}");
        }
    }
    // Nothing changed, and the admin reads it all; a collection without the flag is as it was.
    assert_eq!(
        data.count(&root, "secrets", &Query::default())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        data.get(&root, "secrets", id).await.unwrap().unwrap()["title"],
        "one"
    );
    data.insert(&owner, "open", json!({"title": "mine"}))
        .await
        .unwrap();
    assert_eq!(
        data.count(&owner, "open", &Query::default()).await.unwrap(),
        1
    );
    w.db.close().await;
}
