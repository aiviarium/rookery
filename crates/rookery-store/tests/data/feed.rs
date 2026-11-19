//! The change log and the in-process feed.

use super::common;

use std::time::Duration;

use common::*;
use rookery_store::feed::{CatchUp, ChangeOp, FeedEvent, Subscription};
use rookery_store::{Caller, Store};
use serde_json::json;

async fn next_change(sub: &mut Subscription) -> std::sync::Arc<rookery_store::feed::Change> {
    match tokio::time::timeout(Duration::from_secs(10), sub.next())
        .await
        .expect("feed timed out")
        .unwrap()
        .unwrap()
    {
        FeedEvent::Change(c) => c,
        FeedEvent::Reset { head } => panic!("unexpected reset at {head}"),
    }
}

#[tokio::test]
async fn concurrent_writers_keep_seq_in_commit_order() {
    let Some(w) = World::new().await else { return };
    let c = w.as_user(w.owner);
    let writers = 8;
    let per = 25;
    let mut sub = w.db.store.feed().subscribe().await.unwrap();
    let start = sub.last_seq();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // A reader that checks, in every snapshot, that the committed seqs have no holes: a hole
    // would be a lower seq committing after a higher one.
    let reader = {
        let pool = w.db.store.pool().clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut checks = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let (n, max): (i64, Option<i64>) =
                    sqlx::query_as("SELECT count(*), max(seq) FROM rookery.changes")
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                assert_eq!(n, max.unwrap_or(0), "a hole in the committed seqs");
                checks += 1;
            }
            checks
        })
    };
    let mut tasks = Vec::new();
    for t in 0..writers {
        let store = w.db.store.clone();
        let c = c.clone();
        tasks.push(tokio::spawn(async move {
            let mut seqs = Vec::new();
            for i in 0..per {
                let out = if i % 3 == 2 {
                    let ops: Vec<rookery_store::collections::BatchOp> =
                        serde_json::from_value(json!([
                            { "op": "insert", "collection": "comments", "values": {} },
                            { "op": "insert", "collection": "comments", "values": {} }
                        ]))
                        .unwrap();
                    store.data("issues").batch(&c, &ops).await.unwrap().changes
                } else {
                    let out = store
                        .data("issues")
                        .insert(&c, "issues", json!({ "title": format!("{t}-{i}") }))
                        .await
                        .unwrap();
                    vec![out.change.unwrap()]
                };
                seqs.extend(out.iter().map(|c| c.seq));
            }
            seqs
        }));
    }
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(reader.await.unwrap() > 0);
    all.sort();
    let total = all.len() as i64;
    assert_eq!(all, (start + 1..=start + total).collect::<Vec<_>>());
    // The subscriber got every change, in seq order, with rows.
    for expected in start + 1..=start + total {
        let ch = next_change(&mut sub).await;
        assert_eq!(ch.seq, expected);
        assert!(ch.row.is_some());
        assert_eq!(ch.space_id, Some(w.space));
    }
}

#[tokio::test]
async fn a_lagging_subscriber_catches_up_from_the_log() {
    let Some(w) = World::new().await else { return };
    let mut opts = w.db.options();
    opts.feed_capacity = 16;
    let store = Store::connect(opts).await.unwrap();
    store.reload().await.unwrap();
    let c = w.as_user(w.owner);
    let mut sub = store.feed().subscribe().await.unwrap();
    let start = sub.last_seq();
    let mut ids = Vec::new();
    for i in 0..100 {
        let out = store
            .data("issues")
            .insert(&c, "issues", json!({ "title": format!("lag {i}") }))
            .await
            .unwrap();
        ids.push(out.row.unwrap()["id"].clone());
    }
    // 100 changes went past a channel of 16: the subscriber still sees all of them in order.
    for (i, expected) in (start + 1..=start + 100).enumerate() {
        let ch = next_change(&mut sub).await;
        assert_eq!(ch.seq, expected);
        assert_eq!(ch.row.as_ref().unwrap()["id"], ids[i]);
    }
}

