//! The shared editor's document format on the kernel's side: ProseMirror JSON in and out of the Yjs XML
//! fragment, as `y-prosemirror` lays it out, and what the indexes take from it.
//!
//! `y-prosemirror` stores a node as an XML element named after its type, with the node's attributes as
//! element attributes (`null`s left out), and a run of text as one XML text whose formatting attributes
//! are the marks: the mark's name (with a `--<hash>` suffix when the mark may overlap itself) mapped to
//! its attributes. Inline nodes such as mentions are elements between the texts of their parent.
//!
//! The node and mark names must match `ui/src/editor/schema.ts`. A node the editor's schema cannot hold
//! is deleted by `y-prosemirror` when it loads, so content from plugins is normalised here first.

use serde_json::{Map, Value, json};
use yrs::types::Attrs;
use yrs::types::ToJson;
use yrs::types::text::YChange;
use yrs::{
    Any, Out, Text, TransactionMut, Xml, XmlElementPrelim, XmlElementRef, XmlFragment,
    XmlFragmentRef, XmlOut, XmlTextPrelim, XmlTextRef,
};

/// The XML fragment the editor binds to.
pub const FRAGMENT: &str = "prosemirror";

/// Block nodes that hold inline content.
const TEXTBLOCKS: &[&str] = &["paragraph", "heading", "code_block"];
/// Block nodes that hold blocks.
const CONTAINERS: &[&str] = &["blockquote", "callout"];
/// Leaf blocks.
const ATOM_BLOCKS: &[&str] = &["horizontal_rule", "image", "file", "plugin_block"];
const LISTS: &[(&str, &str)] = &[
    ("bullet_list", "list_item"),
    ("ordered_list", "list_item"),
    ("task_list", "task_item"),
];
const INLINE_ATOMS: &[&str] = &["mention", "hard_break"];
const MARKS: &[&str] = &["strong", "em", "underline", "strike", "code", "link"];

fn is_block(t: &str) -> bool {
    TEXTBLOCKS.contains(&t)
        || CONTAINERS.contains(&t)
        || ATOM_BLOCKS.contains(&t)
        || LISTS.iter().any(|(l, _)| *l == t)
        || t == "table"
}

fn node_type(v: &Value) -> &str {
    v.get("type").and_then(Value::as_str).unwrap_or("")
}

fn is_inline(v: &Value) -> bool {
    let t = node_type(v);
    t == "text" || INLINE_ATOMS.contains(&t)
}

// ── Yjs to ProseMirror JSON ─────────────────────────────────────────────────────────────────────────

/// The document as ProseMirror JSON: `{ "type": "doc", "content": [...] }`.
pub fn to_json<T: yrs::ReadTxn>(txn: &T, frag: &XmlFragmentRef) -> Value {
    json!({ "type": "doc", "content": children_json(txn, frag) })
}

/// The top-level blocks.
pub fn blocks_json<T: yrs::ReadTxn>(txn: &T, frag: &XmlFragmentRef) -> Vec<Value> {
    children_json(txn, frag)
}

fn children_json<T: yrs::ReadTxn, F: XmlFragment>(txn: &T, parent: &F) -> Vec<Value> {
    let mut out = Vec::new();
    for child in parent.children(txn) {
        match child {
            XmlOut::Element(el) => out.push(element_json(txn, &el)),
            XmlOut::Text(t) => out.extend(text_json(txn, &t)),
            XmlOut::Fragment(_) => {}
        }
    }
    out
}

fn element_json<T: yrs::ReadTxn>(txn: &T, el: &XmlElementRef) -> Value {
    let mut node = Map::new();
    node.insert("type".into(), Value::String(el.tag().to_string()));
    let mut attrs: Vec<(String, Value)> = el
        .attributes(txn)
        .map(|(k, v)| (k.to_string(), out_json(txn, v)))
        .filter(|(k, _)| k != "ychange")
        .collect();
    if !attrs.is_empty() {
        attrs.sort_by(|a, b| a.0.cmp(&b.0));
        node.insert("attrs".into(), Value::Object(attrs.into_iter().collect()));
    }
    let content = children_json(txn, el);
    if !content.is_empty() {
        node.insert("content".into(), Value::Array(content));
    }
    Value::Object(node)
}

