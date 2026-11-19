//! DB-free unit tests for collection field constraints, enum transitions, and definition
//! validation. Runs without Postgres (`cargo test -p crow-store --test constraints`).

use rookery_proto::ErrorCode;
use rookery_store::Error;
use rookery_store::collections::constraints::{
    check_field_value, check_insert, check_transition, check_update,
};
use rookery_store::collections::schema::CollectionDef;
use serde_json::{Map, Value, json};

fn def(json_def: Value) -> CollectionDef {
    serde_json::from_value(json_def).expect("valid CollectionDef JSON")
}

fn as_map(v: Value) -> Map<String, Value> {
    v.as_object().cloned().expect("object")
}

#[test]
fn required_constraint_checks_insert_and_update() {
    let c = def(json!({
        "name": "tasks",
        "fields": [
            { "name": "title", "type": "text", "required": true },
            { "name": "note", "type": "text" },
            { "name": "kind", "type": "text", "required": true, "default": "bug" }
        ]
    }));
    c.validate().unwrap();

    // Missing required field fails with constraint = "required".
    let err = check_insert(&c, &as_map(json!({ "note": "hi" }))).unwrap_err();
    assert_eq!(err.collection, "tasks");
    assert_eq!(err.field, "title");
    assert_eq!(err.constraint, "required");
    let store_err = Error::from(err);
    assert_eq!(store_err.code(), ErrorCode::Invalid);
    assert_eq!(
        store_err.details(),
        Some(json!({ "collection": "tasks", "field": "title", "constraint": "required" }))
    );

    // Explicit null on required field fails.
    let err = check_insert(&c, &as_map(json!({ "title": null }))).unwrap_err();
    assert_eq!(err.constraint, "required");
    assert_eq!(err.field, "title");

    // Default satisfies required when omitted, and explicit value satisfies required.
    check_insert(&c, &as_map(json!({ "title": "Fix bug" }))).unwrap();

    // Updating a required field to null fails.
    let old = as_map(json!({ "title": "Fix bug", "kind": "bug" }));
    let err = check_update(&c, &old, &as_map(json!({ "title": null }))).unwrap_err();
    assert_eq!(err.constraint, "required");
    assert_eq!(err.field, "title");
}

#[test]
fn min_and_max_constraints_on_integer_and_number() {
    let c = def(json!({
        "name": "deals",
        "fields": [
            { "name": "amount", "type": "number", "min": 0.0, "max": 100.5, "format": "currency" },
            { "name": "score", "type": "integer", "min": 1, "max": 5, "format": "progress" }
        ]
    }));
    c.validate().unwrap();

    // Boundaries pass.
    check_insert(&c, &as_map(json!({ "amount": 0.0, "score": 1 }))).unwrap();
    check_insert(&c, &as_map(json!({ "amount": 100.5, "score": 5 }))).unwrap();
    check_insert(&c, &as_map(json!({ "amount": null, "score": null }))).unwrap();

    // Below min fails.
    let err = check_insert(&c, &as_map(json!({ "amount": -0.01 }))).unwrap_err();
    assert_eq!(err.collection, "deals");
    assert_eq!(err.field, "amount");
    assert_eq!(err.constraint, "min");

    let err = check_insert(&c, &as_map(json!({ "score": 0 }))).unwrap_err();
    assert_eq!(err.field, "score");
    assert_eq!(err.constraint, "min");

    // Above max fails.
    let err = check_insert(&c, &as_map(json!({ "amount": 100.6 }))).unwrap_err();
    assert_eq!(err.field, "amount");
    assert_eq!(err.constraint, "max");

    let err = check_insert(&c, &as_map(json!({ "score": 6 }))).unwrap_err();
    assert_eq!(err.field, "score");
    assert_eq!(err.constraint, "max");
}

#[test]
fn length_and_pattern_constraints_on_text_longtext_and_path() {
    let c = def(json!({
        "name": "accounts",
        "fields": [
            { "name": "code", "type": "text", "minLength": 2, "maxLength": 4, "pattern": "^[A-Z0-9]+$" },
            { "name": "bio", "type": "longtext", "minLength": 3, "maxLength": 10 },
            { "name": "slug", "type": "path", "maxLength": 20, "pattern": "^[a-z0-9/_-]+$" }
        ]
    }));
    c.validate().unwrap();

    // Valid values pass (including multi-byte Unicode scalar counts).
    check_insert(
        &c,
        &as_map(json!({ "code": "AB1", "bio": "🦀🦀🦀", "slug": "team/core" })),
    )
    .unwrap();

    // minLength fails (emoji has 2 chars, 8 UTF-8 bytes, still < 3 chars).
    let err = check_insert(&c, &as_map(json!({ "bio": "🦀🦀" }))).unwrap_err();
    assert_eq!(err.field, "bio");
    assert_eq!(err.constraint, "minLength");

    // maxLength fails.
    let err = check_insert(&c, &as_map(json!({ "code": "ABCDE" }))).unwrap_err();
    assert_eq!(err.field, "code");
    assert_eq!(err.constraint, "maxLength");

    // pattern fails.
    let err = check_insert(&c, &as_map(json!({ "code": "ab1" }))).unwrap_err();
    assert_eq!(err.field, "code");
    assert_eq!(err.constraint, "pattern");

    let err = check_insert(&c, &as_map(json!({ "slug": "Bad Space" }))).unwrap_err();
    assert_eq!(err.field, "slug");
    assert_eq!(err.constraint, "pattern");
}