#[tokio::test]
async fn changes_from_another_process_are_caught_up_in_order() {
    let Some(w) = World::new().await else { return };
    let other = w.db.second_store().await.unwrap();
    let c = w.as_user(w.owner);
    let mut sub = w.db.store.feed().subscribe().await.unwrap();
    let start = sub.last_seq();
    for i in 0..3 {
        other
            .data("issues")
            .insert(&c, "issues", json!({ "title": format!("elsewhere {i}") }))
            .await
            .unwrap();
    }
    // The next local change reveals the gap; the subscriber reads the three from the log.
    w.data()
        .insert(&c, "issues", json!({ "title": "here" }))
        .await
        .unwrap();
    let mut titles = Vec::new();
    for expected in start + 1..=start + 4 {
        let ch = next_change(&mut sub).await;
        assert_eq!(ch.seq, expected);
        titles.push(
            ch.row.as_ref().unwrap()["title"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(
        titles,
        ["elsewhere 0", "elsewhere 1", "elsewhere 2", "here"]
    );
}

#[tokio::test]
async fn catch_up_trim_and_reset() {
    let Some(w) = World::new().await else { return };
    let c = w.as_user(w.owner);
    let head0 = w.db.store.feed().logged_head().await.unwrap();
    let mut ids = Vec::new();
    for i in 0..10 {
        ids.push(
            w.data()
                .insert(&c, "issues", json!({ "title": format!("c{i}") }))
                .await
                .unwrap()
                .row
                .unwrap()["id"]
                .clone(),
        );
    }
    let id0: uuid::Uuid = ids[0].as_str().unwrap().parse().unwrap();
    w.data()
        .update(&c, "issues", id0, json!({ "title": "c0 again" }), None)
        .await
        .unwrap();
    let id1: uuid::Uuid = ids[1].as_str().unwrap().parse().unwrap();
    w.data().delete(&c, "issues", id1).await.unwrap();
    let cm = w
        .data()
        .insert(&c, "comments", json!({}))
        .await
        .unwrap()
        .row
        .unwrap();
    w.data()
        .delete(&c, "comments", cm["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap();
    let head = w.db.store.feed().logged_head().await.unwrap();
    assert_eq!(head, head0 + 14);

    let CatchUp::Changes(all) = w.db.store.feed().changes_after(head0, 100).await.unwrap() else {
        panic!("reset")
    };
    assert_eq!(all.len(), 14);
    // Caught-up changes carry the current row: the first insert shows the later title.
    assert_eq!(all[0].row.as_ref().unwrap()["title"], json!("c0 again"));
    assert_eq!(all[10].op, ChangeOp::Update);
    assert_eq!(all[11].op, ChangeOp::Delete);
    assert!(
        all[11].row.as_ref().unwrap()["deletedAt"].is_string(),
        "soft delete keeps the row"
    );
    assert!(all[13].row.is_none(), "hard delete has no row");
    let CatchUp::Changes(page) = w.db.store.feed().changes_after(head0 + 5, 3).await.unwrap()
    else {
        panic!("reset")
    };
    assert_eq!(
        page.iter().map(|c| c.seq).collect::<Vec<_>>(),
        [head0 + 6, head0 + 7, head0 + 8]
    );

    // Trim: cursors before the trimmed point reset, later ones continue.
    assert_eq!(
        w.db.store.feed().trim_through(head0 + 5).await.unwrap(),
        (head0 + 5) as u64
    );
    assert!(
        matches!(w.db.store.feed().changes_after(head0 + 2, 100).await.unwrap(), CatchUp::Reset { head: h } if h == head)
    );
    let CatchUp::Changes(rest) =
        w.db.store
            .feed()
            .changes_after(head0 + 5, 100)
            .await
            .unwrap()
    else {
        panic!("reset")
    };
    assert_eq!(rest.len(), 9);
    let mut sub = w.db.store.feed().subscribe_from(head0 + 1);
    match sub.next().await.unwrap().unwrap() {
        FeedEvent::Reset { head: h } => assert_eq!(h, head),
        other => panic!("expected a reset, got {other:?}"),
    }
    // After the reset the subscription continues live.
    w.data()
        .insert(&c, "issues", json!({ "title": "after reset" }))
        .await
        .unwrap();
    assert_eq!(next_change(&mut sub).await.seq, head + 1);
    let mut from = w.db.store.feed().subscribe_from(head0 + 12);
    assert_eq!(next_change(&mut from).await.seq, head0 + 13);
    assert_eq!(next_change(&mut from).await.seq, head0 + 14);
    assert_eq!(next_change(&mut from).await.seq, head + 1);
    // A cursor ahead of the log (say, from before a restore) resets too.
    assert!(matches!(
        w.db.store
            .feed()
            .changes_after(head + 100, 10)
            .await
            .unwrap(),
        CatchUp::Reset { .. }
    ));
    let mut ahead = w.db.store.feed().subscribe_from(head + 100);
    assert!(matches!(
        ahead.next().await.unwrap().unwrap(),
        FeedEvent::Reset { .. }
    ));
    // Trimming by age with nothing old does nothing.
    assert_eq!(
        w.db.store
            .feed()
            .trim(Duration::from_secs(3600))
            .await
            .unwrap(),
        0
    );
    let _ = Caller::system();
}