fn text_json<T: yrs::ReadTxn>(txn: &T, t: &XmlTextRef) -> Vec<Value> {
    let mut out = Vec::new();
    for d in t.diff(txn, YChange::identity) {
        let Out::Any(Any::String(s)) = &d.insert else {
            continue;
        };
        if s.is_empty() {
            continue;
        }
        let mut node = Map::new();
        node.insert("type".into(), "text".into());
        node.insert("text".into(), Value::String(s.to_string()));
        if let Some(attrs) = &d.attributes {
            let mut marks: Vec<Value> = attrs
                .iter()
                .map(|(name, v)| {
                    let mut m = Map::new();
                    m.insert("type".into(), Value::String(mark_name(name).to_string()));
                    let a = any_json(v);
                    if a.as_object().is_some_and(|o| !o.is_empty()) {
                        m.insert("attrs".into(), a);
                    }
                    Value::Object(m)
                })
                .collect();
            marks.sort_by(|a, b| node_type(a).cmp(node_type(b)));
            if !marks.is_empty() {
                node.insert("marks".into(), Value::Array(marks));
            }
        }
        out.push(Value::Object(node));
    }
    out
}

/// A formatting attribute's mark name, without the `--<hash>` suffix of overlapping marks.
fn mark_name(attr: &str) -> &str {
    match attr.rsplit_once("--") {
        Some((name, hash)) if hash.len() == 8 => name,
        _ => attr,
    }
}

fn any_json(a: &Any) -> Value {
    serde_json::to_value(a).unwrap_or(Value::Null)
}

fn out_json<T: yrs::ReadTxn>(txn: &T, o: Out) -> Value {
    match o {
        Out::Any(a) => any_json(&a),
        other => serde_json::to_value(other.to_json(txn)).unwrap_or(Value::Null),
    }
}

fn json_any(v: &Value) -> Any {
    serde_json::from_value(v.clone()).unwrap_or(Any::Null)
}

// ── ProseMirror JSON to Yjs ─────────────────────────────────────────────────────────────────────────

/// Inserts block nodes into the fragment at `index`.
pub fn insert_blocks(
    txn: &mut TransactionMut,
    frag: &XmlFragmentRef,
    index: u32,
    blocks: &[Value],
) {
    insert_children(txn, frag, index, blocks);
}

fn insert_children<F: XmlFragment>(
    txn: &mut TransactionMut,
    parent: &F,
    mut index: u32,
    nodes: &[Value],
) {
    let mut run: Vec<&Value> = Vec::new();
    for n in nodes {
        if node_type(n) == "text" {
            run.push(n);
            continue;
        }
        if !run.is_empty() {
            insert_text_run(txn, parent, index, &run);
            index += 1;
            run.clear();
        }
        let el = parent.insert(
            txn,
            index,
            XmlElementPrelim::empty(node_type(n).to_string()),
        );
        if let Some(attrs) = n.get("attrs").and_then(Value::as_object) {
            for (k, v) in attrs {
                if !v.is_null() {
                    el.insert_attribute(txn, k.as_str(), json_any(v));
                }
            }
        }
        if let Some(content) = n.get("content").and_then(Value::as_array) {
            insert_children(txn, &el, 0, content);
        }
        index += 1;
    }
    if !run.is_empty() {
        insert_text_run(txn, parent, index, &run);
    }
}

fn insert_text_run<F: XmlFragment>(
    txn: &mut TransactionMut,
    parent: &F,
    index: u32,
    run: &[&Value],
) {
    let t = parent.insert(txn, index, XmlTextPrelim::new(""));
    let mut at = 0u32;
    for n in run {
        let s = n.get("text").and_then(Value::as_str).unwrap_or("");
        if s.is_empty() {
            continue;
        }
        t.insert_with_attributes(txn, at, s, marks_attrs(n));
        at += s.encode_utf16().count() as u32;
    }
}

fn marks_attrs(n: &Value) -> Attrs {
    let mut attrs = Attrs::new();
    for m in n
        .get("marks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = node_type(m);
        if name.is_empty() {
            continue;
        }
        let a = m.get("attrs").cloned().unwrap_or_else(|| json!({}));
        attrs.insert(name.into(), json_any(&a));
    }
    attrs
}

// ── Normalising content from outside ────────────────────────────────────────────────────────────────