#[test]
fn enum_membership_and_transitions_rules() {
    let c = def(json!({
        "name": "deals",
        "fields": [
            {
                "name": "state",
                "type": "enum",
                "values": [
                    { "value": "open", "title": "Open", "tone": "neutral" },
                    { "value": "review", "title": "In review", "tone": "accent" },
                    { "value": "won", "title": "Won", "tone": "positive" },
                    { "value": "lost", "title": "Lost", "tone": "negative" }
                ],
                "transitions": {
                    "open": ["review", "lost"],
                    "review": ["open", "won", "lost"]
                }
            },
            {
                "name": "priority",
                "type": "enum",
                "values": ["low", "high"]
            }
        ]
    }));
    c.validate().unwrap();

    // Any declared enum value works on insert.
    check_insert(&c, &as_map(json!({ "state": "open", "priority": "low" }))).unwrap();
    check_insert(&c, &as_map(json!({ "state": "won" }))).unwrap();

    // Undeclared enum value fails with constraint = "enum".
    let err = check_insert(&c, &as_map(json!({ "state": "archived" }))).unwrap_err();
    assert_eq!(err.collection, "deals");
    assert_eq!(err.field, "state");
    assert_eq!(err.constraint, "enum");

    let state_field = c.field("state").unwrap();
    check_field_value("deals", state_field, &json!("review")).unwrap();
    let non_str = check_field_value("deals", state_field, &json!(123)).unwrap_err();
    assert_eq!(non_str.constraint, "enum");

    // Allowed transitions succeed.
    check_transition("deals", state_field, &json!("open"), &json!("review")).unwrap();
    check_transition("deals", state_field, &json!("review"), &json!("won")).unwrap();

    // No-op transition succeeds even from terminal state.
    check_transition("deals", state_field, &json!("won"), &json!("won")).unwrap();

    // Filling null -> declared value succeeds.
    check_transition("deals", state_field, &json!(null), &json!("open")).unwrap();

    // Disallowed transition fails.
    let err = check_transition("deals", state_field, &json!("open"), &json!("won")).unwrap_err();
    assert_eq!(err.collection, "deals");
    assert_eq!(err.field, "state");
    assert_eq!(err.constraint, "transitions");

    // Terminal state (not in transitions map) refuses changing to another value.
    let err = check_transition("deals", state_field, &json!("won"), &json!("open")).unwrap_err();
    assert_eq!(err.constraint, "transitions");

    // Clearing a non-null enum field when transitions are declared fails.
    let err = check_transition("deals", state_field, &json!("open"), &json!(null)).unwrap_err();
    assert_eq!(err.constraint, "transitions");

    // Field without transitions allows any valid change.
    check_update(
        &c,
        &as_map(json!({ "priority": "low" })),
        &as_map(json!({ "priority": "high" })),
    )
    .unwrap();
}

#[test]
fn collection_def_validation_rejects_malformed_constraints_and_metadata() {
    let valid = def(json!({
        "name": "deals",
        "fields": [
            { "name": "title", "type": "text", "title": "Title", "minLength": 1, "maxLength": 120 },
            { "name": "website", "type": "text", "format": "url" },
            { "name": "email", "type": "text", "format": "email" },
            { "name": "budget", "type": "number", "format": "currency", "min": 0 },
            { "name": "margin", "type": "number", "format": "percent" },
            { "name": "cycle", "type": "integer", "format": "duration" },
            { "name": "completion", "type": "integer", "format": "progress" },
            {
                "name": "state",
                "type": "enum",
                "values": [
                    { "value": "open", "title": "Open", "tone": "neutral" },
                    { "value": "lost", "title": "Lost", "tone": "negative" }
                ],
                "transitions": { "open": ["lost"] }
            },
            {
                "name": "lostReason",
                "type": "text",
                "visibleWhen": { "state": "lost" },
                "editableWhen": { "and": [{ "state": "lost" }, { "budget": { "gte": 0 } }] }
            }
        ]
    }));
    valid.validate().unwrap();

    for bad in [
        // min > max
        json!({ "name": "c", "fields": [{ "name": "n", "type": "integer", "min": 10, "max": 5 }] }),
        // min on text
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "min": 1 }] }),
        // minLength > maxLength
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "minLength": 5, "maxLength": 2 }] }),
        // maxLength on integer
        json!({ "name": "c", "fields": [{ "name": "n", "type": "integer", "maxLength": 5 }] }),
        // bad regex
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "pattern": "[unclosed" }] }),
        // pattern on boolean
        json!({ "name": "c", "fields": [{ "name": "b", "type": "boolean", "pattern": "^true$" }] }),
        // duplicate enum value across string and detail
        json!({ "name": "c", "fields": [{ "name": "e", "type": "enum", "values": ["open", { "value": "open", "title": "Open" }] }] }),
        // transition on non-enum
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "transitions": { "a": ["b"] } }] }),
        // transition unknown source
        json!({ "name": "c", "fields": [{ "name": "e", "type": "enum", "values": ["a", "b"], "transitions": { "missing": ["b"] } }] }),
        // transition unknown target
        json!({ "name": "c", "fields": [{ "name": "e", "type": "enum", "values": ["a", "b"], "transitions": { "a": ["missing"] } }] }),
        // incompatible format
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "format": "currency" }] }),
        json!({ "name": "c", "fields": [{ "name": "n", "type": "integer", "format": "email" }] }),
        // invalid visibleWhen / editableWhen
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "visibleWhen": "bad" }] }),
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "visibleWhen": { "nope": 1 } }] }),
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "editableWhen": { "t": { "nope": 1 } } }] }),
        // default violating constraint
        json!({ "name": "c", "fields": [{ "name": "n", "type": "integer", "min": 10, "default": 3 }] }),
        json!({ "name": "c", "fields": [{ "name": "t", "type": "text", "pattern": "^[a-z]+$", "default": "BAD" }] }),
    ] {
        let e = def(bad.clone()).validate().unwrap_err();
        assert_eq!(e.code(), ErrorCode::Invalid, "{bad}");
    }
}
