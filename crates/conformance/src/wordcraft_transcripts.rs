//! The WordCraft schema 1 transcripts (`conformance/wordcraft-1-*.jsonl`), written with
//! [`crate::wordcraft`]. The expected documents after concurrent edits come from replaying the
//! same edits on a simulated app, so they follow Yjs's ordering (a lower client id's text comes
//! first where two people typed at one place).

use std::collections::BTreeMap;

use craft_collab_kit::b64;
use serde_json::{Value as Json, json};
use yrs::IndexedSequence;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Assoc, StateVector, Transact, Update};

use crate::awareness::{self, Entry};
use crate::crypto;
use crate::refapp::RefApp;
use crate::transcript::{HIGH_PEER, PEER, Peer, Script, peer_user};
use crate::wordcraft::{self, Block, Model, Para, Run, WordCraft};

fn minutes() -> Model {
    Model {
        title: "Minutes".into(),
        body: vec![
            Block::Para(Para::text("Minutes").styled("Heading1")),
            Block::Para(Para::of(vec![Run::with("Present: ", &[("bold", json!(true))]), Run::plain("Ada, Bo")])),
            Block::Table(vec![vec![vec![Block::Para(Para::text("Item"))], vec![Block::Para(Para::text("Owner"))]]]),
            Block::Para(Para::text("Next meeting on Monday.")),
        ],
    }
}

fn peer_with(model: &Model) -> Peer {
    let mut p = Peer::new(PEER, 21);
    p.edit(|t, _| wordcraft::seed(t, model));
    p
}

fn model_of(p: &Peer) -> Model {
    wordcraft::read(&p.doc.transact()).unwrap_or_default()
}

fn w(name: &str, description: &str, room: &str, key: Option<[u8; 32]>) -> Script {
    Script::for_app(wordcraft::APP, name, description, room, key)
}

fn update_out(s: &mut Script, model: &Model, optional: bool) {
    let room = s.room.clone();
    s.out_decodes_json(json!({"jsonrpc": "2.0", "method": "collab/update", "params": {"room": room, "update": "$b64"}}), model.to_json(), optional);
}

/// An awareness update: the peer's caret or selection in paragraph `path`, bytes `a..b`.
fn selection(p: &Peer, path: &[usize], a: u32, b: u32) -> Vec<u8> {
    let t = p.doc.transact();
    let sticky = |i: u32| -> String {
        wordcraft::para_el(&t, path)
            .and_then(|el| wordcraft::text_of(&t, &el))
            .and_then(|x| x.sticky_index(&t, i, Assoc::After))
            .map(|s| b64::encode(&s.encode_v1()))
            .unwrap_or_default()
    };
    let state = json!({"user": peer_user(), "selection": {"story": "body", "anchor": sticky(a), "focus": sticky(b)}});
    awareness::encode(&[Entry { client: PEER, clock: 1, state }])
}

