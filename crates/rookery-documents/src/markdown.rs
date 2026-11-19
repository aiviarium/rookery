//! Markdown in and out of the shared editor's ProseMirror JSON.
//!
//! - Mentions are links to entity URIs (`[Ada](user://…)`); a link to anything but `http(s)`, `mailto` or
//!   a relative path reads back as a mention.
//! - Callouts are GitHub alerts (`> [!NOTE]`, `[!TIP]`, `[!WARNING]`, `[!CAUTION]`).
//! - Task lists are `- [ ]` and `- [x]`; tables are GitHub tables with the first row as the header.
//! - Plugin blocks render through their contribution's `markdown` (or `text`) template.

use pulldown_cmark::{
    BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use serde_json::{Map, Value, json};

use super::schema::{BlockText, normalize_blocks};

// ── Out ─────────────────────────────────────────────────────────────────────────────────────────────

/// The blocks as Markdown.
pub fn to_markdown(blocks: &[Value], block_md: &BlockText<'_>) -> String {
    let mut out = String::new();
    write_blocks(blocks, &mut out, "", block_md);
    let t = out.trim_end();
    format!("{t}\n")
}

fn ty(v: &Value) -> &str {
    v.get("type").and_then(Value::as_str).unwrap_or("")
}

fn attr<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get("attrs").and_then(|a| a.get(k))
}

fn content(v: &Value) -> &[Value] {
    v.get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn write_blocks(blocks: &[Value], out: &mut String, prefix: &str, block_md: &BlockText<'_>) {
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str(prefix.trim_end());
            out.push('\n');
        }
        write_block(b, out, prefix, block_md);
    }
}

fn push_lines(out: &mut String, prefix: &str, text: &str) {
    for line in text.split('\n') {
        out.push_str(prefix);
        out.push_str(line);
        out.push('\n');
    }
}

