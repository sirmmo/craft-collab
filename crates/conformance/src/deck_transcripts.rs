//! The `deckcraft-1-*` transcripts (format in `docs/conformance.md`), written with `yrs` and
//! DeckCraft schema 1 ([`crate::deckcraft`]). The app under test runs DeckCraft's own commands
//! (`run` lines), so DeckCraft replays them with its engine.

use craft_collab_kit::b64;
use craft_collab_kit::collab::put;
use serde_json::{Value as Json, json};
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact, TransactionMut, WriteTxn};

use crate::awareness::{self, Entry};
use crate::crypto;
use crate::deckcraft::{self, Model, ShapeModel, SlideModel};
use crate::gridcraft::Ids;
use crate::transcript::PLUGIN;

const PEER: u64 = 1001;
/// A client id above any app's, so the peer's concurrent writes to the same map key win.
const HIGH_PEER: u64 = (1 << 53) - 2;

fn app_user() -> Json {
    json!({"id": "u-app", "name": "App Under Test", "color": "#3B7DD8"})
}

fn peer_user() -> Json {
    json!({"id": "u-peer", "name": "Peer", "color": "#D8513B"})
}

/// Another person's document, with a fixed client id.
struct Peer {
    doc: Doc,
    ids: Ids,
}

impl Peer {
    fn new(client: u64, seed: u64) -> Peer {
        Peer { doc: deckcraft::new_doc(client), ids: Ids::seeded(seed) }
    }

    fn edit(&mut self, f: impl FnOnce(&mut TransactionMut, &mut Ids)) -> Vec<u8> {
        let mut t = self.doc.transact_mut();
        f(&mut t, &mut self.ids);
        t.encode_update_v1()
    }

    fn full(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }

    fn sv(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    fn model(&self) -> Model {
        deckcraft::read(&self.doc.transact()).unwrap_or_default()
    }

    fn slide(&self, i: usize) -> String {
        deckcraft::slide_ids(&self.doc.transact()).get(i).cloned().unwrap_or_default()
    }

    fn shape(&self, slide: usize, i: usize) -> String {
        deckcraft::shape_ids(&self.doc.transact(), &self.slide(slide)).get(i).cloned().unwrap_or_default()
    }
}

/// An id as the app's commands take it: the number.
fn num(id: &str) -> u64 {
    u64::from(deckcraft::parse_id(id).unwrap_or(0))
}

/// Writes one transcript.
struct Script {
    lines: Vec<Json>,
    room: String,
    key: Option<[u8; 32]>,
    nonces: Ids,
}

impl Script {
    fn new(name: &str, description: &str, room: &str, key: Option<[u8; 32]>) -> Script {
        let mut s = Script { lines: Vec::new(), room: room.into(), key, nonces: Ids::seeded(0xDECC) };
        s.lines.push(json!({"t": "meta", "name": name, "app": "deckcraft", "schema": 1, "format": 1, "description": description}));
        s
    }

    fn nonce(&mut self) -> [u8; 12] {
        let mut n = [0u8; 12];
        let (a, b) = (self.nonces.bits().to_le_bytes(), self.nonces.bits().to_le_bytes());
        for (i, x) in n.iter_mut().enumerate() {
            *x = if i < 8 { a.get(i).copied().unwrap_or(0) } else { b.get(i - 8).copied().unwrap_or(0) };
        }
        n
    }

    fn wire(&mut self, method: &str, plain: &[u8]) -> String {
        match self.key {
            Some(k) => {
                let n = self.nonce();
                b64::encode(&crypto::seal(&k, &self.room, method, plain, n).unwrap_or_default())
            }
            None => b64::encode(plain),
        }
    }

    fn note(&mut self, text: &str) {
        self.lines.push(json!({"t": "note", "text": text}));
    }

    fn run(&mut self, command: &str, params: Json) {
        self.lines.push(json!({"t": "run", "command": command, "params": params}));
    }

    fn input(&mut self, method: &str, params: Json) {
        self.lines.push(json!({"t": "in", "msg": {"jsonrpc": "2.0", "method": method, "params": params}}));
    }

    fn invalid(&mut self, method: &str, params: Json) {
        self.lines.push(json!({"t": "in", "valid": false, "msg": {"jsonrpc": "2.0", "method": method, "params": params}}));
    }

    fn request(&mut self, id: u64, method: &str, params: Json) {
        self.lines.push(json!({"t": "in", "msg": {"jsonrpc": "2.0", "id": id, "method": method, "params": params}}));
    }