/// Where a simulated app starts: a document it opened, or the room's state it received.
enum Start<'a> {
    Model(&'a Model),
    State(&'a [u8]),
}

/// What an app starting from `start` sees after running `commands`, then applying `remote` (a
/// peer's updates): the reference app stands in for it.
fn simulate(start: Start, commands: &[(&str, Json)], remote: &[&[u8]]) -> Model {
    let mut app: RefApp<WordCraft> = RefApp::new(Some(77));
    match start {
        Start::Model(m) => app.load(m),
        Start::State(u) => {
            app.doc = yrs::Doc::with_client_id(app.client_id);
            if let Ok(u) = Update::decode_v1(u) {
                let _ = app.doc.transact_mut().apply_update(u);
            }
        }
    }
    for (c, p) in commands {
        let _ = app.run(c, p);
    }
    {
        let mut t = app.doc.transact_mut();
        for u in remote {
            if let Ok(u) = Update::decode_v1(u) {
                let _ = t.apply_update(u);
            }
        }
    }
    app.model().unwrap_or_default()
}

fn join_transcript() -> Script {
    let room = "conformance:wjoin";
    let mut s = w(
        "wordcraft-1-join",
        "Joining a room: the first state, remote typing, formatting, a paragraph split, an edit in a table cell, awareness with relative positions, answering sync1, a local edit, leaving.",
        room,
        None,
    );
    s.join("join");
    s.note("Until the first state arrives the document is empty and read-only.");
    s.model_json(Model::default().to_json());
    let mut p = peer_with(&minutes());
    let full = p.full();
    let u = s.wire("collab/sync2", &full);
    s.input("collab/sync2", json!({"room": room, "update": u, "from": PEER}));
    s.model_json(model_of(&p).to_json());
    s.note("The peer types at the end of the second paragraph: the new text takes no formatting.");
    let u = p.edit(|t, _| {
        let _ = wordcraft::insert_text(t, &[1], 16, " and Cy", &BTreeMap::new());
    });
    s.update(PEER, &u);
    s.model_json(model_of(&p).to_json());
    s.note("The peer makes \"Monday\" bold (an attribute on that range).");
    let u = p.edit(|t, _| {
        let _ = wordcraft::format_text(t, &[3], 16, 22, &[("bold", json!(true))]);
    });
    s.update(PEER, &u);
    s.model_json(model_of(&p).to_json());
    s.note("The peer presses Enter after \"Present: \": the rest of the paragraph moves to a new one.");
    let u = p.edit(|t, _| {
        let _ = wordcraft::split_para(t, &[1], 9);
    });
    s.update(PEER, &u);
    s.model_json(model_of(&p).to_json());
    s.note("The peer types in the table's second cell.");
    let u = p.edit(|t, _| {
        let _ = wordcraft::insert_text(t, &[3, 0, 1, 0], 5, "s", &BTreeMap::new());
    });
    s.update(PEER, &u);
    let m = model_of(&p);
    s.model_json(m.to_json());
    s.note("The peer's selection: \"Ada\" in the third paragraph, as relative positions.");
    let aw = selection(&p, &[2], 0, 3);
    let u = s.wire("collab/awareness", &aw);
    s.input("collab/awareness", json!({"room": room, "update": u, "from": PEER}));
    s.model_json(m.to_json());
    s.note("A message for a room the app isn't in is ignored.");
    let mut other = Peer::new(7, 12);
    let u = other.edit(|t, _| wordcraft::seed(t, &Model::of("Other", vec![Para::text("nope")])));
    s.lines.push(json!({"t": "in", "valid": false, "msg": {"jsonrpc": "2.0", "method": "collab/update", "params": {"room": "conformance:other", "update": b64::encode(&u), "from": 7}}}));
    s.model_json(m.to_json());
    s.note("A newcomer (2002) asks with an empty state vector; the app answers with everything, addressed to it.");
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 2002}));
    s.out_decodes_json(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": 2002}}), m.to_json(), false);
    s.note("A local edit: \"meeting\" replaced by typing \"call\".");
    s.run("select.text", json!({"text": "meeting"}));
    s.run("text.insert", json!({"text": "call"}));
    let state = p.full();
    let local = [("select.text", json!({"text": "meeting"})), ("text.insert", json!({"text": "call"}))];
    let after = simulate(Start::State(&state), &local, &[]);
    update_out(&mut s, &after, false);
    s.model_json(after.to_json());
    s.input("collab/leave", json!({"room": room, "reason": "The room was closed."}));
    s.note("After leaving, the document stays open with every edit, and edits aren't sent.");
    s.model_json(after.to_json());
    s.run("select.text", json!({"text": "Item"}));
    s.run("text.insert", json!({"text": "Topic"}));
    s.lines.push(json!({"t": "quiet"}));
    let last = simulate(
        Start::State(&state),
        &[local[0].clone(), local[1].clone(), ("select.text", json!({"text": "Item"})), ("text.insert", json!({"text": "Topic"}))],
        &[],
    );
    s.model_json(last.to_json());
    s
}