fn write_block(b: &Value, out: &mut String, prefix: &str, block_md: &BlockText<'_>) {
    match ty(b) {
        "paragraph" => push_lines(out, prefix, &inline_md(content(b))),
        "heading" => {
            let level = attr(b, "level")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 6) as usize;
            push_lines(
                out,
                prefix,
                &format!("{} {}", "#".repeat(level), inline_md(content(b))),
            );
        }
        "blockquote" => write_blocks(content(b), out, &format!("{prefix}> "), block_md),
        "callout" => {
            let kind = match attr(b, "tone").and_then(Value::as_str).unwrap_or("info") {
                "success" => "TIP",
                "warning" => "WARNING",
                "danger" => "CAUTION",
                _ => "NOTE",
            };
            let p = format!("{prefix}> ");
            push_lines(out, &p, &format!("[!{kind}]"));
            write_blocks(content(b), out, &p, block_md);
        }
        "code_block" => {
            let lang = attr(b, "language").and_then(Value::as_str).unwrap_or("");
            let code: String = content(b)
                .iter()
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect();
            let fence = if code.contains("```") { "````" } else { "```" };
            push_lines(out, prefix, &format!("{fence}{lang}"));
            push_lines(out, prefix, &code);
            push_lines(out, prefix, fence);
        }
        "horizontal_rule" => push_lines(out, prefix, "---"),
        "bullet_list" | "ordered_list" | "task_list" => {
            let mut n = attr(b, "order").and_then(Value::as_u64).unwrap_or(1);
            for item in content(b) {
                let marker = match ty(b) {
                    "ordered_list" => {
                        let m = format!("{n}. ");
                        n += 1;
                        m
                    }
                    "task_list" => {
                        if attr(item, "checked")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            "- [x] ".to_string()
                        } else {
                            "- [ ] ".to_string()
                        }
                    }
                    _ => "- ".to_string(),
                };
                let mut inner = String::new();
                let indent = " ".repeat(if ty(b) == "ordered_list" {
                    marker.len()
                } else {
                    2
                });
                for (i, c) in content(item).iter().enumerate() {
                    if i > 0 && !matches!(ty(c), "bullet_list" | "ordered_list" | "task_list") {
                        inner.push('\n');
                    }
                    write_block(c, &mut inner, "", block_md);
                }
                let inner = inner.trim_end_matches('\n');
                for (i, line) in inner.split('\n').enumerate() {
                    out.push_str(prefix);
                    if i == 0 {
                        out.push_str(&marker);
                    } else if !line.is_empty() {
                        out.push_str(&indent);
                    }
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        "table" => {
            let rows: Vec<Vec<String>> = content(b)
                .iter()
                .map(|r| {
                    content(r)
                        .iter()
                        .map(|c| {
                            content(c)
                                .iter()
                                .map(|p| inline_md(content(p)))
                                .collect::<Vec<_>>()
                                .join("<br>")
                                .replace('|', "\\|")
                        })
                        .collect()
                })
                .collect();
            let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
            for (i, r) in rows.iter().enumerate() {
                let mut cells = r.clone();
                cells.resize(cols, String::new());
                push_lines(out, prefix, &format!("| {} |", cells.join(" | ")));
                if i == 0 {
                    push_lines(out, prefix, &format!("|{}", " --- |".repeat(cols)));
                }
            }
        }
        "image" => {
            let alt = attr(b, "alt").and_then(Value::as_str).unwrap_or("");
            let src = attr(b, "src").and_then(Value::as_str).unwrap_or("");
            push_lines(out, prefix, &format!("![{}]({src})", escape(alt)));
        }
        "file" => {
            let name = attr(b, "name").and_then(Value::as_str).unwrap_or("file");
            let src = attr(b, "src").and_then(Value::as_str).unwrap_or("");
            push_lines(out, prefix, &format!("[{}]({src})", escape(name)));
        }
        "plugin_block" => {
            if let Some(t) = block_md(b) {
                push_lines(out, prefix, &t);
            }
        }
        _ => push_lines(out, prefix, &inline_md(content(b))),
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn marks_of(n: &Value) -> Vec<&Value> {
    let mut m: Vec<&Value> = n
        .get("marks")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    // Links outermost, code innermost.
    let rank = |m: &&Value| match ty(m) {
        "link" => 0,
        "strong" => 1,
        "em" => 2,
        "strike" => 3,
        "underline" => 4,
        _ => 5,
    };
    m.sort_by_key(rank);
    m
}

fn inline_md(nodes: &[Value]) -> String {
    // Merge neighbouring texts with the same marks, so `**a**` stays one span.
    let mut merged: Vec<Value> = Vec::new();
    for n in nodes {
        if ty(n) == "text"
            && let Some(last) = merged.last_mut()
            && ty(last) == "text"
            && last.get("marks") == n.get("marks")
        {
            let joined = format!(
                "{}{}",
                last["text"].as_str().unwrap_or(""),
                n["text"].as_str().unwrap_or("")
            );
            last["text"] = Value::String(joined);
            continue;
        }
        merged.push(n.clone());
    }
    let mut out = String::new();
    for n in &merged {
        match ty(n) {
            "text" => {
                let raw = n.get("text").and_then(Value::as_str).unwrap_or("");
                let marks = marks_of(n);
                let code = marks.iter().any(|m| ty(m) == "code");
                let mut s = if code {
                    let tick = if raw.contains('`') { "``" } else { "`" };
                    format!("{tick}{raw}{tick}")
                } else {
                    escape(raw)
                };
                for m in marks.iter().rev() {
                    s = match ty(m) {
                        "strong" => format!("**{s}**"),
                        "em" => format!("*{s}*"),
                        "strike" => format!("~~{s}~~"),
                        "underline" => format!("<u>{s}</u>"),
                        "link" => {
                            let href = attr(m, "href").and_then(Value::as_str).unwrap_or("");
                            format!("[{s}]({href})")
                        }
                        _ => s,
                    };
                }
                out.push_str(&s);
            }
            "mention" => {
                let uri = attr(n, "uri").and_then(Value::as_str).unwrap_or("");
                let label = attr(n, "label").and_then(Value::as_str).unwrap_or(uri);
                out.push_str(&format!("[{}]({uri})", escape(label)));
            }
            "hard_break" => out.push_str("\\\n"),
            _ => {}
        }
    }
    out
}

// ── In ──────────────────────────────────────────────────────────────────────────────────────────────

struct Frame {
    node: Map<String, Value>,
    content: Vec<Value>,
    /// Opened for inline content in a container (tight list items, table cells).
    implicit: bool,
}

impl Frame {
    fn new(ty: &str, attrs: Option<Value>) -> Frame {
        let mut node = Map::new();
        node.insert("type".into(), Value::String(ty.into()));
        if let Some(a) = attrs {
            node.insert("attrs".into(), a);
        }
        Frame {
            node,
            content: Vec::new(),
            implicit: false,
        }
    }
    fn ty(&self) -> &str {
        self.node.get("type").and_then(Value::as_str).unwrap_or("")
    }
    fn finish(mut self) -> Value {
        if !self.content.is_empty() {
            self.node
                .insert("content".into(), Value::Array(self.content));
        }
        Value::Object(self.node)
    }
}

struct Builder {
    stack: Vec<Frame>,
    marks: Vec<Value>,
    /// A link to an entity being read as a mention: its URI and label so far.
    mention: Option<(String, String)>,
    /// An image being read: src, title, alt so far.
    image: Option<(String, String)>,
    in_head: bool,
}

fn is_entity_uri(href: &str) -> bool {
    match href.split_once("://") {
        Some((scheme, rest)) => {
            !rest.is_empty()
                && !matches!(scheme, "http" | "https" | "ftp" | "file")
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        None => false,
    }
}

impl Builder {
    fn top(&mut self) -> &mut Frame {
        self.stack.last_mut().expect("the doc frame stays")
    }

    fn textblock(&self) -> bool {
        matches!(
            self.stack.last().map(Frame::ty),
            Some("paragraph" | "heading" | "code_block")
        )
    }

    fn push_inline(&mut self, node: Value) {
        if !self.textblock() {
            let mut f = Frame::new("paragraph", None);
            f.implicit = true;
            self.stack.push(f);
        }
        self.top().content.push(node);
    }

    fn close_implicit(&mut self) {
        if self.stack.last().is_some_and(|f| f.implicit) {
            self.pop();
        }
    }

    fn open(&mut self, ty: &str, attrs: Option<Value>) {
        self.close_implicit();
        self.stack.push(Frame::new(ty, attrs));
    }

    fn pop(&mut self) {
        if self.stack.len() <= 1 {
            return;
        }
        let f = self.stack.pop().expect("checked");
        let v = f.finish();
        // Images are blocks: lift them out of the paragraph they were written in.
        if ty(&v) == "paragraph" && content(&v).iter().any(|c| ty(c) == "image") {
            let mut run = Vec::new();
            for c in content(&v) {
                if ty(c) == "image" {
                    if !run.is_empty() {
                        self.top()
                            .content
                            .push(json!({ "type": "paragraph", "content": run }));
                        run = Vec::new();
                    }
                    self.top().content.push(c.clone());
                } else if !(run.is_empty() && ty(c) == "text" && c["text"].as_str() == Some(" ")) {
                    run.push(c.clone());
                }
            }
            if !run.is_empty() {
                self.top()
                    .content
                    .push(json!({ "type": "paragraph", "content": run }));
            }
            return;
        }
        self.top().content.push(v);
    }

    fn close(&mut self, ty: &str) {
        self.close_implicit();
        if self.stack.last().map(Frame::ty) == Some(ty) {
            self.pop();
        }
    }

    fn text(&mut self, s: &str) {
        if let Some((_, label)) = &mut self.mention {
            label.push_str(s);
            return;
        }
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(s);
            return;
        }
        if self.stack.last().map(Frame::ty) == Some("code_block") {
            self.top()
                .content
                .push(json!({ "type": "text", "text": s }));
            return;
        }
        let mut node = json!({ "type": "text", "text": s });
        if !self.marks.is_empty() {
            node["marks"] = Value::Array(self.marks.clone());
        }
        self.push_inline(node);
    }
}

/// Markdown as top-level blocks of the editor's schema.
pub fn from_markdown(md: &str) -> Vec<Value> {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    let mut b = Builder {
        stack: vec![Frame::new("doc", None)],
        marks: Vec::new(),
        mention: None,
        image: None,
        in_head: false,
    };
    for ev in Parser::new_ext(md, opts) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => b.open("paragraph", None),
                Tag::Heading { level, .. } => {
                    let l = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    };
                    b.open("heading", Some(json!({ "level": l })));
                }
                Tag::BlockQuote(kind) => match kind {
                    None => b.open("blockquote", None),
                    Some(k) => {
                        let tone = match k {
                            BlockQuoteKind::Tip => "success",
                            BlockQuoteKind::Warning => "warning",
                            BlockQuoteKind::Caution => "danger",
                            BlockQuoteKind::Note | BlockQuoteKind::Important => "info",
                        };
                        b.open("callout", Some(json!({ "tone": tone })));
                    }
                },
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => {
                            l.split_whitespace().next().unwrap_or("").to_string()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    b.open("code_block", Some(json!({ "language": lang })));
                }
                Tag::List(Some(n)) => b.open("ordered_list", Some(json!({ "order": n }))),
                Tag::List(None) => b.open("bullet_list", None),
                Tag::Item => b.open("list_item", None),
                Tag::Table(_) => b.open("table", None),
                Tag::TableHead => {
                    b.in_head = true;
                    b.open("table_row", None);
                }
                Tag::TableRow => b.open("table_row", None),
                Tag::TableCell => {
                    let t = if b.in_head {
                        "table_header"
                    } else {
                        "table_cell"
                    };
                    b.open(t, None);
                }
                Tag::Emphasis => b.marks.push(json!({ "type": "em" })),
                Tag::Strong => b.marks.push(json!({ "type": "strong" })),
                Tag::Strikethrough => b.marks.push(json!({ "type": "strike" })),
                Tag::Link { dest_url, .. } => {
                    if is_entity_uri(&dest_url) {
                        b.mention = Some((dest_url.to_string(), String::new()));
                    } else {
                        b.marks.push(
                            json!({ "type": "link", "attrs": { "href": dest_url.to_string() } }),
                        );
                    }
                }
                Tag::Image { dest_url, .. } => {
                    b.image = Some((dest_url.to_string(), String::new()))
                }
                _ => {}
            },
            Event::End(tag) => {
                match tag {
                    TagEnd::Paragraph => b.close("paragraph"),
                    TagEnd::Heading(_) => b.close("heading"),
                    TagEnd::BlockQuote(_) => {
                        b.close_implicit();
                        if matches!(
                            b.stack.last().map(Frame::ty),
                            Some("blockquote" | "callout")
                        ) {
                            b.pop();
                        }
                    }
                    TagEnd::CodeBlock => {
                        if let Some(f) = b.stack.last_mut()
                            && f.ty() == "code_block"
                        {
                            let code: String = f
                                .content
                                .iter()
                                .filter_map(|c| c.get("text").and_then(Value::as_str))
                                .collect();
                            let code = code.strip_suffix('\n').unwrap_or(&code).to_string();
                            f.content = if code.is_empty() {
                                vec![]
                            } else {
                                vec![json!({ "type": "text", "text": code })]
                            };
                        }
                        b.close("code_block");
                    }
                    TagEnd::List(_) => {
                        b.close_implicit();
                        if let Some(f) = b.stack.last_mut()
                            && f.content.iter().any(|i| ty(i) == "task_item")
                        {
                            f.node.insert("type".into(), "task_list".into());
                            f.node.remove("attrs");
                            for i in f.content.iter_mut() {
                                if ty(i) == "list_item" {
                                    i["type"] = "task_item".into();
                                    i["attrs"] = json!({ "checked": false });
                                }
                            }
                        }
                        b.pop();
                    }
                    TagEnd::Item => {
                        b.close_implicit();
                        b.pop();
                    }
                    TagEnd::Table => b.close("table"),
                    TagEnd::TableHead => {
                        b.in_head = false;
                        b.close("table_row");
                    }
                    TagEnd::TableRow => b.close("table_row"),
                    TagEnd::TableCell => {
                        b.close_implicit();
                        b.pop();
                    }
                    TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                        b.marks.pop();
                    }
                    TagEnd::Link => {
                        if let Some((uri, label)) = b.mention.take() {
                            b.push_inline(json!({ "type": "mention", "attrs": { "uri": uri, "label": label } }));
                        } else {
                            b.marks.pop();
                        }
                    }
                    TagEnd::Image => {
                        if let Some((src, alt)) = b.image.take() {
                            // An image served by the blobs facility keeps its blob, as one added in the editor does.
                            let blob = src
                                .strip_prefix("/blobs/")
                                .map(|id| id.split(['?', '#']).next().unwrap_or(id))
                                .filter(|id| !id.is_empty() && !id.contains('/'))
                                .map(|id| format!("blob://{id}"));
                            let mut attrs = json!({ "src": src, "alt": alt });
                            if let Some(uri) = blob {
                                attrs["blob"] = uri.into();
                            }
                            b.push_inline(json!({ "type": "image", "attrs": attrs }));
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => b.text(&t),
            Event::Code(t) => {
                if b.mention.is_some() || b.image.is_some() {
                    b.text(&t);
                } else {
                    let mut marks = b.marks.clone();
                    marks.push(json!({ "type": "code" }));
                    b.push_inline(json!({ "type": "text", "text": t.to_string(), "marks": marks }));
                }
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                let s = t.to_string();
                if !matches!(s.trim(), "<u>" | "</u>" | "<br>" | "<br/>") {
                    b.text(&s);
                }
            }
            Event::SoftBreak => b.text(" "),
            Event::HardBreak => b.push_inline(json!({ "type": "hard_break" })),
            Event::Rule => {
                b.close_implicit();
                b.top().content.push(json!({ "type": "horizontal_rule" }));
            }
            Event::TaskListMarker(checked) => {
                // The marker comes first in its item; the item frame is below any implicit paragraph.
                let idx = b
                    .stack
                    .iter()
                    .rposition(|f| f.ty() == "list_item")
                    .unwrap_or(0);
                if idx > 0 {
                    let f = &mut b.stack[idx];
                    f.node.insert("type".into(), "task_item".into());
                    f.node.insert("attrs".into(), json!({ "checked": checked }));
                }
            }
            _ => {}
        }
    }
    while b.stack.len() > 1 {
        b.pop();
    }
    let doc = b.stack.pop().expect("doc frame").finish();
    normalize_blocks(content(&doc))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: &BlockText<'static> = &|_| None;

    #[test]
    fn reads_markdown() {
        let md = "# Plan\n\nAsk **Ada** about [the issue](issue://1) and *[docs](https://x.y)*.\n\n\
                  - one\n- two\n  - nested\n\n1. first\n2. second\n\n- [x] done\n- [ ] open\n\n\
                  > [!WARNING]\n> Careful\n\n```rust\nfn main() {}\n```\n\n---\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n\
                  ![chart](/blobs/1)\n";
        let blocks = from_markdown(md);
        let types: Vec<&str> = blocks.iter().map(ty).collect();
        assert_eq!(
            types,
            [
                "heading",
                "paragraph",
                "bullet_list",
                "ordered_list",
                "task_list",
                "callout",
                "code_block",
                "horizontal_rule",
                "table",
                "image"
            ]
        );
        let p = content(&blocks[1]);
        assert_eq!(p[1]["marks"][0]["type"], "strong");
        assert_eq!(p[3]["type"], "mention");
        assert_eq!(p[3]["attrs"]["uri"], "issue://1");
        assert_eq!(p[3]["attrs"]["label"], "the issue");
        assert_eq!(blocks[4]["content"][0]["attrs"]["checked"], true);
        assert_eq!(blocks[5]["attrs"]["tone"], "warning");
        assert_eq!(blocks[6]["content"][0]["text"], "fn main() {}");
        assert_eq!(
            blocks[8]["content"][0]["content"][0]["type"],
            "table_header"
        );
        assert_eq!(blocks[2]["content"][1]["content"][1]["type"], "bullet_list");
        assert_eq!(blocks[9]["attrs"]["blob"], "blob://1");
    }

    #[test]
    fn markdown_round_trips() {
        let md = "# Plan\n\nAsk **Ada** about [the issue](issue://1).\n\n- one\n- two\n\n- [x] done\n\n\
                  > quoted\n\n```js\nlet a = 1\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n";
        let once = to_markdown(&from_markdown(md), NONE);
        let twice = to_markdown(&from_markdown(&once), NONE);
        assert_eq!(once, twice);
        assert!(once.contains("**Ada**"), "{once}");
        assert!(once.contains("[the issue](issue://1)"), "{once}");
        assert!(once.contains("- [x] done"), "{once}");
        assert!(once.contains("```js\nlet a = 1\n```"), "{once}");
    }
}