    fn update(&mut self, from: u64, plain: &[u8]) {
        let u = self.wire("collab/update", plain);
        let room = self.room.clone();
        self.input("collab/update", json!({"room": room, "update": u, "from": from}));
    }

    fn sync2(&mut self, from: u64, plain: &[u8]) {
        let u = self.wire("collab/sync2", plain);
        let room = self.room.clone();
        self.input("collab/sync2", json!({"room": room, "update": u, "from": from}));
    }

    fn out(&mut self, method: &str, params: Json) {
        self.lines.push(json!({"t": "out", "msg": {"jsonrpc": "2.0", "method": method, "params": params}}));
    }

    fn out_update(&mut self, model: &Model, optional: bool) {
        let room = self.room.clone();
        let mut l =
            json!({"t": "out", "msg": {"jsonrpc": "2.0", "method": "collab/update", "params": {"room": room, "update": "$b64"}}, "decodesTo": model.to_json()});
        if optional {
            put(&mut l, "optional", json!(true));
        }
        self.lines.push(l);
    }

    fn out_sync2(&mut self, to: u64, model: &Model) {
        let room = self.room.clone();
        self.lines.push(json!({"t": "out", "msg": {"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": to}}, "decodesTo": model.to_json()}));
    }

    fn out_snapshot(&mut self, id: u64, model: &Model) {
        self.lines.push(json!({"t": "out", "msg": {"jsonrpc": "2.0", "id": id, "result": {"update": "$b64"}}, "decodesTo": model.to_json()}));
    }

    fn model(&mut self, m: &Model) {
        self.lines.push(json!({"t": "model", "model": m.to_json()}));
    }

    fn quiet(&mut self) {
        self.lines.push(json!({"t": "quiet"}));
    }

    fn notice(&mut self, text: &str) {
        self.lines.push(json!({"t": "notice", "contains": text, "lang": "en"}));
    }

    fn status(&mut self, state: &str, peers: Json) {
        let room = self.room.clone();
        self.input("collab/status", json!({"room": room, "state": state, "peers": peers}));
    }

    fn join(&mut self, mode: &str) {
        let mut params = json!({"plugin": PLUGIN, "room": self.room, "mode": mode, "user": app_user(), "role": "edit"});
        if let Some(k) = &self.key {
            put(&mut params, "key", json!(b64::encode(k)));
        }
        self.run("collab.join", params);
        let room = self.room.clone();
        self.out(
            "collab/joined",
            json!({"room": room, "clientId": "$clientId", "user": app_user(), "app": "deckcraft", "schema": 1, "encrypted": self.key.is_some()}),
        );
        self.out("collab/sync1", json!({"room": room, "stateVector": "$b64"}));
        self.status("connecting", json!([]));
        self.status("online", json!([{"clientId": PEER, "user": peer_user()}]));
        self.note("Every time the transport reports online after another state, the app sends sync1.");
        self.out("collab/sync1", json!({"room": room, "stateVector": "$b64"}));
    }

    fn text(&self) -> String {
        self.lines.iter().map(|l| format!("{l}\n")).collect()
    }
}

fn talk() -> Model {
    Model {
        title: "Quarterly review".into(),
        slides: vec![
            SlideModel {
                shapes: vec![ShapeModel::new([60.0, 40.0, 840.0, 80.0], "Quarterly review"), ShapeModel::new([60.0, 160.0, 840.0, 300.0], "Revenue\nCosts")],
                notes: "Welcome everyone".into(),
            },
            SlideModel {
                shapes: vec![ShapeModel::new([60.0, 40.0, 400.0, 80.0], "Plan"), ShapeModel::new([500.0, 40.0, 400.0, 80.0], "Risks")],
                notes: String::new(),
            },
        ],
    }
}

fn peer_with_talk() -> Peer {
    let mut p = Peer::new(PEER, 21);
    p.edit(|t, ids| deckcraft::seed(t, &talk(), ids));
    p
}

fn shape_mut(m: &mut Model, slide: usize, shape: usize) -> Option<&mut ShapeModel> {
    m.slides.get_mut(slide)?.shapes.get_mut(shape)
}

