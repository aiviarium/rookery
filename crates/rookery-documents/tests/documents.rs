//! Collaborative documents through the kernel: sync over a stream with `yrs` peers standing in for
//! editors, persistence and compaction, catch-up after offline edits, permissions, what the indexes get,
//! history and restore, server-side edits, and convergence under concurrent random edits.

mod common;

use std::time::Duration;

use common::*;
use rookery_documents::live::{self, Tuning};
use rookery_documents::{frame, take_frame};
use rookery_proto::stream::{decode_frame, encode_frame};
use serde_json::{Value, json};
use yrs::encoding::read::Cursor;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::decoder::{Decode, DecoderV1};
use yrs::updates::encoder::Encode;
use yrs::{
    Doc, GetString, OffsetKind, Options, ReadTxn, StateVector, Text, Transact, Update, Xml,
    XmlElementPrelim, XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim,
};

fn code(r: &Value) -> String {
    r["error"]["data"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error, got {r}"))
        .to_string()
}

async fn user_id(ws: &mut Ws) -> String {
    ws.call("auth.me", json!({})).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A space of kind `jot.team` owned by `owner`, with `use` for the others named.
async fn team(owner: &mut Ws, members: &[(&str, &str)]) -> String {
    for _ in 0..100 {
        let r = owner
            .call_raw(
                "spaces.create",
                json!({ "kind": "jot.team", "name": "Team" }),
            )
            .await;
        if r.get("error").is_none() {
            let space = r["result"]["id"].as_str().unwrap().to_string();
            for (who, level) in members {
                owner
                    .call(
                        "access.grant",
                        json!({ "resource": format!("space://{space}"), "principal": format!("user:{who}"), "level": level }),
                    )
                    .await;
            }
            return space;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the jot plugin did not load");
}

async fn jot(ws: &mut Ws, space: &str, title: &str) -> String {
    for _ in 0..100 {
        let r = ws
            .call_raw(
                "data.insert",
                json!({ "plugin": "jot", "collection": "jots", "space": space, "values": { "title": title } }),
            )
            .await;
        if r.get("error").is_none() {
            return r["result"]["row"]["id"].as_str().unwrap().to_string();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("cannot insert a jot");
}

fn new_doc() -> Doc {
    Doc::with_options(Options {
        offset_kind: OffsetKind::Utf16,
        ..Default::default()
    })
}

/// An editor as far as sync goes: a Yjs replica that talks y-protocols over a document stream.
struct Peer {
    ws: Ws,
    doc: Doc,
    stream: u32,
    seen: usize,
    buf: Vec<u8>,
    synced: bool,
    denied: bool,
    awareness: Vec<Value>,
    read_only: bool,
    /// Updates ahead of what this replica has, as a Yjs client keeps them pending.
    held: Vec<Vec<u8>>,
}

impl Peer {
    async fn open(mut ws: Ws, id: &str) -> Peer {
        let r = ws.call("documents.open", json!({ "id": id })).await;
        let mut p = Peer {
            stream: r["stream"].as_u64().unwrap() as u32,
            read_only: r["readOnly"].as_bool().unwrap(),
            seen: ws.frames.len(),
            ws,
            doc: new_doc(),
            buf: Vec::new(),
            synced: false,
            denied: false,
            awareness: Vec::new(),
            held: Vec::new(),
        };
        p.hello().await;
        p
    }

    /// Opens the document again on the same socket, keeping the replica (a reconnect).
    async fn reopen(&mut self, id: &str) {
        let r = self.ws.call("documents.open", json!({ "id": id })).await;
        self.stream = r["stream"].as_u64().unwrap() as u32;
        self.buf.clear();
        self.synced = false;
        self.hello().await;
    }

    async fn hello(&mut self) {
        let sv = self.doc.transact().state_vector();
        self.send(Message::Sync(SyncMessage::SyncStep1(sv))).await;
    }

    async fn close(&mut self) {
        self.ws
            .notify("streams.close", json!({ "stream": self.stream }))
            .await;
    }

    fn frag(&self) -> XmlFragmentRef {
        self.doc.get_or_insert_xml_fragment("prosemirror")
    }

    async fn send(&mut self, msg: Message) {
        let bytes = frame(&msg.encode_v1());
        self.ws.send_binary(encode_frame(self.stream, &bytes)).await;
    }

    async fn send_update(&mut self, u: Vec<u8>) {
        self.send(Message::Sync(SyncMessage::Update(u))).await;
    }

    /// Makes a local change and sends it.
    async fn change(&mut self, f: impl FnOnce(&mut yrs::TransactionMut, &XmlFragmentRef)) {
        let u = self.change_offline(f);
        self.send_update(u).await;
    }

    /// Makes a change and waits until the kernel has it (an update arrives on its own socket, so a call
    /// on another one could overtake it).
    async fn change_and_settle(
        &mut self,
        ann: &mut Ws,
        id: &str,
        f: impl FnOnce(&mut yrs::TransactionMut, &XmlFragmentRef),
    ) {
        self.change(f).await;
        let mine = self.json();
        for _ in 0..100 {
            if read(ann, id, "json").await["content"] == mine {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the kernel did not get the change");
    }

    /// Makes a local change without sending it; returns the update.
    fn change_offline(
        &mut self,
        f: impl FnOnce(&mut yrs::TransactionMut, &XmlFragmentRef),
    ) -> Vec<u8> {
        let frag = self.frag();
        let sv = self.doc.transact().state_vector();
        {
            let mut txn = self.doc.transact_mut();
            f(&mut txn, &frag);
        }
        self.doc.transact().encode_state_as_update_v1(&sv)
    }

    /// Handles what has arrived on the stream, for up to `limit`.
    async fn pump(&mut self, limit: Duration) {
        let before = self.ws.frames.len();
        self.ws
            .pump_until(limit, |w| w.frames.len() > before + 1000)
            .await;
        self.drain().await;
    }

    async fn drain(&mut self) {
        let stream = self.stream;
        let frames: Vec<Vec<u8>> = self.ws.frames[self.seen..].to_vec();
        self.seen = self.ws.frames.len();
        let mut granted = 0u32;
        for f in frames {
            let Some((id, payload)) = decode_frame(&f) else {
                continue;
            };
            if id != stream {
                continue;
            }
            granted += payload.len() as u32;
            self.buf.extend_from_slice(payload);
        }
        if granted > 0 {
            self.ws
                .notify(
                    "streams.credit",
                    json!({ "stream": stream, "bytes": granted }),
                )
                .await;
        }
        while let Some(msg) = take_frame(&mut self.buf).unwrap() {
            let mut dec = DecoderV1::new(Cursor::new(&msg));
            match Message::decode(&mut dec).unwrap() {
                Message::Sync(SyncMessage::SyncStep1(sv)) => {
                    let diff = self.doc.transact().encode_state_as_update_v1(&sv);
                    self.send(Message::Sync(SyncMessage::SyncStep2(diff))).await;
                }
                Message::Sync(SyncMessage::SyncStep2(u)) => {
                    self.apply(&u);
                    self.synced = true;
                }
                Message::Sync(SyncMessage::Update(u)) => self.apply(&u),
                Message::Awareness(u) => {
                    for e in u.clients.values() {
                        self.awareness.push(serde_json::from_str(&e.json).unwrap());
                    }
                }
                Message::Auth(Some(_)) => self.denied = true,
                _ => {}
            }
        }
    }

    /// Applies an update as a Yjs client does: structs that follow ones it does not have yet wait
    /// until those come (yrs would integrate them at once beside a placeholder, see
    /// `live::ahead_of`), then everything that can go in goes in.
    fn apply(&mut self, u: &[u8]) {
        self.held.push(u.to_vec());
        loop {
            let sv = self.doc.transact().state_vector();
            let Some(i) = self.held.iter().position(|h| {
                !rookery_documents::live::ahead_of(&Update::decode_v1(h).unwrap(), &sv)
            }) else {
                return;
            };
            let h = self.held.remove(i);
            self.doc
                .transact_mut()
                .apply_update(Update::decode_v1(&h).unwrap())
                .unwrap();
        }
    }

    // The fragment first: getting it takes a write transaction, which waits for any read one.
    fn text(&self) -> String {
        let frag = self.frag();
        let txn = self.doc.transact();
        frag.get_string(&txn)
    }

    fn json(&self) -> Value {
        let frag = self.frag();
        let txn = self.doc.transact();
        rookery_documents::schema::to_json(&txn, &frag)
    }
}

fn paragraph(txn: &mut yrs::TransactionMut, frag: &XmlFragmentRef, at: u32, text: &str) {
    let p = frag.insert(txn, at, XmlElementPrelim::empty("paragraph"));
    p.push_back(txn, XmlTextPrelim::new(text));
}

async fn read(ws: &mut Ws, id: &str, format: &str) -> Value {
    ws.call("documents.read", json!({ "id": id, "format": format }))
        .await
}

async fn text_of(ws: &mut Ws, id: &str) -> String {
    read(ws, id, "text").await["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn sync_persistence_compaction_and_catch_up() {
    let Some(k) = start().await else { return };
    live::set_tuning(
        &k.kernel,
        Tuning {
            compact_after: 5,
            idle: Duration::from_millis(300),
            ..Tuning::default()
        },
    );
    let mut ann = Ws::connect(&k, &sign_in(&k, "ann").await).await;
    let mut bo = Ws::connect(&k, &sign_in(&k, "bo").await).await;
    let bo_id = user_id(&mut bo).await;
    let space = team(&mut ann, &[(&bo_id, "use")]).await;
    let note = jot(&mut ann, &space, "Plan").await;
    let doc = ann
        .call(
            "documents.create",
            json!({ "parent": format!("jot://{note}"), "markdown": "# Plan\n\nFirst line" }),
        )
        .await;
    let id = doc["id"].as_str().unwrap().to_string();
    assert_eq!(doc["uri"], format!("document://{id}"));
    assert_eq!(doc["source"], format!("jot://{note}"));
    assert_eq!(doc["spaceId"], space);
    assert_eq!(doc["title"], "Plan");
    assert_eq!(doc["level"], "manage");

    // Two editors get the content and see each other's typing.
    let mut a = Peer::open(Ws::connect(&k, &sign_in(&k, "ann").await).await, &id).await;
    let mut b = Peer::open(Ws::connect(&k, &sign_in(&k, "bo").await).await, &id).await;
    assert!(!b.read_only);
    a.pump(Duration::from_millis(300)).await;
    b.pump(Duration::from_millis(300)).await;
    assert!(a.synced && b.synced);
    assert_eq!(a.json()["content"][1]["content"][0]["text"], "First line");
    a.change(|txn, f| paragraph(txn, f, 2, "from Ann")).await;
    b.pump(Duration::from_millis(300)).await;
    assert_eq!(b.json(), a.json());

    // Bo goes offline and keeps typing; Ann types too. On reconnect both edits survive.
    b.close().await;
    for i in 0..6 {
        let s = format!(" a{i}");
        a.change(move |txn, f| {
            let Some(XmlOut::Element(p)) = f.get(txn, 2) else {
                panic!()
            };
            let Some(XmlOut::Text(t)) = p.get(txn, 0) else {
                panic!()
            };
            let n = t.len(txn);
            t.insert(txn, n, &s);
        })
        .await;
    }
    b.change_offline(|txn, f| paragraph(txn, f, 0, "Bo offline"));
    a.pump(Duration::from_millis(200)).await;
    assert!(!a.text().contains("Bo offline"));
    b.reopen(&id).await;
    b.pump(Duration::from_millis(400)).await;
    a.pump(Duration::from_millis(300)).await;
    assert_eq!(
        a.json(),
        b.json(),
        "the replicas converge after the reconnect"
    );
    assert!(a.text().contains("Bo offline") && b.text().contains("a5"));

    // The kernel stored it: the updates were compacted along the way, and a read after the replica
    // stopped loads the same content.
    ann.call("documents.flush", json!({ "id": id })).await;
    let pending =
        k.db.store
            .documents()
            .pending_updates(id.parse().unwrap())
            .await
            .unwrap();
    assert_eq!(pending, 0, "a flush compacts");
    let via_kernel = read(&mut ann, &id, "json").await["content"].clone();
    assert_eq!(via_kernel, a.json());
    a.close().await;
    b.close().await;
    let uuid: uuid::Uuid = id.parse().unwrap();
    for _ in 0..50 {
        if !live::is_live(&k.kernel, uuid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!live::is_live(&k.kernel, uuid), "an unused replica stops");
    assert_eq!(read(&mut ann, &id, "json").await["content"], via_kernel);
    let md = read(&mut ann, &id, "markdown").await["markdown"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(md.starts_with("Bo offline\n\n# Plan\n"), "{md}");

    // A new editor joining late catches up from the snapshot.
    let mut c = Peer::open(Ws::connect(&k, &sign_in(&k, "bo").await).await, &id).await;
    c.pump(Duration::from_millis(400)).await;
    assert_eq!(c.json(), via_kernel);
    k.close().await;
}

#[tokio::test]
async fn permissions_follow_the_parent() {
    let Some(k) = start().await else { return };
    let mut ann = Ws::connect(&k, &sign_in(&k, "ann").await).await;
    let mut vi = Ws::connect(&k, &sign_in(&k, "vi").await).await;
    let mut cy = Ws::connect(&k, &sign_in(&k, "cy").await).await;
    let vi_id = user_id(&mut vi).await;
    let space = team(&mut ann, &[(&vi_id, "view")]).await;
    let note = jot(&mut ann, &space, "Spec").await;
    let id = ann
        .call(
            "documents.create",
            json!({ "parent": format!("jot://{note}"), "markdown": "Hello" }),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Someone outside the space sees nothing, and cannot attach documents to the note.
    for m in [
        "documents.get",
        "documents.open",
        "documents.read",
        "documents.history",
    ] {
        assert_eq!(
            code(&cy.call_raw(m, json!({ "id": id })).await),
            "NOT_FOUND",
            "{m}"
        );
    }
    let r = cy
        .call_raw(
            "documents.create",
            json!({ "parent": format!("jot://{note}") }),
        )
        .await;
    assert_eq!(code(&r), "FORBIDDEN");

    // A viewer reads, follows the document read-only, and cannot change it.
    assert_eq!(
        vi.call("documents.get", json!({ "id": id })).await["level"],
        "view"
    );
    assert_eq!(text_of(&mut vi, &id).await, "Hello");
    let r = vi
        .call_raw(
            "documents.edit",
            json!({ "id": id, "ops": [{ "op": "appendMarkdown", "markdown": "no" }] }),
        )
        .await;
    assert_eq!(code(&r), "FORBIDDEN");
    let r = vi.call_raw("documents.delete", json!({ "id": id })).await;
    assert_eq!(code(&r), "FORBIDDEN");
    let mut v = Peer::open(Ws::connect(&k, &sign_in(&k, "vi").await).await, &id).await;
    assert!(v.read_only);
    v.pump(Duration::from_millis(300)).await;
    v.change(|txn, f| paragraph(txn, f, 0, "sneaky")).await;
    v.pump(Duration::from_millis(300)).await;
    assert!(v.denied, "the kernel says the update was refused");
    assert_eq!(text_of(&mut ann, &id).await, "Hello");

    // The owner deletes it; it is gone for everyone.
    ann.call("documents.delete", json!({ "id": id })).await;
    assert_eq!(
        code(&vi.call_raw("documents.get", json!({ "id": id })).await),
        "NOT_FOUND"
    );
    k.close().await;
}

#[tokio::test]
async fn content_reaches_the_indexes() {
    let Some(k) = start().await else { return };
    let mut ann = Ws::connect(&k, &sign_in(&k, "ann").await).await;
    let mut bo = Ws::connect(&k, &sign_in(&k, "bo").await).await;
    let bo_id = user_id(&mut bo).await;
    let space = team(&mut ann, &[(&bo_id, "use")]).await;
    let target = jot(&mut ann, &space, "Target").await;
    let host = jot(&mut ann, &space, "Host").await;
    let id = ann
        .call(
            "documents.create",
            json!({ "parent": format!("jot://{host}"), "title": "Draft" }),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let uri = format!("document://{id}");

    // Ann types a heading, a mention of the target and of Bo, and a plugin block, as an editor would.
    let mut a = Peer::open(Ws::connect(&k, &sign_in(&k, "ann").await).await, &id).await;
    a.pump(Duration::from_millis(200)).await;
    let (target_uri, bo_uri) = (format!("jot://{target}"), format!("user://{bo_id}"));
    a.change_and_settle(&mut ann, &id, move |txn, f| {
        let h = f.insert(txn, 0, XmlElementPrelim::empty("heading"));
        h.insert_attribute(txn, "level", yrs::Any::from(1));
        h.push_back(txn, XmlTextPrelim::new("Quarterly zebra plan"));
        let p = f.insert(txn, 1, XmlElementPrelim::empty("paragraph"));
        p.push_back(txn, XmlTextPrelim::new("See "));
        for (u, l) in [(&target_uri, "Target"), (&bo_uri, "Bo")] {
            let m = p.push_back(txn, XmlElementPrelim::empty("mention"));
            m.insert_attribute(txn, "uri", yrs::Any::from(u.as_str()));
            m.insert_attribute(txn, "label", yrs::Any::from(l));
        }
        let b = f.insert(txn, 2, XmlElementPrelim::empty("plugin_block"));
        b.insert_attribute(txn, "type", yrs::Any::from("jot.counter"));
        let attrs: yrs::Any =
            serde_json::from_value(json!({ "label": "Visits", "count": 7 })).unwrap();
        b.insert_attribute(txn, "attrs", attrs);
    })
    .await;
    ann.call("documents.flush", json!({ "id": id })).await;

    // The title follows the first block. Search finds the text, and the plugin block's rendering, as the note
    // the document is attached to (the document itself is not a separate hit).
    assert_eq!(
        ann.call("documents.get", json!({ "id": id })).await["title"],
        "Quarterly zebra plan"
    );
    let hits = bo.call("entities.search", json!({ "text": "zebra" })).await;
    let uris: Vec<&str> = hits["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, vec![format!("jot://{host}")], "{hits}");
    let hits = bo
        .call("entities.search", json!({ "text": "Visits" }))
        .await;
    assert_eq!(hits["entities"][0]["uri"], format!("jot://{host}"));
    let found = bo
        .call("search.query", json!({ "text": "zebra", "facets": true }))
        .await;
    assert_eq!(found["hits"].as_array().unwrap().len(), 1, "{found}");
    assert_eq!(found["hits"][0]["entity"]["uri"], format!("jot://{host}"));
    assert_eq!(found["facets"], json!([{ "scheme": "jot", "count": 1 }]));
    // A document without a parent is found as itself.
    let lone = ann
        .call(
            "documents.create",
            json!({ "markdown": "Aardvark inventory" }),
        )
        .await;
    let hits = ann
        .call("entities.search", json!({ "text": "aardvark" }))
        .await;
    assert_eq!(hits["entities"][0]["uri"], lone["uri"]);
    let _ = &uri;
    assert!(text_of(&mut ann, &id).await.contains("Visits: 7"));

    // The mention is a backlink from the note the document is attached to, as for a richtext field.
    let links = bo
        .call("data.refsTo", json!({ "uri": format!("jot://{target}") }))
        .await;
    assert_eq!(links["backlinks"][0]["source"], format!("jot://{host}"));
    assert_eq!(links["backlinks"][0]["title"], "Host");
    assert_eq!(links["backlinks"][0]["field"], format!("document:{id}"));

    // Bo got one mention notification; editing again does not repeat it.
    let n = bo.call("notify.list", json!({})).await;
    let mentions: Vec<&Value> = n["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "rookery/mention")
        .collect();
    assert_eq!(mentions.len(), 1, "{n}");
    assert_eq!(mentions[0]["title"], "ann mentioned you in Host");
    assert_eq!(mentions[0]["data"]["entity"], format!("jot://{host}"));
    a.change_and_settle(&mut ann, &id, |txn, f| paragraph(txn, f, 3, "more"))
        .await;
    ann.call("documents.flush", json!({ "id": id })).await;
    let n = bo.call("notify.list", json!({})).await;
    let count = n["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "rookery/mention")
        .count();
    assert_eq!(count, 1);

    // Removing the mention removes the backlink.
    a.change_and_settle(&mut ann, &id, |txn, f| f.remove_range(txn, 1, 1))
        .await;
    ann.call("documents.flush", json!({ "id": id })).await;
    let links = bo
        .call("data.refsTo", json!({ "uri": format!("jot://{target}") }))
        .await;
    assert_eq!(links["backlinks"], json!([]));
    k.close().await;
}

#[tokio::test]
async fn history_restore_and_server_side_edits() {
    let Some(k) = start().await else { return };
    let mut ann = Ws::connect(&k, &sign_in(&k, "ann").await).await;
    let id = ann
        .call(
            "documents.create",
            json!({ "content": { "type": "doc", "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": "Version one" }] }
            ]}}),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut a = Peer::open(Ws::connect(&k, &sign_in(&k, "ann").await).await, &id).await;
    a.pump(Duration::from_millis(200)).await;

    let v1 = ann
        .call(
            "documents.saveVersion",
            json!({ "id": id, "name": "First draft" }),
        )
        .await;
    assert_eq!(v1["kind"], "named");

    // A server-side edit, as an agent would make it: the editor sees the change and the author.
    let r = ann
        .call(
            "documents.edit",
            json!({ "id": id, "author": { "name": "Planner" }, "ops": [
                { "op": "replaceText", "block": 0, "from": 8, "to": 11, "text": "two" },
                { "op": "appendMarkdown", "markdown": "- [ ] ship it\n- [x] write it" },
                { "op": "insert", "at": 0, "content": { "type": "heading", "attrs": { "level": 2 }, "content": [{ "type": "text", "text": "Notes" }] } }
            ]}),
        )
        .await;
    assert_eq!(r["blocks"], 3);
    a.pump(Duration::from_millis(300)).await;
    let j = a.json();
    assert_eq!(j["content"][0]["type"], "heading");
    assert_eq!(j["content"][1]["content"][0]["text"], "Version two");
    assert_eq!(j["content"][2]["type"], "task_list");
    assert!(
        a.awareness.iter().any(|s| s["user"]["name"] == "Planner"),
        "{:?}",
        a.awareness
    );
    let md = read(&mut ann, &id, "markdown").await["markdown"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        md,
        "## Notes\n\nVersion two\n\n- [ ] ship it\n- [x] write it\n"
    );
    let r = ann
        .call_raw(
            "documents.edit",
            json!({ "id": id, "ops": [{ "op": "replaceText", "block": 2, "from": 0, "to": 1, "text": "x" }] }),
        )
        .await;
    assert_eq!(code(&r), "INVALID", "a list is not a text block");

    // An old version can be read, and restoring it reaches the editor as an edit.
    let v = ann
        .call(
            "documents.read",
            json!({ "id": id, "version": v1["id"], "format": "text" }),
        )
        .await;
    assert_eq!(v["text"], "Version one");
    let before = ann
        .call(
            "documents.restore",
            json!({ "id": id, "version": v1["id"] }),
        )
        .await;
    assert_eq!(before["kind"], "restore");
    a.pump(Duration::from_millis(300)).await;
    assert_eq!(a.text(), "<paragraph>Version one</paragraph>");
    let h = ann.call("documents.history", json!({ "id": id })).await;
    let kinds: Vec<&str> = h["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"restore") && kinds.contains(&"named"),
        "{kinds:?}"
    );
    // The state before the restore is kept.
    let r = ann
        .call(
            "documents.read",
            json!({ "id": id, "version": before["id"], "format": "text" }),
        )
        .await;
    assert!(r["text"].as_str().unwrap().contains("ship it"));
    k.close().await;
}

#[tokio::test]
async fn periodic_versions_are_saved() {
    let Some(k) = start().await else { return };
    live::set_tuning(
        &k.kernel,
        Tuning {
            index_delay: Duration::from_millis(50),
            version_every: Duration::from_millis(1),
            ..Tuning::default()
        },
    );
    let mut ann = Ws::connect(&k, &sign_in(&k, "ann").await).await;
    let id = ann.call("documents.create", json!({})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut a = Peer::open(Ws::connect(&k, &sign_in(&k, "ann").await).await, &id).await;
    a.pump(Duration::from_millis(100)).await;
    a.change(|txn, f| paragraph(txn, f, 0, "one")).await;
    a.pump(Duration::from_millis(300)).await;
    a.change(|txn, f| paragraph(txn, f, 1, "two")).await;
    a.pump(Duration::from_millis(300)).await;
    let h = ann.call("documents.history", json!({ "id": id })).await;
    let auto = h["versions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "auto")
        .count();
    assert!(auto >= 2, "{h}");
    k.close().await;
}

/// A small deterministic generator, so a failing seed can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

/// One random edit: type into a block, delete some of it, add a block or remove one.
fn random_edit(rng: &mut Rng, txn: &mut yrs::TransactionMut, f: &XmlFragmentRef, who: usize) {
    let blocks = f.len(txn);
    let pick = rng.below(10);
    if blocks == 0 || pick == 0 {
        let at = rng.below(blocks as u64 + 1) as u32;
        paragraph(txn, f, at, &format!("p{who}"));
        return;
    }
    let b = rng.below(blocks as u64) as u32;
    if pick == 1 && blocks > 1 {
        f.remove_range(txn, b, 1);
        return;
    }
    let Some(XmlOut::Element(p)) = f.get(txn, b) else {
        return;
    };
    let t = match p.get(txn, 0) {
        Some(XmlOut::Text(t)) => t,
        _ => p.insert(txn, 0, XmlTextPrelim::new("")),
    };
    let len = t.len(txn);
    if pick <= 3 && len > 0 {
        let at = rng.below(len as u64) as u32;
        let n = (rng.below(4) as u32 + 1).min(len - at);
        t.remove_range(txn, at, n);
    } else {
        let at = rng.below(len as u64 + 1) as u32;
        let s: String = (0..rng.below(4) + 1)
            .map(|_| (b'a' + rng.below(26) as u8) as char)
            .collect();
        t.insert(txn, at, &format!("{s}{who}"));
    }
}

/// A client's edit that reaches the kernel before an earlier edit of the same client (it typed after a
/// reconnect before its offline edits went out with its sync step 2) waits for that one, as a Yjs
/// client keeps it pending: yrs 0.28 would integrate it at once with a placeholder for the gap and count
/// the gap as had, so the earlier edit was never asked for and the replicas diverged.
#[tokio::test]
async fn an_edit_ahead_of_its_predecessor_waits_for_it() {
    let Some(k) = start().await else { return };
    let mut ann = Ws::connect(&k, &sign_in(&k, "ann").await).await;
    let id = ann
        .call("documents.create", json!({ "markdown": "start" }))
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut a = Peer::open(Ws::connect(&k, &sign_in(&k, "ann").await).await, &id).await;
    let mut b = Peer::open(Ws::connect(&k, &sign_in(&k, "ann").await).await, &id).await;
    // Both have the document as it starts before one edits it offline.
    for p in [&mut a, &mut b] {
        for _ in 0..100 {
            p.pump(Duration::from_millis(50)).await;
            if p.json().to_string().contains("start") {
                break;
            }
        }
    }
    // "second" goes after "start", so nothing in it refers to "first": only its clocks come after.
    let first = a.change_offline(|txn, f| paragraph(txn, f, 0, "first"));
    let second = a.change_offline(|txn, f| paragraph(txn, f, 2, "second"));
    a.send_update(second).await;
    b.pump(Duration::from_millis(300)).await;
    // The kernel does not show "second" before "first": it waits, as everyone else's replica does.
    assert!(!text_of(&mut ann, &id).await.contains("second"));
    a.send_update(first).await;
    let mut done = false;
    for _ in 0..50 {
        b.pump(Duration::from_millis(100)).await;
        let text = text_of(&mut ann, &id).await;
        if text.contains("first") && text.contains("second") && b.json() == a.json() {
            done = true;
            break;
        }
    }
    assert!(done, "the kernel and both peers have both edits");
    let server = read(&mut ann, &id, "json").await["content"].clone();
    assert_eq!(a.json(), server);
    // A peer that joins now gets both, and the stored state loads back the same.
    let mut c = Peer::open(Ws::connect(&k, &sign_in(&k, "ann").await).await, &id).await;
    c.pump(Duration::from_millis(300)).await;
    assert_eq!(c.json(), server);
    ann.call("documents.flush", json!({ "id": id })).await;
    let stored =
        k.db.store
            .documents()
            .load(id.parse().unwrap())
            .await
            .unwrap();
    let mut parts: Vec<&[u8]> = stored.snapshot.iter().map(|s| s.as_slice()).collect();
    parts.extend(stored.updates.iter().map(|(_, u)| u.as_slice()));
    let d = rookery_documents::live::rebuild(&parts).unwrap();
    let frag = d.get_or_insert_xml_fragment("prosemirror");
    assert_eq!(
        rookery_documents::schema::to_json(&d.transact(), &frag),
        server
    );
    k.close().await;
}

#[tokio::test]
async fn concurrent_random_edits_converge() {
    let Some(k) = start().await else { return };
    live::set_tuning(
        &k.kernel,
        Tuning {
            compact_after: 17,
            ..Tuning::default()
        },
    );
    let mut ann = Ws::connect(&k, &sign_in(&k, "ann").await).await;
    for seed in [1u64, 7, 42, 1234] {
        let id = ann
            .call("documents.create", json!({ "markdown": "start" }))
            .await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut peers = Vec::new();
        for _ in 0..3 {
            let ws = Ws::connect(&k, &sign_in(&k, "ann").await).await;
            let mut p = Peer::open(ws, &id).await;
            p.pump(Duration::from_millis(100)).await;
            peers.push(p);
        }
        let mut rng = Rng(seed * 2654435761 + 1);
        for _round in 0..40 {
            // Each peer edits a few times without hearing from the others first.
            for (who, p) in peers.iter_mut().enumerate() {
                for _ in 0..rng.below(3) + 1 {
                    let r = rng.next();
                    let u = p.change_offline(|txn, f| random_edit(&mut Rng(r | 1), txn, f, who));
                    if rng.below(8) == 0 {
                        // Sometimes an update is held back and arrives late (a slow link).
                        let delay = u;
                        p.drain().await;
                        p.send_update(delay).await;
                    } else {
                        p.send_update(u).await;
                    }
                }
            }
            if rng.below(3) == 0 {
                for p in peers.iter_mut() {
                    p.pump(Duration::from_millis(5)).await;
                }
            }
            // Now and then a peer drops off and comes back with its offline edits.
            if rng.below(10) == 0 {
                let who = rng.below(3) as usize;
                let p = &mut peers[who];
                p.close().await;
                for _ in 0..3 {
                    let r = rng.next();
                    p.change_offline(|txn, f| random_edit(&mut Rng(r | 1), txn, f, who));
                }
                p.reopen(&id).await;
            }
        }
        // Until everyone has everything: on a loaded machine the kernel can be seconds behind the peers.
        let until = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut server;
        loop {
            for p in peers.iter_mut() {
                p.pump(Duration::from_millis(100)).await;
            }
            server = read(&mut ann, &id, "json").await["content"].clone();
            if peers.iter().all(|p| p.json() == server) || tokio::time::Instant::now() > until {
                break;
            }
        }
        for (i, p) in peers.iter().enumerate() {
            assert_eq!(
                p.json(),
                server,
                "seed {seed}: peer {i} differs from the kernel"
            );
        }
        // And what was stored loads back the same.
        ann.call("documents.flush", json!({ "id": id })).await;
        let stored =
            k.db.store
                .documents()
                .load(id.parse().unwrap())
                .await
                .unwrap();
        let d = new_doc();
        {
            let mut txn = d.transact_mut();
            txn.apply_update(Update::decode_v1(stored.snapshot.as_ref().unwrap()).unwrap())
                .unwrap();
            for (_, u) in &stored.updates {
                txn.apply_update(Update::decode_v1(u).unwrap()).unwrap();
            }
        }
        let frag = d.get_or_insert_xml_fragment("prosemirror");
        let loaded = rookery_documents::schema::to_json(&d.transact(), &frag);
        assert_eq!(loaded, server, "seed {seed}: the stored state differs");
        for p in peers.iter_mut() {
            p.close().await;
        }
        let _ = StateVector::default();
    }
    k.close().await;
}

#[tokio::test]
async fn attachments_require_the_collections_read_and_write_levels() {
    let Some(k) = start().await else { return };
    let mut owner = Ws::connect(&k, &k.person("Ann", true).await).await;
    let mut member = Ws::connect(&k, &sign_in(&k, "Vi").await).await;
    let member_id = user_id(&mut member).await;
    let space = team(&mut owner, &[(&member_id, "use")]).await;
    let parent = format!("jot://{}", jot(&mut owner, &space, "Spec").await);

    let apply = |access: Value| {
        let store = k.db.store.clone();
        async move {
            let defs: Vec<rookery_store::collections::CollectionDef> = serde_json::from_value(json!([{
                "name": "jots", "entity": { "scheme": "jot", "title": "title" }, "trash": true,
                "access": access, "fields": [{ "name": "title", "type": "text", "search": true }]
            }]))
            .unwrap();
            store.collections().apply("jot", &defs).await.unwrap();
        }
    };
    apply(json!({"read": "view", "write": "manage"})).await;
    let doc = owner
        .call(
            "documents.create",
            json!({ "parent": parent, "markdown": "Spec" }),
        )
        .await;
    let id = doc["id"].as_str().unwrap();
    let opened = member.call("documents.open", json!({ "id": id })).await;
    assert_eq!(opened["readOnly"], true);
    for (method, params) in [
        ("documents.create", json!({ "parent": parent })),
        (
            "documents.edit",
            json!({ "id": id, "ops": [{ "op": "appendMarkdown", "markdown": "Change" }] }),
        ),
        ("documents.delete", json!({ "id": id })),
    ] {
        assert_eq!(
            code(&member.call_raw(method, params).await),
            "FORBIDDEN",
            "{method}"
        );
    }

    for access in [json!({"read": "manage"}), json!({"admin": true})] {
        apply(access).await;
        for method in ["documents.get", "documents.read", "documents.open"] {
            assert_eq!(
                code(&member.call_raw(method, json!({"id": id})).await),
                "NOT_FOUND",
                "{method}"
            );
        }
        assert_eq!(
            member
                .call("documents.list", json!({ "parent": parent }))
                .await["documents"],
            json!([])
        );
        assert_eq!(text_of(&mut owner, id).await, "Spec");
    }
    k.close().await;
}
