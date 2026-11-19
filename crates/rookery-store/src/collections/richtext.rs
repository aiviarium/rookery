//! Just enough of ProseMirror's JSON format: plain text for search, mentions for references.
//!
//! A document is a tree of nodes `{ "type": ..., "content": [...], "text": ..., "attrs": {...} }`.
//! A mention is a node of type `mention` whose `attrs.uri` is an entity URI; its `attrs.label`
//! (if any) counts as text.

use serde_json::Value;

/// The document's text: text nodes concatenated, block nodes separated by newlines.
pub fn plain_text(doc: &Value) -> String {
    let mut out = String::new();
    walk_text(doc, &mut out, 0);
    out.trim().to_string()
}

fn walk_text(node: &Value, out: &mut String, depth: usize) {
    if depth > 64 {
        return;
    }
    let Some(obj) = node.as_object() else { return };
    match obj.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(t) = obj.get("text").and_then(Value::as_str) {
                out.push_str(t);
            }
            return;
        }
        Some("mention") => {
            if let Some(l) = obj
                .get("attrs")
                .and_then(|a| a.get("label"))
                .and_then(Value::as_str)
            {
                out.push_str(l);
            }
            return;
        }
        Some("hard_break") | Some("hardBreak") => {
            out.push('\n');
            return;
        }
        _ => {}
    }
    if let Some(content) = obj.get("content").and_then(Value::as_array) {
        let inline = content.iter().all(|c| {
            matches!(
                c.get("type").and_then(Value::as_str),
                Some("text") | Some("mention") | Some("hard_break") | Some("hardBreak")
            )
        });
        for c in content {
            walk_text(c, out, depth + 1);
            if !inline && !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }
}

/// The URIs of every mention in the document, in order, without duplicates.
pub fn mentions(doc: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk_mentions(doc, &mut out, 0);
    out
}

fn walk_mentions(node: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 64 {
        return;
    }
    let Some(obj) = node.as_object() else { return };
    if obj.get("type").and_then(Value::as_str) == Some("mention") {
        if let Some(uri) = obj
            .get("attrs")
            .and_then(|a| a.get("uri"))
            .and_then(Value::as_str)
            && !out.iter().any(|u| u == uri)
        {
            out.push(uri.to_string());
        }
        return;
    }
    if let Some(content) = obj.get("content").and_then(Value::as_array) {
        for c in content {
            walk_mentions(c, out, depth + 1);
        }
    }
}