fn join_transcript() -> Script {
    let room = "conformance:deck-join";
    let mut s = Script::new(
        "deckcraft-1-join",
        "Joining a room: the first state, remote edits (typing in a shape, moving one, a slide added and one deleted, notes), awareness, another room's message ignored, answering a newcomer's sync1, local edits (typing, moving a shape), collab/leave, editing after leaving.",
        room,
        None,
    );
    s.join("join");
    s.note("Until the first state arrives the presentation is read-only; it shows nothing of the room yet.");
    let mut p = peer_with_talk();
    let full = p.full();
    s.sync2(PEER, &full);
    s.model(&p.model());
    let (s0, title, body) = (p.slide(0), p.shape(0, 0), p.shape(0, 1));
    s.note("The peer types in the body (after \"Revenue\") and moves the title.");
    let u = p.edit(|t, _| deckcraft::type_text(t, &s0, Some(&body), (0, 7), " up 12%"));
    s.update(PEER, &u);
    s.model(&p.model());
    let u = p.edit(|t, _| deckcraft::set_rect(t, &s0, &title, [80.0, 30.0, 800.0, 90.0]));
    s.update(PEER, &u);
    s.model(&p.model());
    s.note("The peer adds a slide at the end with a shape, deletes the second slide and edits the first slide's notes.");
    let u = p.edit(|t, ids| {
        let id = deckcraft::add_slide(t, 2, ids);
        deckcraft::add_shape(t, &id, [100.0, 100.0, 300.0, 60.0], Some("Questions?"), ids);
    });
    s.update(PEER, &u);
    s.model(&p.model());
    let s1 = p.slide(1);
    let u = p.edit(|t, _| {
        deckcraft::delete_slide(t, &s1);
        deckcraft::set_notes(t, &s0, "Welcome everyone\nThank the team");
    });
    s.update(PEER, &u);
    s.model(&p.model());
    let aw = awareness::encode(&[Entry { client: PEER, clock: 1, state: json!({"user": peer_user(), "selection": {"slide": s0, "shapes": [title]}}) }]);
    let u = s.wire("collab/awareness", &aw);
    s.input("collab/awareness", json!({"room": room, "update": u, "from": PEER}));
    s.model(&p.model());
    s.note("A message for a room the app isn't in is ignored.");
    let mut other = Peer::new(7, 12);
    let u = other.edit(|t, ids| deckcraft::seed(t, &Model { title: "Other".into(), slides: vec![Default::default()] }, ids));
    s.lines.push(json!({"t": "in", "valid": false, "msg": {"jsonrpc": "2.0", "method": "collab/update", "params": {"room": "conformance:other", "update": b64::encode(&u), "from": 7}}}));
    s.model(&p.model());
    s.note("A newcomer (2002) asks with an empty state vector; the app answers with everything, addressed to it.");
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 2002}));
    s.out_sync2(2002, &p.model());
    s.note("The app types in the body, at the end of its second paragraph: one update; then moves the body.");
    s.run("text.edit", json!({"id": num(&body), "at": [1, 5]}));
    s.run("text.insert", json!({"text": " down 3%"}));
    let mut after = p.model();
    if let Some(sh) = shape_mut(&mut after, 0, 1) {
        sh.text = Some("Revenue up 12%\nCosts down 3%".into());
    }
    s.out_update(&after, false);
    s.model(&after);
    s.run("shape.setBounds", json!({"ids": [num(&body)], "x": 60, "y": 180, "w": 840, "h": 280}));
    if let Some(sh) = shape_mut(&mut after, 0, 1) {
        sh.rect = Some([60.0, 180.0, 840.0, 280.0]);
    }
    s.out_update(&after, false);
    s.model(&after);
    s.input("collab/leave", json!({"room": room, "reason": "The room was closed."}));
    s.note("After leaving, the presentation stays open with every edit, and edits aren't sent.");
    s.model(&after);
    s.run("shape.setBounds", json!({"ids": [num(&body)], "x": 70, "y": 180, "w": 840, "h": 280}));
    s.quiet();
    if let Some(sh) = shape_mut(&mut after, 0, 1) {
        sh.rect = Some([70.0, 180.0, 840.0, 280.0]);
    }
    s.model(&after);
    s
}

