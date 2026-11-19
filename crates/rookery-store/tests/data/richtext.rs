//! Rich text: the plain text and the mentions the indexes get.

use rookery_store::collections::richtext::*;
use serde_json::json;

#[test]
fn text_and_mentions() {
    let doc = json!({
        "type": "doc",
        "content": [
            { "type": "heading", "content": [{ "type": "text", "text": "Plan" }] },
            { "type": "paragraph", "content": [
                { "type": "text", "text": "Ask " },
                { "type": "mention", "attrs": { "uri": "user://a", "label": "@ada" } },
                { "type": "text", "text": " about " },
                { "type": "mention", "attrs": { "uri": "issue://1" } },
                { "type": "mention", "attrs": { "uri": "user://a" } }
            ]},
            { "type": "bullet_list", "content": [
                { "type": "list_item", "content": [
                    { "type": "paragraph", "content": [{ "type": "text", "text": "one" }] }
                ]}
            ]}
        ]
    });
    assert_eq!(plain_text(&doc), "Plan\nAsk @ada about \none");
    assert_eq!(
        mentions(&doc),
        vec!["user://a".to_string(), "issue://1".to_string()]
    );
    assert_eq!(plain_text(&json!({})), "");
    assert!(mentions(&json!("not a doc")).is_empty());
}