fn share_transcript() -> Script {
    let room = "conformance:wshare";
    let mut s = w(
        "wordcraft-1-share",
        "Sharing the open document: seeding, answering collab/snapshot and sync1, local edits as one update each (bold, typing over a selection, Enter, typing), and a concurrent remote edit (the title and a paragraph) merging in.",
        room,
        None,
    );
    s.note("Edits before joining a room aren't sent anywhere.");
    let before = [("text.insert", json!({"text": "Agenda"})), ("text.newParagraph", json!({})), ("text.insert", json!({"text": "Budget review"}))];
    for (c, p) in &before {
        s.run(c, p.clone());
    }
    s.lines.push(json!({"t": "quiet"}));
    let blank = Model::of("", vec![Para::default()]);
    let base = simulate(Start::Model(&blank), &before, &[]);
    s.model_json(base.to_json());
    s.join("share");
    s.request(1, "collab/snapshot", json!({"room": room}));
    s.out_decodes_json(json!({"jsonrpc": "2.0", "id": 1, "result": {"update": "$b64"}}), base.to_json(), false);
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 4004}));
    s.out_decodes_json(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": 4004}}), base.to_json(), false);
    let mut commands: Vec<(&str, Json)> = Vec::new();
    for step in [
        vec![("select.text", json!({"text": "Budget"})), ("format.bold", json!({"value": true}))],
        vec![("select.text", json!({"text": "review"})), ("text.insert", json!({"text": "plan"}))],
        vec![("text.newParagraph", json!({}))],
        vec![("text.insert", json!({"text": "Close"}))],
    ] {
        for (c, p) in &step {
            s.run(c, p.clone());
            commands.push((c, p.clone()));
        }
        let all: Vec<(&str, Json)> = before.iter().cloned().chain(commands.iter().cloned()).collect();
        let m = simulate(Start::Model(&blank), &all, &[]);
        update_out(&mut s, &m, false);
        s.model_json(m.to_json());
    }
    s.note("A peer that started from nothing sets the title and adds a paragraph, concurrently: both merge (its client id is higher, so its title wins and its paragraph comes last).");
    let mut p = Peer::new(HIGH_PEER, 13);
    let u = p.edit(|t, _| {
        wordcraft::set_title(t, "Team agenda");
        wordcraft::push_block(t, &Block::Para(Para::text("From the peer")));
    });
    s.update(HIGH_PEER, &u);
    let all: Vec<(&str, Json)> = before.iter().cloned().chain(commands.iter().cloned()).collect();
    let m = simulate(Start::Model(&blank), &all, &[&u]);
    s.model_json(m.to_json());
    s.request(2, "collab/snapshot", json!({"room": room}));
    s.out_decodes_json(json!({"jsonrpc": "2.0", "id": 2, "result": {"update": "$b64"}}), m.to_json(), false);
    s.input("collab/leave", json!({"room": room}));
    s.model_json(m.to_json());
    s
}