fn share_transcript() -> Script {
    let room = "conformance:deck-share";
    let mut s = Script::new(
        "deckcraft-1-share",
        "Sharing the open presentation: seeding, answering collab/snapshot and sync1, local edits as one update each (typing, notes, a new slide), and a concurrent remote edit (the title, a new slide with a shape) merging in.",
        room,
        None,
    );
    s.note("A known starting point: a blank slide replaces the new presentation's title slide, and gets a rectangle. Edits before joining a room aren't sent anywhere.");
    s.run("slide.new", json!({"layout": "blank", "at": 0}));
    s.run("slide.delete", json!({"index": 1}));
    s.run("shape.insert", json!({"preset": "rect", "rect": [100, 100, 400, 120], "text": "Hello"}));
    s.quiet();
    let base =
        Model { title: String::new(), slides: vec![SlideModel { shapes: vec![ShapeModel::new([100.0, 100.0, 400.0, 120.0], "Hello")], notes: String::new() }] };
    s.model(&base);
    s.join("share");
    s.request(1, "collab/snapshot", json!({"room": room}));
    s.out_snapshot(1, &base);
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 4004}));
    s.out_sync2(4004, &base);
    let mut m = base.clone();
    s.note("Typing in the rectangle (it's still selected): one update.");
    s.run("text.edit", json!({"at": [0, 5]}));
    s.run("text.insert", json!({"text": " world"}));
    if let Some(sh) = shape_mut(&mut m, 0, 0) {
        sh.text = Some("Hello world".into());
    }
    s.out_update(&m, false);
    s.model(&m);
    s.run("slide.notes", json!({"text": "Speak slowly", "index": 0}));
    if let Some(sl) = m.slides.get_mut(0) {
        sl.notes = "Speak slowly".into();
    }
    s.out_update(&m, false);
    s.model(&m);
    s.run("slide.new", json!({"layout": "blank", "at": 1}));
    m.slides.push(SlideModel::default());
    s.out_update(&m, false);
    s.model(&m);
    s.note("A peer that started from nothing sets the title and adds a slide with a shape, concurrently: both merge (its client id is higher, so its title wins, and its slide, inserted into a list it saw empty, goes after the app's).");
    let mut p = Peer::new(HIGH_PEER, 23);
    let u = p.edit(|t, ids| {
        let meta = t.get_or_insert_map("meta");
        meta.insert(t, "title", "Shared talk");
        let id = deckcraft::add_slide(t, 0, ids);
        deckcraft::add_shape(t, &id, [10.0, 10.0, 200.0, 50.0], Some("From the peer"), ids);
    });
    s.update(HIGH_PEER, &u);
    m.title = "Shared talk".into();
    m.slides.push(SlideModel { shapes: vec![ShapeModel::new([10.0, 10.0, 200.0, 50.0], "From the peer")], notes: String::new() });
    s.model(&m);
    s.request(2, "collab/snapshot", json!({"room": room}));
    s.out_snapshot(2, &m);
    s.input("collab/leave", json!({"room": room}));
    s.model(&m);
    s
}

