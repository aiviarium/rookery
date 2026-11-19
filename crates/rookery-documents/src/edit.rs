//! Server-side edits (`documents.edit`) on the XML fragment, in one transaction.

use rookery_proto::RpcError;
use rookery_proto::documents::DocumentEditOp;
use serde_json::Value;
use yrs::{Text, TransactionMut, XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim};

use super::markdown::from_markdown;
use super::schema::{insert_blocks, normalize_content};

/// The blocks an op inserts: its JSON content, else its Markdown.
pub fn content_blocks(
    content: &Option<Value>,
    markdown: &Option<String>,
) -> Result<Vec<Value>, RpcError> {
    match (content, markdown) {
        (Some(c), _) => Ok(normalize_content(c)),
        (None, Some(md)) => Ok(from_markdown(md)),
        (None, None) => Err(RpcError::invalid("give content or markdown")),
    }
}

fn range(from: u32, to: u32, len: u32) -> Result<(), RpcError> {
    if from > to || to > len {
        return Err(RpcError::invalid(format!(
            "blocks {from}..{to} are outside the document, which has {len}"
        )));
    }
    Ok(())
}

/// Applies the operations in order.
pub fn apply(
    txn: &mut TransactionMut,
    frag: &XmlFragmentRef,
    ops: &[DocumentEditOp],
) -> Result<(), RpcError> {
    for op in ops {
        let len = frag.len(txn);
        match op {
            DocumentEditOp::Insert {
                at,
                content,
                markdown,
            } => {
                let blocks = content_blocks(content, markdown)?;
                let at = at.unwrap_or(len).min(len);
                insert_blocks(txn, frag, at, &blocks);
            }
            DocumentEditOp::ReplaceBlocks {
                from,
                to,
                content,
                markdown,
            } => {
                range(*from, *to, len)?;
                let blocks = content_blocks(content, markdown)?;
                if to > from {
                    frag.remove_range(txn, *from, to - from);
                }
                insert_blocks(txn, frag, *from, &blocks);
            }
            DocumentEditOp::DeleteBlocks { from, to } => {
                range(*from, *to, len)?;
                if to > from {
                    frag.remove_range(txn, *from, to - from);
                }
            }
            DocumentEditOp::ReplaceText {
                block,
                from,
                to,
                text,
            } => replace_text(txn, frag, *block, *from, *to, text)?,
            DocumentEditOp::AppendMarkdown { markdown } => {
                let blocks = from_markdown(markdown);
                insert_blocks(txn, frag, len, &blocks);
            }
            DocumentEditOp::SetContent { content, markdown } => {
                let blocks = content_blocks(content, markdown)?;
                if len > 0 {
                    frag.remove_range(txn, 0, len);
                }
                insert_blocks(txn, frag, 0, &blocks);
            }
        }
    }
    Ok(())
}

fn child_len(txn: &TransactionMut, c: &XmlOut) -> u32 {
    match c {
        XmlOut::Text(t) => t.len(txn),
        _ => 1,
    }
}

fn replace_text(
    txn: &mut TransactionMut,
    frag: &XmlFragmentRef,
    block: u32,
    from: u32,
    to: u32,
    text: &str,
) -> Result<(), RpcError> {
    let Some(XmlOut::Element(el)) = frag.get(txn, block) else {
        return Err(RpcError::invalid(format!("there is no block {block}")));
    };
    if !matches!(el.tag().as_ref(), "paragraph" | "heading" | "code_block") {
        return Err(RpcError::invalid(format!(
            "block {block} is a {}, not a text block",
            el.tag()
        )));
    }
    let children: Vec<XmlOut> = el.children(txn).collect();
    let lens: Vec<u32> = children.iter().map(|c| child_len(txn, c)).collect();
    let total: u32 = lens.iter().sum();
    if from > to || to > total {
        return Err(RpcError::invalid(format!(
            "characters {from}..{to} are outside block {block}, which has {total}"
        )));
    }
    // Delete from the end, so earlier offsets and indexes stay put.
    let mut starts = Vec::with_capacity(lens.len());
    let mut s = 0;
    for l in &lens {
        starts.push(s);
        s += l;
    }
    for i in (0..children.len()).rev() {
        let (a, b) = (starts[i].max(from), (starts[i] + lens[i]).min(to));
        if a >= b {
            continue;
        }
        match &children[i] {
            XmlOut::Text(t) => t.remove_range(txn, a - starts[i], b - a),
            _ => el.remove_range(txn, i as u32, 1),
        }
    }
    if text.is_empty() {
        return Ok(());
    }
    let children: Vec<XmlOut> = el.children(txn).collect();
    let mut s = 0;
    for (i, c) in children.iter().enumerate() {
        let l = child_len(txn, c);
        if let XmlOut::Text(t) = c
            && from >= s
            && from <= s + l
        {
            t.insert(txn, from - s, text);
            return Ok(());
        }
        if s >= from {
            el.insert(txn, i as u32, XmlTextPrelim::new(text));
            return Ok(());
        }
        s += l;
    }
    el.push_back(txn, XmlTextPrelim::new(text));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FRAGMENT, to_json};
    use serde_json::json;
    use yrs::{Doc, OffsetKind, Options, Transact};

    fn doc() -> (Doc, XmlFragmentRef) {
        let d = Doc::with_options(Options {
            offset_kind: OffsetKind::Utf16,
            ..Default::default()
        });
        let f = d.get_or_insert_xml_fragment(FRAGMENT);
        (d, f)
    }

    #[test]
    fn ops_apply_in_order() {
        let (d, f) = doc();
        {
            let mut txn = d.transact_mut();
            apply(
                &mut txn,
                &f,
                &[
                    DocumentEditOp::AppendMarkdown {
                        markdown: "# Title\n\nHello wörld, see @[x](note://1)".into(),
                    },
                    DocumentEditOp::Insert {
                        at: Some(1),
                        content: Some(json!({ "type": "paragraph", "content": [
                            { "type": "text", "text": "ab" },
                            { "type": "mention", "attrs": { "uri": "user://u", "label": "U" } },
                            { "type": "text", "text": "cd" }
                        ]})),
                        markdown: None,
                    },
                    // "ab@cd" -> "aXd": deletes across the mention.
                    DocumentEditOp::ReplaceText {
                        block: 1,
                        from: 1,
                        to: 4,
                        text: "X".into(),
                    },
                    DocumentEditOp::ReplaceText {
                        block: 2,
                        from: 6,
                        to: 11,
                        text: "world".into(),
                    },
                ],
            )
            .unwrap();
        }
        let j = to_json(&d.transact(), &f);
        assert_eq!(j["content"][0]["type"], "heading");
        let p1: String = j["content"][1]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n["text"].as_str())
            .collect();
        assert_eq!(p1, "aXd");
        assert_eq!(j["content"][2]["content"][0]["text"], "Hello world, see @");
        {
            let mut txn = d.transact_mut();
            assert!(
                apply(
                    &mut txn,
                    &f,
                    &[DocumentEditOp::DeleteBlocks { from: 2, to: 9 }]
                )
                .is_err()
            );
            apply(
                &mut txn,
                &f,
                &[DocumentEditOp::ReplaceBlocks {
                    from: 0,
                    to: 1,
                    content: None,
                    markdown: Some("## New".into()),
                }],
            )
            .unwrap();
        }
        let j = to_json(&d.transact(), &f);
        assert_eq!(j["content"][0]["attrs"]["level"], 2);
        assert_eq!(j["content"].as_array().unwrap().len(), 3);
    }
}