fn encrypted_transcript() -> Script {
    let room = "conformance:wsecret";
    let key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(11).wrapping_add(5));
    let mut s = w(
        "wordcraft-1-encrypted",
        "An end-to-end encrypted room: the stored snapshot (method collab/snapshot), updates, messages that must be dropped (wrong method, tampered, another room, wrong key, garbage), answering collab/snapshot and sync1 with ciphertext, \"Wrong room key?\" after 10 failures.",
        room,
        Some(key),
    );
    s.join("join");
    let mut p = peer_with(&minutes());
    s.note("The stored snapshot arrives as sync2 with snapshot: true and is decrypted with the method name collab/snapshot.");
    let full = p.full();
    let u = s.wire("collab/snapshot", &full);
    s.input("collab/sync2", json!({"room": room, "update": u, "snapshot": true}));
    s.model_json(model_of(&p).to_json());
    let u = p.edit(|t, _| {
        let _ = wordcraft::insert_text(t, &[1], 16, " (secret)", &BTreeMap::new());
    });
    s.update(PEER, &u);
    let good = model_of(&p);
    s.model_json(good.to_json());
    let mut stray = peer_with(&minutes());
    let stray_update = stray.edit(|t, _| {
        let _ = wordcraft::insert_text(t, &[0], 0, "must not appear ", &BTreeMap::new());
    });
    s.note("Ciphertext made for collab/awareness, sent as collab/update: dropped.");
    let n = s.nonce();
    let wrong_method = crypto::seal(&key, room, "collab/awareness", &stray_update, n).unwrap_or_default();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&wrong_method), "from": PEER}));
    s.model_json(good.to_json());
    s.note("Tampered (one bit flipped): dropped.");
    let n = s.nonce();
    let mut tampered = crypto::seal(&key, room, "collab/update", &stray_update, n).unwrap_or_default();
    if let Some(b) = tampered.get_mut(20) {
        *b ^= 1;
    }
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&tampered), "from": PEER}));
    s.model_json(good.to_json());
    s.note("Encrypted for another room: dropped.");
    let n = s.nonce();
    let other_room = crypto::seal(&key, "conformance:elsewhere", "collab/update", &stray_update, n).unwrap_or_default();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&other_room), "from": PEER}));
    s.model_json(good.to_json());
    s.note("Encrypted with another key: dropped.");
    let n = s.nonce();
    let other_key = crypto::seal(&[1u8; 32], room, "collab/update", &stray_update, n).unwrap_or_default();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&other_key), "from": PEER}));
    s.model_json(good.to_json());
    let aw = selection(&p, &[1], 9, 9);
    let u = s.wire("collab/awareness", &aw);
    s.input("collab/awareness", json!({"room": room, "update": u, "from": PEER}));
    s.model_json(good.to_json());
    s.note("The transport asks for the room's state to store: the answer is ciphertext for collab/snapshot.");
    s.request(1, "collab/snapshot", json!({"room": room}));
    s.out_decodes_json(json!({"jsonrpc": "2.0", "id": 1, "result": {"update": "$b64"}}), good.to_json(), false);
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 3003}));
    s.out_decodes_json(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": 3003}}), good.to_json(), false);
    let local = [("select.text", json!({"text": "Monday"})), ("text.insert", json!({"text": "Friday"}))];
    for (c, p) in &local {
        s.run(c, p.clone());
    }
    let state = p.full();
    let after = simulate(Start::State(&state), &local, &[]);
    update_out(&mut s, &after, false);
    s.note("Six more messages that don't decrypt (ten in all): the app shows \"Wrong room key?\".");
    for i in 0..6u8 {
        let junk: Vec<u8> = (0..40).map(|j| j ^ i).collect();
        s.invalid("collab/update", json!({"room": room, "update": b64::encode(&junk), "from": PEER}));
    }
    s.model_json(after.to_json());
    s.lines.push(json!({"t": "notice", "contains": "Wrong room key?", "lang": "en"}));
    s
}

fn robustness_transcript() -> Script {
    let room = "conformance:wrobust";
    let mut s = w(
        "wordcraft-1-robustness",
        "Bad input never crashes the app or changes the document: missing fields, bad base64, bytes that aren't a Yjs update, a truncated update, an unknown status, another app's document (\"This room is for gridcraft 1\"), an unknown method, bad awareness.",
        room,
        None,
    );
    s.join("join");
    let mut p = peer_with(&minutes());
    let full = p.full();
    s.input("collab/sync2", json!({"room": room, "update": b64::encode(&full), "from": PEER}));
    let good = model_of(&p);
    s.model_json(good.to_json());
    s.invalid("collab/update", json!({"room": room, "from": PEER}));
    s.invalid("collab/update", json!({"update": "AAA=", "from": PEER}));
    s.invalid("collab/update", json!({"room": room, "update": "not base64!!", "from": PEER}));
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&[0xff; 16]), "from": PEER}));
    let u = peer_with(&minutes()).edit(|t, _| {
        let _ = wordcraft::insert_text(t, &[0], 7, " (truncated)", &BTreeMap::new());
    });
    let cut = u.get(..u.len() / 2).unwrap_or_default().to_vec();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&cut), "from": PEER}));
    s.input("collab/sync1", json!({"room": room, "stateVector": "%%%", "from": PEER}));
    s.lines.push(json!({"t": "quiet"}));
    s.input("collab/status", json!({"room": room, "state": "dancing"}));
    s.model_json(good.to_json());
    s.note("A peer of another app (GridCraft) writes its meta: the app refuses the update and says so.");
    let mut g = Peer::new(HIGH_PEER - 1, 14);
    let u = g.edit(|t, _| {
        use yrs::{Map, WriteTxn};
        let meta = t.get_or_insert_map("meta");
        meta.insert(t, "app", "gridcraft");
        meta.insert(t, "schema", 1);
    });
    s.invalid("collab/sync2", json!({"room": room, "update": b64::encode(&u), "from": HIGH_PEER - 1}));
    s.model_json(good.to_json());
    s.lines.push(json!({"t": "notice", "contains": "This room is for gridcraft 1", "lang": "en"}));
    let u = p.edit(|t, _| {
        let _ = wordcraft::insert_text(t, &[3], 0, "Still fine. ", &BTreeMap::new());
    });
    s.update(PEER, &u);
    s.model_json(model_of(&p).to_json());
    s.input("collab/unknown", json!({"room": room}));
    s.input("collab/awareness", json!({"room": room, "update": "AQ==", "from": PEER}));
    s.model_json(model_of(&p).to_json());
    s
}