fn encrypted_transcript() -> Script {
    let room = "conformance:deck-secret";
    let key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(11).wrapping_add(5));
    let mut s = Script::new(
        "deckcraft-1-encrypted",
        "An end-to-end encrypted room: the stored snapshot (method collab/snapshot), updates, messages that must be dropped (wrong method, tampered, another room, wrong key, garbage), answering collab/snapshot and sync1 with ciphertext, \"Wrong room key?\" after 10 failures.",
        room,
        Some(key),
    );
    s.join("join");
    let mut p = peer_with_talk();
    s.note("The stored snapshot arrives as sync2 with snapshot: true and is decrypted with the method name collab/snapshot.");
    let full = p.full();
    let u = s.wire("collab/snapshot", &full);
    s.input("collab/sync2", json!({"room": room, "update": u, "snapshot": true}));
    s.model(&p.model());
    let (s0, title) = (p.slide(0), p.shape(0, 0));
    let u = p.edit(|t, _| deckcraft::type_text(t, &s0, Some(&title), (0, 16), " 2026"));
    s.update(PEER, &u);
    s.model(&p.model());
    let good = p.model();
    let mut stray = peer_with_talk();
    let stray_update = stray.edit(|t, _| deckcraft::type_text(t, &s0, Some(&title), (0, 0), "must not appear "));
    s.note("Ciphertext made for collab/awareness, sent as collab/update: dropped.");
    let n = s.nonce();
    let wrong_method = crypto::seal(&key, room, "collab/awareness", &stray_update, n).unwrap_or_default();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&wrong_method), "from": PEER}));
    s.model(&good);
    s.note("Tampered (one bit flipped): dropped.");
    let n = s.nonce();
    let mut tampered = crypto::seal(&key, room, "collab/update", &stray_update, n).unwrap_or_default();
    if let Some(b) = tampered.get_mut(20) {
        *b ^= 1;
    }
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&tampered), "from": PEER}));
    s.model(&good);
    s.note("Encrypted for another room: dropped.");
    let n = s.nonce();
    let other_room = crypto::seal(&key, "conformance:elsewhere", "collab/update", &stray_update, n).unwrap_or_default();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&other_room), "from": PEER}));
    s.model(&good);
    s.note("Encrypted with another key: dropped.");
    let n = s.nonce();
    let other_key = crypto::seal(&[1u8; 32], room, "collab/update", &stray_update, n).unwrap_or_default();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&other_key), "from": PEER}));
    s.model(&good);
    let aw = awareness::encode(&[Entry { client: PEER, clock: 1, state: json!({"user": peer_user(), "selection": {"slide": s0, "shapes": []}}) }]);
    let u = s.wire("collab/awareness", &aw);
    s.input("collab/awareness", json!({"room": room, "update": u, "from": PEER}));
    s.model(&good);
    s.note("The transport asks for the room's state to store: the answer is ciphertext for collab/snapshot.");
    s.request(1, "collab/snapshot", json!({"room": room}));
    s.out_snapshot(1, &good);
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 3003}));
    s.out_sync2(3003, &good);
    s.run("shape.setBounds", json!({"ids": [num(&title)], "x": 50, "y": 50, "w": 860, "h": 70}));
    let mut after = good.clone();
    if let Some(sh) = shape_mut(&mut after, 0, 0) {
        sh.rect = Some([50.0, 50.0, 860.0, 70.0]);
    }
    s.out_update(&after, false);
    s.note("Six more messages that don't decrypt (ten in all): the app shows \"Wrong room key?\".");
    for i in 0..6u8 {
        let junk: Vec<u8> = (0..40).map(|j| j ^ i).collect();
        s.invalid("collab/update", json!({"room": room, "update": b64::encode(&junk), "from": PEER}));
    }
    s.model(&after);
    s.notice("Wrong room key?");
    s
}

fn robustness_transcript() -> Script {
    let room = "conformance:deck-robust";
    let mut s = Script::new(
        "deckcraft-1-robustness",
        "Bad input never crashes the app or changes the presentation: missing fields, bad base64, bytes that aren't a Yjs update, a truncated update, a bad state vector, an unknown status, another app's document (\"This room is for gridcraft 1\"), slide lists and properties that aren't what schema 1 says, an unknown method, bad awareness.",
        room,
        None,
    );
    s.join("join");
    let mut p = peer_with_talk();
    let full = p.full();
    s.sync2(PEER, &full);
    let good = p.model();
    s.model(&good);
    s.invalid("collab/update", json!({"room": room, "from": PEER}));
    s.invalid("collab/update", json!({"update": "AAA=", "from": PEER}));
    s.invalid("collab/update", json!({"room": room, "update": "not base64!!", "from": PEER}));
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&[0xff; 16]), "from": PEER}));
    let (s0, title) = (p.slide(0), p.shape(0, 0));
    let u = peer_with_talk().edit(|t, _| deckcraft::type_text(t, &s0, Some(&title), (0, 0), "truncated "));
    let cut = u.get(..u.len() / 2).unwrap_or_default().to_vec();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&cut), "from": PEER}));
    s.input("collab/sync1", json!({"room": room, "stateVector": "%%%", "from": PEER}));
    s.quiet();
    s.input("collab/status", json!({"room": room, "state": "dancing"}));
    s.model(&good);
    s.note("A peer of another app (GridCraft) writes its meta: the app refuses the update and says so.");
    let mut w = Peer::new(HIGH_PEER - 1, 24);
    let u = w.edit(|t, _| {
        let meta = t.get_or_insert_map("meta");
        meta.insert(t, "app", "gridcraft");
        meta.insert(t, "schema", 1);
    });
    s.invalid("collab/sync2", json!({"room": room, "update": b64::encode(&u), "from": HIGH_PEER - 1}));
    s.model(&good);
    s.notice("This room is for gridcraft 1");
    s.note("A peer writes things schema 1 doesn't allow: ids that aren't ids and a number in the slide list, a property that isn't JSON (the title's box: the shape keeps no box), a shape entry that isn't a map. Readers skip them.");
    let u = p.edit(|t, _| {
        let slides = t.get_or_insert_array("slides");
        yrs::Array::push_back(&slides, t, 5);
        yrs::Array::push_back(&slides, t, "NOT-AN-ID");
        yrs::Array::push_back(&slides, t, "zzzzzzzzz");
        let root = t.get_or_insert_map(format!("slide:{s0}"));
        if let Some(yrs::Out::YMap(m)) = root.get(t, &format!("shape:{title}")) {
            m.insert(t, "xfrm", "{not json");
        }
        let shapes = match root.get(t, "shapes") {
            Some(yrs::Out::YArray(a)) => Some(a),
            _ => None,
        };
        root.insert(t, "shape:01000000", 7);
        if let Some(a) = shapes {
            yrs::Array::push_back(&a, t, "01000000");
        }
    });
    s.update(PEER, &u);
    s.note("The title has no box now (the shape takes the layout's): the model says `rect: null`.");
    s.model(&p.model());
    let u = p.edit(|t, _| deckcraft::type_text(t, &s0, Some(&title), (0, 0), "Still "));
    s.update(PEER, &u);
    s.model(&p.model());
    s.input("collab/unknown", json!({"room": room}));
    s.input("collab/awareness", json!({"room": room, "update": "AQ==", "from": PEER}));
    s.model(&p.model());
    s
}

