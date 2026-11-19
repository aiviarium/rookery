//! `schedules.*` and webhooks: who makes schedules, their firing and pausing, and deliveries to
//! `/hooks/<plugin>/<name>` verified, enqueued once and refused when they are not right.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Caller, Methods, RpcError, test_kernel};
use rookery_proto::schedules::{SecretSource, WebhookContribution, WebhookScheme, WebhookVerify};
use serde_json::{Value, json};
use tower::ServiceExt;

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

async fn jobs_of(k: &TestKernel, kind: &str) -> Vec<rookery_store::jobs::Job> {
    k.db.store
        .jobs()
        .list(&rookery_store::jobs::Filter {
            kind: Some(kind),
            limit: 100,
            ..Default::default()
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn schedules_fire_their_jobs_once_at_a_time_and_belong_to_who_made_them() {
    let k: TestKernel = test_kernel!(methods());
    let notes = Caller::Plugin("notes".into());
    let ops = k.person("Ops", false).await;
    let admin = k.person("Maria", true).await;
    let s = k
        .call(
            &notes,
            "schedules.create",
            json!({ "name": "sync", "job": "sync", "everySecs": 60, "params": { "full": true } }),
        )
        .await
        .unwrap();
    assert_eq!(s["job"], "notes/sync");
    assert_eq!(s["plugin"], "notes");
    // A person makes none; an admin makes one only of a kind a handler runs, or a plugin's.
    assert_eq!(
        code(
            k.call(
                &ops,
                "schedules.create",
                json!({ "name": "x", "job": "apply", "everySecs": 60 })
            )
            .await
        ),
        "FORBIDDEN"
    );
    assert_eq!(
        code(
            k.call(
                &admin,
                "schedules.create",
                json!({ "name": "x", "job": "nothing.runs.this", "everySecs": 60 })
            )
            .await
        ),
        "INVALID"
    );
    assert_eq!(
        code(
            k.call(
                &notes,
                "schedules.create",
                json!({ "name": "y", "job": "sync", "cron": "0 * * * *", "everySecs": 5 })
            )
            .await
        ),
        "INVALID"
    );
    assert_eq!(
        code(k.call(&notes, "schedules.create", json!({ "name": "y", "job": "sync", "cron": "0 3 * * *", "timezone": "Mars/Olympus" })).await),
        "INVALID"
    );
    // More often than once a minute is refused.
    for timing in [
        json!({ "everySecs": 59 }),
        json!({ "cron": "*/30 * * * * *" }),
    ] {
        let mut p = json!({ "name": "fast", "job": "sync" });
        p.as_object_mut()
            .unwrap()
            .extend(timing.as_object().unwrap().clone());
        assert_eq!(code(k.call(&notes, "schedules.create", p).await), "INVALID");
    }
    // Due now, it fires within the tick; due again while its job is open, it starts no second one.
    let due_now = || {
        sqlx::query("UPDATE rookery.schedules SET next_run_at = now() WHERE name = 'sync'")
            .execute(k.db.store.pool())
    };
    due_now().await.unwrap();
    let mut fired = Vec::new();
    for _ in 0..50 {
        fired = jobs_of(&k, "notes/sync").await;
        if !fired.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(fired.len(), 1, "the schedule fired");
    assert_eq!(fired[0].params, json!({ "full": true }));
    assert_eq!(fired[0].enqueued_by, "plugin:notes");
    due_now().await.unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        jobs_of(&k, "notes/sync").await.len(),
        1,
        "one job at a time"
    );
    // Paused, then run by hand with more params; another plugin sees none of it.
    let id = s["id"].clone();
    let paused = k
        .call(
            &notes,
            "schedules.setEnabled",
            json!({ "id": id, "enabled": false }),
        )
        .await
        .unwrap();
    assert_eq!(paused["enabled"], false);
    let other = Caller::Plugin("other".into());
    assert_eq!(
        code(k.call(&other, "schedules.fire", json!({ "id": id })).await),
        "NOT_FOUND"
    );
    assert!(
        k.call(&other, "schedules.list", json!({})).await.unwrap()["schedules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    k.call(
        &admin,
        "schedules.fire",
        json!({ "id": id, "params": { "since": "yesterday" } }),
    )
    .await
    .unwrap();
    let all = jobs_of(&k, "notes/sync").await;
    assert_eq!(all.len(), 2);
    assert!(
        all.iter()
            .any(|j| j.params == json!({ "full": true, "since": "yesterday" }))
    );
    k.call(&notes, "schedules.remove", json!({ "id": id }))
        .await
        .unwrap();
    assert!(
        k.call(&admin, "schedules.list", json!({ "all": true }))
            .await
            .unwrap()["schedules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    k.close().await;
}

#[tokio::test]
async fn declared_schedules_follow_the_manifest() {
    let k: TestKernel = test_kernel!(methods());
    let declare = |cron: &str| {
        serde_json::from_value::<Vec<rookery_proto::schedules::ScheduleContribution>>(json!([
            { "id": "notes.nightly", "job": "compact", "cron": cron, "timezone": "Europe/Berlin" }
        ]))
        .unwrap()
    };
    rookery_data::schedules::sync_declared(&k.kernel, "notes", &declare("0 3 * * *"))
        .await
        .unwrap();
    let notes = Caller::Plugin("notes".into());
    let listed = k.call(&notes, "schedules.list", json!({})).await.unwrap();
    let s = &listed["schedules"][0];
    assert_eq!(
        (s["name"].clone(), s["job"].clone(), s["declared"].clone()),
        (json!("nightly"), json!("notes/compact"), json!(true))
    );
    assert_eq!(
        code(
            k.call(&notes, "schedules.remove", json!({ "id": s["id"] }))
                .await
        ),
        "INVALID"
    );
    rookery_data::schedules::sync_declared(&k.kernel, "notes", &declare("0 4 * * *"))
        .await
        .unwrap();
    let again = k.call(&notes, "schedules.list", json!({})).await.unwrap();
    assert_eq!(again["schedules"][0]["cron"], "0 4 * * *");
    assert_eq!(
        again["schedules"][0]["id"], s["id"],
        "the same schedule, changed"
    );
    rookery_data::schedules::sync_declared(&k.kernel, "notes", &[])
        .await
        .unwrap();
    assert!(
        k.call(&notes, "schedules.list", json!({})).await.unwrap()["schedules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    k.close().await;
}

fn github(secret: &str) -> WebhookContribution {
    WebhookContribution {
        id: "code.push".into(),
        job: "delivery".into(),
        title: None,
        verify: WebhookVerify {
            scheme: WebhookScheme::Github,
            secret: Some(SecretSource::Secret {
                secret: secret.into(),
            }),
            algorithm: None,
            header: None,
            prefix: None,
            encoding: None,
            timestamp_header: None,
            delivery_header: None,
            tolerance_secs: None,
        },
        max_bytes: Some(1024),
    }
}

async fn post(
    k: &TestKernel,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> (StatusCode, Value) {
    let mut req = Request::post(path).header("content-type", "application/json");
    for (h, v) in headers {
        req = req.header(*h, v);
    }
    let response = rookery_data::webhooks::routes()
        .with_state(k.kernel.clone())
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn signature(secret: &str, body: &str) -> String {
    let mac = rookery_data::webhooks::sign("sha256", secret.as_bytes(), &[body.as_bytes()]);
    format!(
        "sha256={}",
        mac.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

#[tokio::test]
async fn webhook_deliveries_are_verified_enqueued_once_and_refused_when_wrong() {
    let k: TestKernel = test_kernel!(methods());
    rookery_data::webhooks::set_webhooks(&k.kernel, "code", vec![github("hook-secret")]).unwrap();
    let admin = k.person("Maria", true).await;
    let body = r#"{"ref":"refs/heads/main"}"#;
    let signed = |id: &str, body: &str| {
        vec![
            ("x-hub-signature-256", signature("s3cret", body)),
            ("x-github-delivery", id.to_owned()),
        ]
    };
    // Until its secret is set the webhook takes nothing.
    let (status, _) = post(&k, "/hooks/code/push", &signed("d1", body), body).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let listed = k.call(&admin, "webhooks.list", json!({})).await.unwrap();
    assert_eq!(listed["webhooks"][0]["ready"], false);
    k.call(
        &admin,
        "webhooks.setSecret",
        json!({ "id": "code.push", "secret": "s3cret" }),
    )
    .await
    .unwrap();
    let (status, accepted) = post(&k, "/hooks/code/push", &signed("d1", body), body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let jobs = jobs_of(&k, "code/delivery").await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].params["body"]["ref"], "refs/heads/main");
    assert_eq!(jobs[0].params["deliveryId"], "d1");
    assert!(
        jobs[0].params["headers"]
            .get("x-hub-signature-256")
            .is_some()
    );
    // The same delivery again enqueues nothing, under another delivery id too, which is not signed.
    for id in ["d1", "d9"] {
        let (status, again) = post(&k, "/hooks/code/push", &signed(id, body), body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again["duplicate"], true);
    }
    assert_eq!(jobs_of(&k, "code/delivery").await.len(), 1);
    // A wrong signature, a body too large and a webhook nobody declares are refused.
    let mut wrong = signed("d2", body);
    wrong[0].1 = signature("guess", body);
    for _ in 0..3 {
        assert_eq!(
            post(&k, "/hooks/code/push", &wrong, body).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    let large = format!(r#"{{"pad":"{}"}}"#, "x".repeat(2000));
    assert_eq!(
        post(&k, "/hooks/code/push", &signed("d3", &large), &large)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        post(&k, "/hooks/code/nothing", &[], "{}").await.0,
        StatusCode::NOT_FOUND
    );
    let listed = k.call(&admin, "webhooks.list", json!({})).await.unwrap();
    let outcomes: Vec<&str> = listed["webhooks"][0]["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|o| **o == "accepted").count(), 1);
    assert_eq!(outcomes.iter().filter(|o| **o == "duplicate").count(), 1);
    // Refusals are counted per minute and status: one entry each, not one a request.
    assert_eq!(outcomes.iter().filter(|o| **o == "refused").count(), 3);
    let unauthorized = listed["webhooks"][0]["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["status"] == 401)
        .unwrap();
    assert!(unauthorized["count"].as_i64().unwrap() >= 1);
    assert_eq!(
        code(
            k.call(&k.person("Ops", false).await, "webhooks.list", json!({}))
                .await
        ),
        "FORBIDDEN"
    );
    // A declaration that cannot verify is refused.
    let mut open = github("x");
    open.verify.secret = None;
    assert!(rookery_data::webhooks::set_webhooks(&k.kernel, "code", vec![open]).is_err());
    k.close().await;
}

#[tokio::test]
async fn an_unsigned_webhook_is_limited_and_a_large_declared_body_is_read() {
    let k: TestKernel = test_kernel!(methods());
    let mut open = github("x");
    open.id = "code.ping".into();
    open.verify.scheme = WebhookScheme::None;
    open.verify.secret = None;
    open.max_bytes = Some(4 << 20);
    rookery_data::webhooks::set_webhooks(&k.kernel, "code", vec![open]).unwrap();
    // A body over axum's own default limit, within the webhook's.
    let big = format!(r#"{{"pad":"{}"}}"#, "x".repeat(3 << 20));
    assert_eq!(
        post(&k, "/hooks/code/ping", &[], &big).await.0,
        StatusCode::ACCEPTED
    );
    let mut statuses = Vec::new();
    for i in 0..rookery_data::webhooks::NONE_PER_MINUTE + 1 {
        statuses.push(
            post(&k, "/hooks/code/ping", &[], &format!("{{\"n\":{i}}}"))
                .await
                .0,
        );
    }
    // The minute may turn during the loop; past its limit it answers 429 and enqueues nothing.
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
    let queued = jobs_of(&k, "code/delivery").await.len() as u32;
    assert!(
        queued <= rookery_data::webhooks::NONE_PER_MINUTE + 1,
        "{queued}"
    );
    k.close().await;
}

#[tokio::test]
async fn a_schedule_of_a_demoted_admin_is_paused_and_they_are_told() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let root = k.person("Root", true).await;
    let s = k
        .call(
            &admin,
            "schedules.create",
            json!({ "name": "nightly", "plugin": "notes", "job": "sync", "cron": "0 3 * * *" }),
        )
        .await
        .unwrap();
    let uid: uuid::Uuid = admin.user().unwrap().id.parse().unwrap();
    k.db.store.users().set_role(uid, false).await.unwrap();
    sqlx::query("UPDATE rookery.schedules SET next_run_at = now() WHERE name = 'nightly'")
        .execute(k.db.store.pool())
        .await
        .unwrap();
    let mut paused = Value::Null;
    for _ in 0..50 {
        let all = k
            .call(&root, "schedules.list", json!({ "all": true }))
            .await
            .unwrap();
        paused = all["schedules"][0].clone();
        if paused["enabled"] == false {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(paused["id"], s["id"]);
    assert_eq!(paused["enabled"], false);
    assert!(paused["pausedReason"].is_string());
    assert!(
        jobs_of(&k, "notes/sync").await.is_empty(),
        "nothing ran as them"
    );
    let inbox =
        k.db.store
            .notify()
            .list(uid, false, None, 10)
            .await
            .unwrap();
    assert_eq!(inbox[0].kind, "rookery/schedulePaused");
    k.close().await;
}

#[tokio::test]
async fn a_plugin_has_at_most_a_hundred_schedules() {
    let k: TestKernel = test_kernel!(methods());
    let notes = Caller::Plugin("notes".into());
    let max = rookery_store::schedules::MAX_PER_PLUGIN;
    for i in 0..max {
        k.call(
            &notes,
            "schedules.create",
            json!({ "name": format!("s{i}"), "job": "sync", "everySecs": 3600 }),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        code(
            k.call(
                &notes,
                "schedules.create",
                json!({ "name": "one more", "job": "sync", "everySecs": 3600 })
            )
            .await
        ),
        "INVALID"
    );
    // Replacing one is not one more.
    k.call(
        &notes,
        "schedules.create",
        json!({ "name": "s0", "job": "sync", "everySecs": 7200 }),
    )
    .await
    .unwrap();
    k.close().await;
}