/// Content given as a `doc`, a node or a list of nodes, as top-level blocks the editor's schema holds.
pub fn normalize_content(v: &Value) -> Vec<Value> {
    let nodes: Vec<Value> = match v {
        Value::Array(a) => a.clone(),
        Value::Object(_) if node_type(v) == "doc" => v
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Value::Object(_) => vec![v.clone()],
        Value::String(s) => vec![paragraph(vec![text(s)])],
        _ => Vec::new(),
    };
    normalize_blocks(&nodes)
}

fn text(s: &str) -> Value {
    json!({ "type": "text", "text": s })
}

fn paragraph(content: Vec<Value>) -> Value {
    if content.is_empty() {
        json!({ "type": "paragraph" })
    } else {
        json!({ "type": "paragraph", "content": content })
    }
}

/// Blocks: inline runs are wrapped in paragraphs, unknown nodes become paragraphs of their text.
pub fn normalize_blocks(nodes: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut inline: Vec<Value> = Vec::new();
    for n in nodes {
        if is_inline(n) {
            inline.extend(normalize_inline(std::slice::from_ref(n), true));
            continue;
        }
        if !inline.is_empty() {
            out.push(paragraph(std::mem::take(&mut inline)));
        }
        out.push(normalize_block(n));
    }
    if !inline.is_empty() {
        out.push(paragraph(inline));
    }
    out
}

fn with_content(n: &Value, content: Vec<Value>) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), Value::String(node_type(n).to_string()));
    if let Some(a) = n
        .get("attrs")
        .filter(|a| a.as_object().is_some_and(|o| !o.is_empty()))
    {
        o.insert("attrs".into(), a.clone());
    }
    if !content.is_empty() {
        o.insert("content".into(), Value::Array(content));
    }
    Value::Object(o)
}