fn offline_transcript() -> Script {
    let room = "conformance:deck-offline";
    let mut s = Script::new(
        "deckcraft-1-offline",
        "Going offline and back: the app and a peer edit meanwhile (typing in the same shape's text at both ends, moving different shapes); everything merges after the app's new sync1, and the answer to the peer's sync1 holds the app's offline edits.",
        room,
        None,
    );
    s.join("join");
    let mut p = peer_with_talk();
    let full = p.full();
    s.sync2(PEER, &full);
    s.model(&p.model());
    let (s0, title, body) = (p.slide(0), p.shape(0, 0), p.shape(0, 1));
    s.input("collab/status", json!({"room": room, "state": "offline", "peers": [], "message": "Connection lost"}));
    s.note("Editing goes on offline; the app may send the updates now (the transport may queue them) or not.");
    let mut mine = p.model();
    s.run("text.edit", json!({"id": num(&body), "at": [1, 5]}));
    s.run("text.insert", json!({"text": " and more"}));
    if let Some(sh) = shape_mut(&mut mine, 0, 1) {
        sh.text = Some("Revenue\nCosts and more".into());
    }
    s.out_update(&mine, true);
    s.run("shape.setBounds", json!({"ids": [num(&title)], "x": 40, "y": 20, "w": 880, "h": 100}));
    if let Some(sh) = shape_mut(&mut mine, 0, 0) {
        sh.rect = Some([40.0, 20.0, 880.0, 100.0]);
    }
    s.out_update(&mine, true);
    s.note("The peer edited too while the app was away: typing at the start of the same text, and moving the other shape.");
    let u = p.edit(|t, _| {
        deckcraft::type_text(t, &s0, Some(&body), (0, 0), "Q3 ");
        deckcraft::set_rect(t, &s0, &body, [60.0, 150.0, 840.0, 320.0]);
    });
    s.input("collab/status", json!({"room": room, "state": "online", "peers": [{"clientId": PEER, "user": peer_user()}]}));
    s.out("collab/sync1", json!({"room": room, "stateVector": "$b64"}));
    s.update(PEER, &u);
    let mut both = p.model();
    if let Some(sh) = shape_mut(&mut both, 0, 1) {
        sh.text = Some("Q3 Revenue\nCosts and more".into());
    }
    if let Some(sh) = shape_mut(&mut both, 0, 0) {
        sh.rect = Some([40.0, 20.0, 880.0, 100.0]);
    }
    s.model(&both);
    s.note("The peer asks with its state vector: the app's answer holds its offline edits.");
    let sv = p.sv();
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&sv), "from": PEER}));
    s.out_sync2(PEER, &both);
    s
}

/// The DeckCraft transcripts: (file name, contents).
pub fn generate() -> Vec<(String, String)> {
    [join_transcript(), share_transcript(), encrypted_transcript(), robustness_transcript(), offline_transcript()]
        .into_iter()
        .map(|s| {
            let name = s.lines.first().and_then(|m| m.get("name")).and_then(Json::as_str).unwrap_or("transcript").to_string();
            (format!("{name}.jsonl"), s.text())
        })
        .collect()
}
