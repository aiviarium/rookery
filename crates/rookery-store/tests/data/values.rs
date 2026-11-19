//! DB-free tests for binary values, atomic operations and lease conditions.
use rookery_store::collections::{
    CollectionDef, VectorIndex,
    operations::{FieldOps, apply},
    values::{decode_bytes, vector_values},
};
use serde_json::{Value, json};

fn definition() -> CollectionDef {
    serde_json::from_value(json!({"name":"assets","fields":[
        {"name":"n","type":"integer","min":0,"max":100},
        {"name":"score","type":"number"},
        {"name":"tags","type":"refs","to":"tag"},
        {"name":"title","type":"text"},
        {"name":"bytes","type":"bytes"},
        {"name":"embedding","type":"vector","dimensions":3,"index":"hnsw"}
    ]}))
    .unwrap()
}
fn ops(v: Value) -> FieldOps {
    serde_json::from_value(v).unwrap()
}

#[test]
fn binary_decoding_and_vector_quantization() {
    assert_eq!(decode_bytes(&json!("AAD/gA==")).unwrap(), [0, 0, 255, 128]);
    assert!(decode_bytes(&json!("not base64")).is_err());
    assert!(decode_bytes(&json!("AB==")).is_err());
    assert!(decode_bytes(&json!("")).unwrap().is_empty());
    assert_eq!(
        vector_values(&json!([0.1, -1, 0])).unwrap(),
        [0.1f32, -1.0, 0.0]
    );
    for invalid in [json!([1e100]), json!(["1"]), json!("[1]")] {
        assert!(vector_values(&invalid).is_err());
    }
}

#[test]
fn definitions_enforce_binary_limits_and_index_types() {
    let mut def = definition();
    def.validate().unwrap();
    for field in [
        json!({"name":"bad","type":"bytes","maxBytes":16777217}),
        json!({"name":"bad","type":"bytes","index":true}),
        json!({"name":"bad","type":"vector","dimensions":2001,"index":"hnsw"}),
        json!({"name":"bad","type":"vector","dimensions":2001}),
        json!({"name":"bad","type":"vector","dimensions":0,"index":"none"}),
        json!({"name":"bad","type":"vector","dimensions":3,"index":true}),
        json!({"name":"bad","type":"vector","dimensions":3,"index":false}),
        json!({"name":"bad","type":"text","index":"hnsw"}),
    ] {
        def.fields.push(serde_json::from_value(field).unwrap());
        assert!(def.validate().is_err());
        def.fields.pop();
    }
    // A vector without `index` is hnsw (at most 2,000 dimensions); `none` is exact (up to 16,000).
    let auto: CollectionDef = serde_json::from_value(json!({"name":"v","fields":[
        {"name":"a","type":"vector","dimensions":2000}
    ]}))
    .unwrap();
    auto.validate().unwrap();
    assert_eq!(auto.fields[0].vector_index(), Some(VectorIndex::Hnsw));
    let exact: CollectionDef = serde_json::from_value(json!({"name":"v","fields":[
        {"name":"a","type":"vector","dimensions":16000,"index":"none"}
    ]}))
    .unwrap();
    exact.validate().unwrap();
    assert_eq!(exact.fields[0].vector_index(), Some(VectorIndex::None));
}

#[test]
fn field_operations_are_typed_and_preserve_integer_precision() {
    let def = definition();
    let result = apply(
        &def,
        &json!({"n":9007199254740993i64,"score":1.25,"tags":["tag://a"]}),
        &Default::default(),
        &ops(json!({"n":{"inc":2},"score":{"inc":0.5},"tags":{"add":["a","b","b"]}})),
    )
    .unwrap();
    assert_eq!(result["n"], 9007199254740995i64);
    assert_eq!(result["score"], 1.75);
    assert_eq!(result["tags"], json!(["tag://a", "tag://b"]));
    let removed = apply(
        &def,
        &Value::Object(result),
        &Default::default(),
        &ops(json!({"tags":{"remove":["a","missing"]}})),
    )
    .unwrap();
    assert_eq!(removed["tags"], json!(["tag://b"]));
}

#[test]
fn operations_reject_ambiguity_null_numeric_overflow_and_wrong_types() {
    let def = definition();
    for (old, patch, operations) in [
        (json!({"n":1}), json!({"n":2}), json!({"n":{"inc":1}})),
        (json!({"n":null}), json!({}), json!({"n":{"inc":1}})),
        (json!({"n":i64::MAX}), json!({}), json!({"n":{"inc":1}})),
        (json!({"n":1}), json!({}), json!({"n":{"inc":0.5}})),
        (
            json!({"score":1e308}),
            json!({}),
            json!({"score":{"inc":1e308}}),
        ),
        (json!({"title":"a"}), json!({}), json!({"title":{"inc":1}})),
        (json!({}), json!({}), json!({"nope":{"add":[]}})),
    ] {
        assert!(apply(&def, &old, patch.as_object().unwrap(), &ops(operations)).is_err());
    }
    assert!(serde_json::from_value::<FieldOps>(json!({"tags":{"append":["a"]}})).is_err());
    assert!(serde_json::from_value::<FieldOps>(json!({"n":{"inc":1,"add":[]}})).is_err());
    assert_eq!(
        apply(
            &def,
            &json!({}),
            &Default::default(),
            &ops(json!({"tags":{"add":["a"]}}))
        )
        .unwrap()["tags"],
        json!(["tag://a"])
    );
}