fn content_of(n: &Value) -> &[Value] {
    n.get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn normalize_block(n: &Value) -> Value {
    let t = node_type(n);
    if !is_block(t) {
        // Something the editor does not know: keep its text.
        let s = crate::schema::plain_text_of(n, &|_| None);
        return paragraph(if s.is_empty() { vec![] } else { vec![text(&s)] });
    }
    if t == "code_block" {
        let s: String = content_of(n)
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect();
        return with_content(n, if s.is_empty() { vec![] } else { vec![text(&s)] });
    }
    if TEXTBLOCKS.contains(&t) {
        return with_content(n, normalize_inline(content_of(n), true));
    }
    if CONTAINERS.contains(&t) {
        let mut c = normalize_blocks(content_of(n));
        if c.is_empty() {
            c.push(paragraph(vec![]));
        }
        return with_content(n, c);
    }
    if let Some((_, item)) = LISTS.iter().find(|(l, _)| *l == t) {
        let mut items: Vec<Value> = content_of(n)
            .iter()
            .map(|c| {
                let inner = if node_type(c) == "list_item" || node_type(c) == "task_item" {
                    content_of(c).to_vec()
                } else {
                    vec![c.clone()]
                };
                let mut blocks = normalize_blocks(&inner);
                if blocks.first().map(node_type) != Some("paragraph") {
                    blocks.insert(0, paragraph(vec![]));
                }
                let mut it = Map::new();
                it.insert("type".into(), Value::String(item.to_string()));
                if *item == "task_item" {
                    let checked = c
                        .get("attrs")
                        .and_then(|a| a.get("checked"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    it.insert("attrs".into(), json!({ "checked": checked }));
                }
                it.insert("content".into(), Value::Array(blocks));
                Value::Object(it)
            })
            .collect();
        if items.is_empty() {
            let mut it = Map::new();
            it.insert("type".into(), Value::String(item.to_string()));
            it.insert("content".into(), json!([{ "type": "paragraph" }]));
            items.push(Value::Object(it));
        }
        return with_content(n, items);
    }
    if t == "table" {
        let rows: Vec<Value> = content_of(n)
            .iter()
            .filter(|r| node_type(r) == "table_row")
            .map(|r| {
                let cells: Vec<Value> = content_of(r)
                    .iter()
                    .map(|c| {
                        let kind = if node_type(c) == "table_header" {
                            "table_header"
                        } else {
                            "table_cell"
                        };
                        let mut blocks = normalize_blocks(content_of(c));
                        if blocks.is_empty() {
                            blocks.push(paragraph(vec![]));
                        }
                        let mut o = Map::new();
                        o.insert("type".into(), kind.into());
                        if let Some(a) = c.get("attrs") {
                            o.insert("attrs".into(), a.clone());
                        }
                        o.insert("content".into(), Value::Array(blocks));
                        Value::Object(o)
                    })
                    .collect();
                json!({ "type": "table_row", "content": cells })
            })
            .filter(|r| !content_of(r).is_empty())
            .collect();
        if rows.is_empty() {
            return paragraph(vec![]);
        }
        return with_content(n, rows);
    }
    // Atom blocks keep their attributes and lose any content.
    with_content(n, vec![])
}

/// Inline content: known inline nodes, marks limited to the editor's.
fn normalize_inline(nodes: &[Value], allow_marks: bool) -> Vec<Value> {
    let mut out = Vec::new();
    for n in nodes {
        match node_type(n) {
            "text" => {
                let s = n.get("text").and_then(Value::as_str).unwrap_or("");
                if s.is_empty() {
                    continue;
                }
                let mut o = Map::new();
                o.insert("type".into(), "text".into());
                o.insert("text".into(), Value::String(s.to_string()));
                if allow_marks && let Some(marks) = n.get("marks").and_then(Value::as_array) {
                    let ms: Vec<Value> = marks
                        .iter()
                        .filter(|m| MARKS.contains(&node_type(m)))
                        .cloned()
                        .collect();
                    if !ms.is_empty() {
                        o.insert("marks".into(), Value::Array(ms));
                    }
                }
                out.push(Value::Object(o));
            }
            "mention" | "hard_break" => out.push(with_content(n, vec![])),
            _ => {
                let s = plain_text_of(n, &|_| None);
                if !s.is_empty() {
                    out.push(text(&s));
                }
            }
        }
    }
    out
}

// ── What the indexes take ───────────────────────────────────────────────────────────────────────────

/// Plain text for a plugin block: its contribution's `text` template with the attributes filled in.
pub type BlockText<'a> = dyn Fn(&Value) -> Option<String> + 'a;

/// A node's plain text: text concatenated, blocks separated by newlines, mentions by their label, plugin
/// blocks by `block_text`.
pub fn plain_text_of(node: &Value, block_text: &BlockText<'_>) -> String {
    let mut out = String::new();
    walk_text(node, &mut out, 0, block_text);
    out.trim().to_string()
}

fn walk_text(node: &Value, out: &mut String, depth: usize, block_text: &BlockText<'_>) {
    if depth > 64 {
        return;
    }
    match node_type(node) {
        "text" => {
            if let Some(t) = node.get("text").and_then(Value::as_str) {
                out.push_str(t);
            }
            return;
        }
        "mention" => {
            if let Some(l) = node
                .get("attrs")
                .and_then(|a| a.get("label"))
                .and_then(Value::as_str)
            {
                out.push_str(l);
            }
            return;
        }
        "hard_break" => {
            out.push('\n');
            return;
        }
        "plugin_block" => {
            if let Some(t) = block_text(node) {
                out.push_str(&t);
            }
            return;
        }
        "image" | "file" => {
            let a = node.get("attrs");
            let s = a
                .and_then(|a| a.get("alt").or_else(|| a.get("name")))
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push_str(s);
            return;
        }
        _ => {}
    }
    let content = content_of(node);
    let inline = content.iter().all(is_inline);
    for c in content {
        walk_text(c, out, depth + 1, block_text);
        if !inline && !out.ends_with('\n') {
            out.push('\n');
        }
    }
}

/// The document's title: the first block's text, first line, at most 120 characters.
pub fn title_of(blocks: &[Value], block_text: &BlockText<'_>) -> String {
    for b in blocks {
        let t = plain_text_of(b, block_text);
        let line = t.lines().map(str::trim).find(|l| !l.is_empty());
        if let Some(l) = line {
            return l.chars().take(120).collect();
        }
    }
    String::new()
}

/// Entity URIs the document references: mentions, and the blobs of images and files (so the blob
/// collector keeps them). In order, without duplicates.
pub fn references(node: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk_refs(node, &mut out, 0);
    out
}

fn walk_refs(node: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 64 {
        return;
    }
    let attrs = node.get("attrs");
    let uri = match node_type(node) {
        "mention" => attrs.and_then(|a| a.get("uri")),
        "image" | "file" => attrs.and_then(|a| a.get("blob")),
        _ => None,
    };
    if let Some(u) = uri.and_then(Value::as_str)
        && u.contains("://")
        && !out.iter().any(|x| x == u)
    {
        out.push(u.to_string());
    }
    for c in content_of(node) {
        walk_refs(c, out, depth + 1);
    }
}

/// Fills `{name}` in a template from an object's fields; missing fields are empty.
pub fn fill_template(template: &str, attrs: &Value) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                match attrs.get(key) {
                    Some(Value::String(s)) => out.push_str(s),
                    Some(Value::Null) | None => {}
                    Some(v) => out.push_str(&v.to_string()),
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character length of an inline node in a text block, as the editor counts offsets.
pub fn inline_len(node: &Value) -> u32 {
    match node_type(node) {
        "text" => node
            .get("text")
            .and_then(Value::as_str)
            .map(|s| s.encode_utf16().count() as u32)
            .unwrap_or(0),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::{Doc, Transact};

    fn roundtrip(blocks: Vec<Value>) -> Value {
        let doc = Doc::new();
        let frag = doc.get_or_insert_xml_fragment(FRAGMENT);
        {
            let mut txn = doc.transact_mut();
            insert_blocks(&mut txn, &frag, 0, &blocks);
        }
        to_json(&doc.transact(), &frag)
    }

    #[test]
    fn json_survives_yjs() {
        let blocks = vec![
            json!({ "type": "heading", "attrs": { "level": 2 }, "content": [{ "type": "text", "text": "Plan" }] }),
            json!({ "type": "paragraph", "content": [
                { "type": "text", "text": "Ask " },
                { "type": "text", "text": "Ada", "marks": [{ "type": "strong" }] },
                { "type": "text", "text": " about " },
                { "type": "mention", "attrs": { "label": "the issue", "uri": "issue://1" } },
                { "type": "text", "text": " now", "marks": [{ "type": "link", "attrs": { "href": "https://x.y" } }] }
            ]}),
            json!({ "type": "task_list", "content": [
                { "type": "task_item", "attrs": { "checked": true }, "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": "done" }] }] }
            ]}),
            json!({ "type": "paragraph" }),
        ];
        let out = roundtrip(blocks.clone());
        assert_eq!(out["content"], Value::Array(blocks));
        assert_eq!(out["content"][0]["attrs"]["level"], json!(2));
    }

    #[test]
    fn normalising_wraps_and_repairs() {
        let got = normalize_content(&json!([
            { "type": "text", "text": "loose" },
            { "type": "mystery", "content": [{ "type": "text", "text": "kept" }] },
            { "type": "bullet_list", "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": "a" }] }] },
            { "type": "blockquote" }
        ]));
        assert_eq!(node_type(&got[0]), "paragraph");
        assert_eq!(got[1]["content"][0]["text"], "kept");
        assert_eq!(got[2]["content"][0]["type"], "list_item");
        assert_eq!(got[3]["content"][0]["type"], "paragraph");
    }

    #[test]
    fn text_title_and_references() {
        let doc = json!({ "type": "doc", "content": [
            { "type": "paragraph" },
            { "type": "heading", "attrs": { "level": 1 }, "content": [{ "type": "text", "text": "  Launch plan " }] },
            { "type": "paragraph", "content": [
                { "type": "mention", "attrs": { "uri": "user://a", "label": "@ada" } },
                { "type": "mention", "attrs": { "uri": "user://a", "label": "@ada" } }
            ]},
            { "type": "image", "attrs": { "src": "/blobs/b1", "blob": "blob://b1", "alt": "chart" } },
            { "type": "plugin_block", "attrs": { "type": "probe.counter", "attrs": { "label": "Visits", "count": 3 } } }
        ]});
        let bt = |n: &Value| Some(fill_template("{label}: {count}", &n["attrs"]["attrs"]));
        let text = plain_text_of(&doc, &bt);
        assert_eq!(text, "Launch plan \n@ada@ada\nchart\nVisits: 3");
        assert_eq!(
            title_of(doc["content"].as_array().unwrap(), &bt),
            "Launch plan"
        );
        assert_eq!(references(&doc), vec!["user://a", "blob://b1"]);
        assert_eq!(fill_template("{a} and {b", &json!({ "a": 1 })), "1 and {b");
    }
}
