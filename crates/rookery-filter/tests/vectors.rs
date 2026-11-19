use std::collections::BTreeMap;

use rookery_filter::{Bind, Filter, Kind, normalize_ref, parse_datetime, to_bind};
use serde_json::{Value, json};

fn vectors() -> Value {
    serde_json::from_str(include_str!("../vectors/filter.json")).unwrap()
}

fn fields(collection: &Value) -> BTreeMap<String, Kind> {
    let mut fields = if collection["kind"] == "log" {
        BTreeMap::from([
            ("id".into(), Kind::Uuid),
            ("seq".into(), Kind::Integer),
            ("createdAt".into(), Kind::Datetime),
            ("createdBy".into(), Kind::Uuid),
        ])
    } else {
        BTreeMap::from([
            ("id".into(), Kind::Uuid),
            ("createdAt".into(), Kind::Datetime),
            ("updatedAt".into(), Kind::Datetime),
            ("createdBy".into(), Kind::Uuid),
            ("updatedBy".into(), Kind::Uuid),
            ("rev".into(), Kind::Integer),
        ])
    };
    match collection["scope"].as_str().unwrap_or("space") {
        "space" => fields.insert("spaceId".into(), Kind::Uuid),
        "user" => fields.insert("userId".into(), Kind::Uuid),
        _ => None,
    };
    if collection["trash"] == true && collection["kind"] != "log" {
        fields.insert("deletedAt".into(), Kind::Datetime);
    }
    for field in collection["fields"].as_array().unwrap() {
        let target = field["to"].as_str().map(String::from);
        let kind = match field["type"].as_str().unwrap() {
            "bytes" => Kind::Bytes,
            "vector" => Kind::Vector,
            "blob" | "text" | "longtext" | "path" => Kind::Text,
            "richtext" => Kind::Richtext,
            "integer" | "sequence" => Kind::Integer,
            "number" => Kind::Number,
            "boolean" => Kind::Boolean,
            "date" => Kind::Date,
            "datetime" => Kind::Datetime,
            "enum" => Kind::Enum(
                field["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_string())
                    .collect(),
            ),
            "json" => Kind::Json,
            "ref" => Kind::Ref(target),
            "refs" => Kind::Refs(target),
            other => panic!("unknown recorded field type {other}"),
        };
        fields.insert(field["name"].as_str().unwrap().to_string(), kind);
    }
    fields
}

#[test]
fn recorded_matches_agree_with_the_store() {
    let recording = vectors();
    let fields = fields(&recording["collection"]);
    let cases = recording["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 35);
    for case in cases {
        let filter = Filter::parse(&fields, &case["where"]).unwrap();
        let mut actual: Vec<_> = recording["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| filter.matches(row))
            .map(|row| row["title"].as_str().unwrap())
            .collect();
        let mut expected: Vec<_> = case["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected, "{}", case["where"]);
    }
}

#[test]
fn recorded_invalid_filters_are_refused() {
    let recording = vectors();
    let fields = fields(&recording["collection"]);
    let invalid = recording["invalid"].as_array().unwrap();
    assert_eq!(invalid.len(), 10);
    for invalid in invalid {
        assert!(Filter::parse(&fields, invalid).is_err(), "{invalid}");
    }
}

#[test]
fn limits_are_enforced_without_a_store() {
    let fields = BTreeMap::from([("title".into(), Kind::Text)]);
    let nested = |depth| (0..depth).fold(json!({}), |inner, _| json!({ "not": inner }));
    assert!(Filter::parse(&fields, &nested(16)).is_ok());
    assert!(Filter::parse(&fields, &nested(17)).is_err());
    assert!(Filter::parse(&fields, &json!({ "or": vec![json!({"title": "x"}); 255] })).is_ok());
    assert!(Filter::parse(&fields, &json!({ "or": vec![json!({"title": "x"}); 256] })).is_err());
    assert!(Filter::parse(&fields, &json!({ "title": { "in": vec!["x"; 1000] } })).is_ok());
    assert!(Filter::parse(&fields, &json!({ "title": { "in": vec!["x"; 1001] } })).is_err());
}

#[test]
fn references_and_comparable_values_keep_their_semantics() {
    assert_eq!(normalize_ref("abc", Some("user")).unwrap(), "user://abc");
    for reference in ["x", "User://x", "user://", "user://a b", "user://a\n"] {
        assert!(normalize_ref(reference, None).is_err(), "{reference}");
    }
    assert!(normalize_ref("env://x", Some("user")).is_err());
    assert_eq!(
        parse_datetime(&json!("2025-12-24T08:30:00+02:00")),
        parse_datetime(&json!("2025-12-24T06:30:00Z"))
    );
    assert_eq!(to_bind(&Kind::Integer, &json!(1.0)).unwrap(), Bind::Int(1));
    assert!(to_bind(&Kind::Integer, &json!(1.5)).is_err());
    assert!(to_bind(&Kind::Richtext, &json!([])).is_err());
    assert_eq!(
        to_bind(
            &Kind::Refs(Some("user".into())),
            &json!(["abc", "user://abc"])
        )
        .unwrap(),
        Bind::TextArr(vec!["user://abc".into()])
    );
    assert!(Filter::all().matches(&json!({})));
}

#[test]
fn binary_filter_vectors_match_both_ports() {
    let recording: Value = serde_json::from_str(include_str!("../vectors/binary.json")).unwrap();
    let fields = fields(&recording["collection"]);
    for case in recording["cases"].as_array().unwrap() {
        let f = Filter::parse(&fields, &case["where"]).unwrap();
        let matches: Vec<_> = recording["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| f.matches(r))
            .map(|r| r["id"].clone())
            .collect();
        assert_eq!(json!(matches), case["matches"]);
    }
    for case in recording["invalid"].as_array().unwrap() {
        assert!(Filter::parse(&fields, case).is_err(), "{case}");
    }
}

#[test]
fn log_collection_filter_vectors_match_both_ports() {
    let recording: Value = serde_json::from_str(include_str!("../vectors/log.json")).unwrap();
    let fields = fields(&recording["collection"]);
    assert!(
        !fields.contains_key("rev")
            && !fields.contains_key("updatedAt")
            && fields.contains_key("seq")
    );
    for case in recording["cases"].as_array().unwrap() {
        let f = Filter::parse(&fields, &case["where"]).unwrap();
        let matches: Vec<_> = recording["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| f.matches(r))
            .map(|r| r["id"].clone())
            .collect();
        assert_eq!(json!(matches), case["matches"], "{}", case["where"]);
    }
    for case in recording["invalid"].as_array().unwrap() {
        assert!(Filter::parse(&fields, case).is_err(), "{case}");
    }
}