fn offline_transcript() -> Script {
    let room = "conformance:woffline";
    let mut s = w(
        "wordcraft-1-offline",
        "Going offline and back, with both people typing in one paragraph meanwhile (one of them at the same place): the edits merge character by character after the app's new sync1, the peer's text first where both typed at one place (its client id is lower).",
        room,
        None,
    );
    s.join("join");
    let start = Model::of("Notes", vec![Para::text("The fox jumps."), Para::text("Second line")]);
    let mut p = peer_with(&start);
    let full = p.full();
    s.input("collab/sync2", json!({"room": room, "update": b64::encode(&full), "from": PEER}));
    s.model_json(start.to_json());
    s.input("collab/status", json!({"room": room, "state": "offline", "peers": [], "message": "Connection lost"}));
    s.note("Editing goes on offline; the app may send the updates now (the transport may queue them) or not.");
    let caret = |off: usize| json!({"pos": {"story": "body", "path": [0], "off": off}});
    let local = [
        ("caret.set", caret(4)),
        ("text.insert", json!({"text": "quick "})),
        ("text.insert", json!({"text": "red "})),
        ("caret.set", caret(23)),
        ("text.insert", json!({"text": " high"})),
    ];
    let mut done: Vec<(&str, Json)> = Vec::new();
    for (c, prm) in &local {
        s.run(c, prm.clone());
        done.push((c, prm.clone()));
        if *c == "text.insert" {
            let m = simulate(Start::State(&full), &done, &[]);
            update_out(&mut s, &m, true);
            s.model_json(m.to_json());
        }
    }
    s.note("The peer typed too, in the same paragraph: \"brown \" where the app typed \"quick \", and \" over the dog\" before the full stop; its update arrives once the app is back.");
    let u = p.edit(|t, _| {
        let _ = wordcraft::insert_text(t, &[0], 13, " over the dog", &BTreeMap::new());
        let _ = wordcraft::insert_text(t, &[0], 4, "brown ", &BTreeMap::new());
    });
    s.input("collab/status", json!({"room": room, "state": "online", "peers": [{"clientId": PEER, "user": peer_user()}]}));
    s.out("collab/sync1", json!({"room": room, "stateVector": "$b64"}));
    s.update(PEER, &u);
    let both = simulate(Start::State(&full), &done, &[&u]);
    s.model_json(both.to_json());
    s.note("The peer asks with its state vector: the app's answer holds its offline edits.");
    let sv = p.sv();
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&sv), "from": PEER}));
    s.out_decodes_json(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": PEER}}), both.to_json(), false);
    s
}

/// Every WordCraft transcript.
pub(crate) fn all() -> Vec<Script> {
    vec![join_transcript(), share_transcript(), encrypted_transcript(), robustness_transcript(), offline_transcript()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_typing_merges_by_client_id() {
        let start = Model::of("", vec![Para::text("The fox jumps.")]);
        let mut p = peer_with(&start);
        let full = p.full();
        let u = p.edit(|t, _| {
            let _ = wordcraft::insert_text(t, &[0], 4, "brown ", &BTreeMap::new());
        });
        let caret = json!({"pos": {"story": "body", "path": [0], "off": 4}});
        let m = simulate(Start::State(&full), &[("caret.set", caret), ("text.insert", json!({"text": "quick "}))], &[&u]);
        let text = match m.body.first() {
            Some(Block::Para(p)) => p.plain_text(),
            _ => String::new(),
        };
        assert_eq!(text, "The brown quick fox jumps.");
    }
}
