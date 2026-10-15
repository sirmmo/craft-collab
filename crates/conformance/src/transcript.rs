//! App-side transcripts (`conformance/*.jsonl`, format in `docs/conformance.md`): what a
//! transport sends an app, what the app must send back, and the document the app must have
//! after each step. [`generate`] writes them (deterministically, with `yrs`); [`replay`] runs
//! one against the reference app.

use std::collections::{HashMap, VecDeque};

use craft_collab_kit::b64;
use craft_collab_kit::collab::put;
use serde_json::{Value as Json, json};
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact, TransactionMut, WriteTxn};

use crate::awareness::{self, Entry};
use crate::crypto;
use crate::deckref::DeckRefApp;
use crate::gridcraft::{self, Ids, Model};
use crate::refapp::RefApp;

/// An app a transcript is replayed against: the reference apps here, or an app's own engine in
/// its tests (`run` → its commands, `receive` → its collab inbox, `model_json` → its document as
/// the transcript's schema describes it).
pub trait App {
    /// Runs one of the app's commands; the messages it sent.
    fn run(&mut self, command: &str, params: &Json) -> Result<Vec<Json>, String>;
    /// Handles one message from the transport; the messages it sent.
    fn receive(&mut self, msg: &Json) -> Vec<Json>;
    /// The document as a transcript model.
    fn model_json(&self) -> Result<Json, String>;
    /// What it showed in its status bar.
    fn notices(&self) -> Vec<String>;
}

impl App for RefApp {
    fn run(&mut self, command: &str, params: &Json) -> Result<Vec<Json>, String> {
        RefApp::run(self, command, params)
    }
    fn receive(&mut self, msg: &Json) -> Vec<Json> {
        RefApp::receive(self, msg)
    }
    fn model_json(&self) -> Result<Json, String> {
        self.model().map(|m| m.to_json())
    }
    fn notices(&self) -> Vec<String> {
        self.notices.clone()
    }
}

impl App for DeckRefApp {
    fn run(&mut self, command: &str, params: &Json) -> Result<Vec<Json>, String> {
        DeckRefApp::run(self, command, params)
    }
    fn receive(&mut self, msg: &Json) -> Vec<Json> {
        DeckRefApp::receive(self, msg)
    }
    fn model_json(&self) -> Result<Json, String> {
        self.model().map(|m| m.to_json())
    }
    fn notices(&self) -> Vec<String> {
        self.notices.clone()
    }
}

/// The app a transcript is for (its `meta` line), `gridcraft` when it doesn't say.
pub fn app_of(lines: &[Json]) -> String {
    lines.first().and_then(|m| m.get("app")).and_then(Json::as_str).unwrap_or("gridcraft").to_string()
}

/// The reference app for a transcript's app.
pub fn reference_app(lines: &[Json], seed: Option<u64>) -> Box<dyn App> {
    match app_of(lines).as_str() {
        "deckcraft" => Box::new(DeckRefApp::new(seed)),
        _ => Box::new(RefApp::new(seed)),
    }
}

/// A model in its canonical JSON form, for the transcript's app.
fn normalize(app: &str, j: &Json) -> Result<Json, String> {
    match app {
        "deckcraft" => crate::deckcraft::Model::from_json(j).map(|m| m.to_json()),
        _ => Model::from_json(j).map(|m| m.to_json()),
    }
}

/// What a peer that has every update in `updates` sees, as a transcript model.
fn peer_model(app: &str, updates: &[Vec<u8>]) -> Result<Json, String> {
    match app {
        "deckcraft" => DeckRefApp::model_of(updates).map(|m| m.to_json()),
        _ => RefApp::model_of(updates).map(|m| m.to_json()),
    }
}

/// The transport id the transcripts use in `collab.join`.
pub const PLUGIN: &str = "org.craft.conformance";

/// Messages an app may send between the expected ones.
const EXTRA: [&str; 3] = ["collab/awareness", "collab/sync1", "host/log"];

// ---------------------------------------------------------------------------------------------
// Matching

/// Whether `value` matches `pattern`: objects match when every key of the pattern matches
/// (`"$absent"`: the key must be missing), arrays element by element, `"$any"` anything present,
/// `"$b64"` a base64 string, `"$int"` an integer, `"$name"` binds a variable the first time and
/// must be equal afterwards (`$clientId`); anything else must be equal.
pub fn matches(pattern: &Json, value: Option<&Json>, vars: &mut HashMap<String, Json>) -> Result<(), String> {
    match pattern {
        Json::String(p) if p == "$absent" => match value {
            None => Ok(()),
            Some(v) => Err(format!("expected nothing, got {v}")),
        },
        Json::String(p) if p.starts_with('$') => {
            let v = value.ok_or_else(|| format!("missing ({p})"))?;
            match p.as_str() {
                "$any" => Ok(()),
                "$b64" => v.as_str().and_then(b64::decode).map(|_| ()).ok_or_else(|| format!("{v} isn't base64")),
                "$int" => v.as_u64().or_else(|| v.as_i64().map(|i| i as u64)).map(|_| ()).ok_or_else(|| format!("{v} isn't an integer")),
                name => match vars.get(name) {
                    Some(bound) if bound == v => Ok(()),
                    Some(bound) => Err(format!("{name} is {bound}, got {v}")),
                    None => {
                        vars.insert(name.to_string(), v.clone());
                        Ok(())
                    }
                },
            }
        }
        Json::Object(p) => {
            let v =
                value.and_then(Json::as_object).ok_or_else(|| format!("expected an object, got {}", value.map(Json::to_string).unwrap_or("nothing".into())))?;
            for (k, pv) in p {
                matches(pv, v.get(k), vars).map_err(|e| format!("{k}: {e}"))?;
            }
            Ok(())
        }
        Json::Array(p) => {
            let v = value.and_then(Json::as_array).ok_or("expected an array")?;
            if v.len() != p.len() {
                return Err(format!("expected {} items, got {}", p.len(), v.len()));
            }
            for (i, (pv, vv)) in p.iter().zip(v).enumerate() {
                matches(pv, Some(vv), vars).map_err(|e| format!("[{i}]: {e}"))?;
            }
            Ok(())
        }
        _ => match value {
            Some(v) if v == pattern => Ok(()),
            Some(v) => Err(format!("expected {pattern}, got {v}")),
            None => Err(format!("expected {pattern}, got nothing")),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Replaying

/// The method name a message's `update` was encrypted with.
fn update_method(msg: &Json) -> Option<&'static str> {
    if msg.get("result").is_some() {
        return Some("collab/snapshot");
    }
    let p = msg.get("params")?;
    match msg.get("method")?.as_str()? {
        "collab/update" => Some("collab/update"),
        "collab/sync2" if p.get("snapshot").and_then(Json::as_bool) == Some(true) => Some("collab/snapshot"),
        "collab/sync2" => Some("collab/sync2"),
        _ => None,
    }
}

/// The plaintext update a message carries.
fn update_of(msg: &Json, key: Option<&[u8; 32]>, room: &str) -> Option<Vec<u8>> {
    let method = update_method(msg)?;
    let text = msg.get("result").or_else(|| msg.get("params"))?.get("update")?.as_str()?;
    let data = b64::decode(text)?;
    match key {
        Some(k) => crypto::open(k, room, method, &data),
        None => Some(data),
    }
}

/// Replays a transcript against an app. `Err` names the failing line.
pub fn replay(lines: &[Json], app: &mut dyn App) -> Result<(), String> {
    let schema_app = app_of(lines);
    let mut vars = HashMap::new();
    let mut outbox: VecDeque<Json> = VecDeque::new();
    let mut key: Option<[u8; 32]> = None;
    let mut room = String::new();
    let mut peer: Vec<Vec<u8>> = Vec::new();
    let mut mine: Vec<Vec<u8>> = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let at = |e: String| format!("line {}: {e}", n + 1);
        let t = line.get("t").and_then(Json::as_str).unwrap_or("");
        let mut sent = Vec::new();
        match t {
            "meta" | "note" => {}
            "run" => {
                let command = line.get("command").and_then(Json::as_str).ok_or_else(|| at("run needs `command`".into()))?;
                let params = line.get("params").cloned().unwrap_or_else(|| json!({}));
                if command == "collab.join" {
                    room = params.get("room").and_then(Json::as_str).unwrap_or("").to_string();
                    key = params.get("key").and_then(Json::as_str).and_then(b64::decode).and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok());
                }
                sent = app.run(command, &params).map_err(|e| at(format!("{command} failed: {e}")))?;
            }
            "in" => {
                let msg = line.get("msg").ok_or_else(|| at("in needs `msg`".into()))?;
                if line.get("valid").and_then(Json::as_bool) != Some(false)
                    && let Some(u) = update_of(msg, key.as_ref(), &room)
                {
                    peer.push(u);
                }
                sent = app.receive(msg);
            }
            "out" => {
                let pattern = line.get("msg").ok_or_else(|| at("out needs `msg`".into()))?;
                let optional = line.get("optional").and_then(Json::as_bool) == Some(true);
                let mut found = false;
                while let Some(m) = outbox.pop_front() {
                    let mut trial = vars.clone();
                    if matches(pattern, Some(&m), &mut trial).is_ok() {
                        vars = trial;
                        found = true;
                        break;
                    }
                    let method = m.get("method").and_then(Json::as_str).unwrap_or("");
                    if !EXTRA.contains(&method) {
                        if optional {
                            outbox.push_front(m);
                            break;
                        }
                        let why = matches(pattern, Some(&m), &mut vars.clone()).err().unwrap_or_default();
                        return Err(at(format!("expected {pattern}, the app sent {m} ({why})")));
                    }
                }
                if !found && !optional {
                    return Err(at(format!("the app didn't send {pattern}")));
                }
                if found && let Some(expected) = line.get("decodesTo") {
                    let expected = normalize(&schema_app, expected).map_err(at)?;
                    let all: Vec<Vec<u8>> = peer.iter().chain(mine.iter()).cloned().collect();
                    let got = peer_model(&schema_app, &all).map_err(at)?;
                    if got != expected {
                        return Err(at(format!("a peer would see {got}, expected {expected}")));
                    }
                }
            }
            "quiet" => {
                if let Some(m) = outbox.iter().find(|m| !EXTRA.contains(&m.get("method").and_then(Json::as_str).unwrap_or(""))) {
                    return Err(at(format!("the app sent {m}, expected nothing")));
                }
                outbox.clear();
            }
            "model" => {
                let expected = normalize(&schema_app, line.get("model").unwrap_or(&Json::Null)).map_err(at)?;
                let got = normalize(&schema_app, &app.model_json().map_err(at)?).map_err(at)?;
                if got != expected {
                    return Err(at(format!("the app has {got}, expected {expected}")));
                }
            }
            "notice" => {
                let text = line.get("contains").and_then(Json::as_str).unwrap_or("");
                let notices = app.notices();
                if !notices.iter().any(|n| n.contains(text)) {
                    return Err(at(format!("the app didn't show “{text}” (it showed {notices:?})")));
                }
            }
            other => return Err(at(format!("unknown line type {other:?}"))),
        }
        for m in sent {
            if let Some(u) = update_of(&m, key.as_ref(), &room) {
                mine.push(u);
            }
            outbox.push_back(m);
        }
    }
    Ok(())
}

/// Parses a `.jsonl` transcript.
pub fn parse(text: &str) -> Result<Vec<Json>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str::<Json>(l).map_err(|e| format!("line {}: {e}", i + 1)))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Generating

/// Another person's document, with a fixed client id.
struct Peer {
    doc: Doc,
    ids: Ids,
}

impl Peer {
    fn new(client: u64, seed: u64) -> Peer {
        Peer { doc: Doc::with_client_id(client), ids: Ids::seeded(seed) }
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
        gridcraft::read(&self.doc.transact()).unwrap_or_default()
    }

    fn sheet(&self, i: usize) -> String {
        gridcraft::sheet_ids(&self.doc.transact()).get(i).cloned().unwrap_or_default()
    }

    /// Row and column ids of a sheet.
    fn lines(&self, sheet: &str) -> (Vec<String>, Vec<String>) {
        let t = self.doc.transact();
        let Some(m) = t.get_map(format!("sheet:{sheet}")) else { return (Vec::new(), Vec::new()) };
        let ids = |k: &str| match m.get(&t, k) {
            Some(yrs::Out::YArray(a)) => gridcraft::ids_of(&t, &a),
            _ => Vec::new(),
        };
        (ids("rows"), ids("cols"))
    }
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
        let mut s = Script { lines: Vec::new(), room: room.into(), key, nonces: Ids::seeded(0xC0FFEE) };
        s.lines.push(json!({"t": "meta", "name": name, "app": "gridcraft", "schema": 1, "format": 1, "description": description}));
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

    /// Plaintext → wire (encrypted for `method` when keyed).
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

    fn out(&mut self, method: &str, params: Json) {
        self.lines.push(json!({"t": "out", "msg": {"jsonrpc": "2.0", "method": method, "params": params}}));
    }

    fn out_decodes(&mut self, msg: Json, model: &Model, optional: bool) {
        let mut l = json!({"t": "out", "msg": msg, "decodesTo": model.to_json()});
        if optional {
            put(&mut l, "optional", json!(true));
        }
        self.lines.push(l);
    }

    fn model(&mut self, m: &Model) {
        self.lines.push(json!({"t": "model", "model": m.to_json()}));
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
            json!({"room": room, "clientId": "$clientId", "user": app_user(), "app": "gridcraft", "schema": 1, "encrypted": self.key.is_some()}),
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

const PEER: u64 = 1001;
/// A client id above any app's, so the peer's concurrent writes to the same map key win.
const HIGH_PEER: u64 = (1 << 53) - 2;

fn app_user() -> Json {
    json!({"id": "u-app", "name": "App Under Test", "color": "#3B7DD8"})
}

fn peer_user() -> Json {
    json!({"id": "u-peer", "name": "Peer", "color": "#D8513B"})
}

fn budget() -> Model {
    Model::sheet(
        "Budget",
        "Budget",
        &[("A1", "Item"), ("B1", "Cost"), ("A2", "Coffee"), ("B2", "3.5"), ("A3", "Tea"), ("B3", "2"), ("A4", "Total"), ("B4", "=SUM(B2:B3)")],
    )
}

fn peer_with_budget() -> Peer {
    let mut p = Peer::new(PEER, 11);
    p.edit(|t, ids| gridcraft::seed(t, &budget(), ids));
    p
}

fn set(peer: &mut Peer, sheet: usize, a1: &str, input: &str) -> Vec<u8> {
    let s = peer.sheet(sheet);
    let (r, c) = gridcraft::parse_a1(a1).unwrap_or((0, 0));
    peer.edit(|t, _| gridcraft::set_cell(t, &s, r, c, input))
}

fn join_transcript() -> Script {
    let room = "conformance:join";
    let mut s = Script::new(
        "gridcraft-1-join",
        "Joining a room: the first state, remote edits (cells, an inserted row, a deleted column, sheets), awareness, answering sync1, a local edit, leaving.",
        room,
        None,
    );
    s.join("join");
    s.note("Until the first state arrives the document is empty and read-only.");
    s.model(&Model::default());
    let mut p = peer_with_budget();
    let full = p.full();
    let u = s.wire("collab/sync2", &full);
    s.input("collab/sync2", json!({"room": room, "update": u, "from": PEER}));
    s.model(&p.model());
    let u = set(&mut p, 0, "C1", "Notes");
    s.update(PEER, &u);
    s.model(&p.model());
    s.note(
        "The peer inserts a row before row 3 and rewrites the total's formula in the same update: the app maps rows by id and doesn't adjust formulas again.",
    );
    let sheet = p.sheet(0);
    let u = p.edit(|t, ids| {
        gridcraft::insert_lines(t, &sheet, true, 2, 1, ids);
        for (a1, input) in [("A3", "Milk"), ("B3", "1.2"), ("B5", "=SUM(B2:B4)")] {
            if let Some((r, c)) = gridcraft::parse_a1(a1) {
                gridcraft::set_cell(t, &sheet, r, c, input);
            }
        }
    });
    s.update(PEER, &u);
    s.model(&p.model());
    let u = p.edit(|t, _| gridcraft::delete_lines(t, &sheet, false, 2, 1));
    s.update(PEER, &u);
    s.model(&p.model());
    let u = p.edit(|t, ids| {
        gridcraft::rename_sheet(t, &sheet, "Budget 2026");
        let notes = gridcraft::add_sheet(t, "Notes", None, ids);
        gridcraft::set_cell(t, &notes, 0, 0, "hello");
    });
    s.update(PEER, &u);
    s.model(&p.model());
    let (rows, cols) = p.lines(&sheet);
    let range = json!([[rows.first(), cols.first(), rows.get(1), cols.get(1)]]);
    let aw = awareness::encode(&[Entry { client: PEER, clock: 1, state: json!({"user": peer_user(), "selection": {"sheet": sheet, "ranges": range}}) }]);
    let u = s.wire("collab/awareness", &aw);
    s.input("collab/awareness", json!({"room": room, "update": u, "from": PEER}));
    s.model(&p.model());
    s.note("A message for a room the app isn't in is ignored.");
    let mut other = Peer::new(7, 12);
    let u = other.edit(|t, ids| gridcraft::seed(t, &Model::sheet("Other", "Other", &[("A1", "nope")]), ids));
    s.lines.push(json!({"t": "in", "valid": false, "msg": {"jsonrpc": "2.0", "method": "collab/update", "params": {"room": "conformance:other", "update": b64::encode(&u), "from": 7}}}));
    s.model(&p.model());
    s.note("A newcomer (2002) asks with an empty state vector; the app answers with everything, addressed to it.");
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 2002}));
    s.out_decodes(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": 2002}}), &p.model(), false);
    s.run("cell.set", json!({"cell": "D1", "input": "Local"}));
    let mut after = p.model();
    if let Some(sh) = after.sheets.first_mut() {
        sh.cells.insert("D1".into(), "Local".into());
    }
    s.out_decodes(json!({"jsonrpc": "2.0", "method": "collab/update", "params": {"room": room, "update": "$b64"}}), &after, false);
    s.model(&after);
    s.input("collab/leave", json!({"room": room, "reason": "The room was closed."}));
    s.note("After leaving, the document stays open with every edit, and edits aren't sent.");
    s.model(&after);
    s.run("cell.set", json!({"cell": "D2", "input": "After leaving"}));
    s.lines.push(json!({"t": "quiet"}));
    if let Some(sh) = after.sheets.first_mut() {
        sh.cells.insert("D2".into(), "After leaving".into());
    }
    s.model(&after);
    s
}

fn encrypted_transcript() -> Script {
    let room = "conformance:secret";
    let key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(3));
    let mut s = Script::new(
        "gridcraft-1-encrypted",
        "An end-to-end encrypted room: the stored snapshot (method collab/snapshot), updates, messages that must be dropped (wrong method, tampered, another room, wrong key, garbage), answering collab/snapshot and sync1 with ciphertext, \"Wrong room key?\" after 10 failures.",
        room,
        Some(key),
    );
    s.join("join");
    let mut p = peer_with_budget();
    s.note("The stored snapshot arrives as sync2 with snapshot: true and is decrypted with the method name collab/snapshot.");
    let full = p.full();
    let u = s.wire("collab/snapshot", &full);
    s.input("collab/sync2", json!({"room": room, "update": u, "snapshot": true}));
    s.model(&p.model());
    let u = set(&mut p, 0, "C1", "Secret");
    s.update(PEER, &u);
    s.model(&p.model());
    let good = p.model();
    let mut stray = peer_with_budget();
    let stray_update = set(&mut stray, 0, "E1", "must not appear");
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
    let aw = awareness::encode(&[Entry { client: PEER, clock: 1, state: json!({"user": peer_user(), "selection": {"sheet": p.sheet(0), "ranges": []}}) }]);
    let u = s.wire("collab/awareness", &aw);
    s.input("collab/awareness", json!({"room": room, "update": u, "from": PEER}));
    s.model(&good);
    s.note("The transport asks for the room's state to store: the answer is ciphertext for collab/snapshot.");
    s.request(1, "collab/snapshot", json!({"room": room}));
    s.out_decodes(json!({"jsonrpc": "2.0", "id": 1, "result": {"update": "$b64"}}), &good, false);
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 3003}));
    s.out_decodes(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": 3003}}), &good, false);
    s.run("cell.set", json!({"cell": "C2", "input": "local secret"}));
    let mut after = good.clone();
    if let Some(sh) = after.sheets.first_mut() {
        sh.cells.insert("C2".into(), "local secret".into());
    }
    s.out_decodes(json!({"jsonrpc": "2.0", "method": "collab/update", "params": {"room": room, "update": "$b64"}}), &after, false);
    s.note("Six more messages that don't decrypt (ten in all): the app shows \"Wrong room key?\".");
    for i in 0..6u8 {
        let junk: Vec<u8> = (0..40).map(|j| j ^ i).collect();
        s.invalid("collab/update", json!({"room": room, "update": b64::encode(&junk), "from": PEER}));
    }
    s.model(&after);
    s.lines.push(json!({"t": "notice", "contains": "Wrong room key?", "lang": "en"}));
    s
}

fn share_transcript() -> Script {
    let room = "conformance:share";
    let mut s = Script::new(
        "gridcraft-1-share",
        "Sharing the open document: seeding, answering collab/snapshot and sync1, local edits as one update each (a cell, an inserted row), and a concurrent remote edit (title and a new sheet) merging in.",
        room,
        None,
    );
    s.note("Edits before joining a room aren't sent anywhere.");
    let base = Model::sheet("", "Sheet1", &[("A1", "Item"), ("B1", "Cost"), ("A2", "Coffee"), ("B2", "3.5")]);
    for (a1, input) in [("A1", "Item"), ("B1", "Cost"), ("A2", "Coffee"), ("B2", "3.5")] {
        s.run("cell.set", json!({"cell": a1, "input": input}));
    }
    s.lines.push(json!({"t": "quiet"}));
    s.model(&base);
    s.join("share");
    s.request(1, "collab/snapshot", json!({"room": room}));
    s.out_decodes(json!({"jsonrpc": "2.0", "id": 1, "result": {"update": "$b64"}}), &base, false);
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 4004}));
    s.out_decodes(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": 4004}}), &base, false);
    let mut m = base.clone();
    let step = |s: &mut Script, m: &mut Model, command: &str, params: Json, f: &dyn Fn(&mut Model)| {
        s.run(command, params);
        f(m);
        s.out_decodes(json!({"jsonrpc": "2.0", "method": "collab/update", "params": {"room": room, "update": "$b64"}}), m, false);
        s.model(m);
    };
    let cells = |m: &mut Model, pairs: &[(&str, &str)]| {
        if let Some(sh) = m.sheets.first_mut() {
            sh.cells = pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        }
    };
    step(&mut s, &mut m, "cell.set", json!({"cell": "A3", "input": "Tea"}), &|m| {
        cells(m, &[("A1", "Item"), ("B1", "Cost"), ("A2", "Coffee"), ("B2", "3.5"), ("A3", "Tea")])
    });
    step(&mut s, &mut m, "home.insertRows", json!({"rows": "2:2"}), &|m| {
        cells(m, &[("A1", "Item"), ("B1", "Cost"), ("A3", "Coffee"), ("B3", "3.5"), ("A4", "Tea")])
    });
    step(&mut s, &mut m, "cell.set", json!({"cell": "A2", "input": "Milk"}), &|m| {
        cells(m, &[("A1", "Item"), ("B1", "Cost"), ("A2", "Milk"), ("A3", "Coffee"), ("B3", "3.5"), ("A4", "Tea")])
    });
    s.note("A peer that started from nothing sets the title and adds a sheet, concurrently: both merge (its client id is higher, so its title wins).");
    let mut p = Peer::new(HIGH_PEER, 13);
    let u = p.edit(|t, ids| {
        let meta = t.get_or_insert_map("meta");
        meta.insert(t, "title", "Shared budget");
        let id = gridcraft::add_sheet(t, "Peer notes", None, ids);
        gridcraft::set_cell(t, &id, 0, 0, "from the peer");
    });
    s.update(HIGH_PEER, &u);
    m.title = "Shared budget".into();
    m.sheets.push(gridcraft::SheetModel { name: "Peer notes".into(), cells: [("A1".to_string(), "from the peer".to_string())].into_iter().collect() });
    s.model(&m);
    s.request(2, "collab/snapshot", json!({"room": room}));
    s.out_decodes(json!({"jsonrpc": "2.0", "id": 2, "result": {"update": "$b64"}}), &m, false);
    s.input("collab/leave", json!({"room": room}));
    s.model(&m);
    s
}

fn robustness_transcript() -> Script {
    let room = "conformance:robust";
    let mut s = Script::new(
        "gridcraft-1-robustness",
        "Bad input never crashes the app or changes the document: missing fields, bad base64, bytes that aren't a Yjs update, a truncated update, an unknown status, another app's document (\"This room is for wordcraft 1\"), an unknown method, bad awareness.",
        room,
        None,
    );
    s.join("join");
    let mut p = peer_with_budget();
    let full = p.full();
    s.input("collab/sync2", json!({"room": room, "update": b64::encode(&full), "from": PEER}));
    let good = p.model();
    s.model(&good);
    s.invalid("collab/update", json!({"room": room, "from": PEER}));
    s.invalid("collab/update", json!({"update": "AAA=", "from": PEER}));
    s.invalid("collab/update", json!({"room": room, "update": "not base64!!", "from": PEER}));
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&[0xff; 16]), "from": PEER}));
    let u = set(&mut peer_with_budget(), 0, "Z9", "truncated");
    let cut = u.get(..u.len() / 2).unwrap_or_default().to_vec();
    s.invalid("collab/update", json!({"room": room, "update": b64::encode(&cut), "from": PEER}));
    s.input("collab/sync1", json!({"room": room, "stateVector": "%%%", "from": PEER}));
    s.lines.push(json!({"t": "quiet"}));
    s.input("collab/status", json!({"room": room, "state": "dancing"}));
    s.model(&good);
    s.note("A peer of another app (WordCraft) writes its meta: the app refuses the update and says so.");
    let mut w = Peer::new(HIGH_PEER - 1, 14);
    let u = w.edit(|t, _| {
        let meta = t.get_or_insert_map("meta");
        meta.insert(t, "app", "wordcraft");
        meta.insert(t, "schema", 1);
    });
    s.invalid("collab/sync2", json!({"room": room, "update": b64::encode(&u), "from": HIGH_PEER - 1}));
    s.model(&good);
    s.lines.push(json!({"t": "notice", "contains": "This room is for wordcraft 1", "lang": "en"}));
    let u = set(&mut p, 0, "D1", "still fine");
    s.update(PEER, &u);
    s.model(&p.model());
    s.input("collab/unknown", json!({"room": room}));
    s.input("collab/awareness", json!({"room": room, "update": "AQ==", "from": PEER}));
    s.model(&p.model());
    s
}

fn offline_transcript() -> Script {
    let room = "conformance:offline";
    let mut s = Script::new(
        "gridcraft-1-offline",
        "Going offline and back: local and remote edits made meanwhile merge after the app's new sync1, including both sides growing the sheet past its last row at once (block ids `~<block>.<i>` make them land in the same rows).",
        room,
        None,
    );
    s.join("join");
    let mut p = peer_with_budget();
    let full = p.full();
    s.input("collab/sync2", json!({"room": room, "update": b64::encode(&full), "from": PEER}));
    s.model(&p.model());
    s.input("collab/status", json!({"room": room, "state": "offline", "peers": [], "message": "Connection lost"}));
    s.note("Editing goes on offline; the app may send the updates now (the transport may queue them) or not.");
    let mut mine = p.model();
    for (a1, input) in [("B2", "4"), ("A70", "far below")] {
        s.run("cell.set", json!({"cell": a1, "input": input}));
        if let Some(sh) = mine.sheets.first_mut() {
            sh.cells.insert(a1.into(), input.into());
        }
        s.out_decodes(json!({"jsonrpc": "2.0", "method": "collab/update", "params": {"room": room, "update": "$b64"}}), &mine, true);
    }
    s.note("The peer edited too while the app was away, also past the last row; its update arrives once the app is back.");
    let sheet = p.sheet(0);
    let u = p.edit(|t, _| {
        for (a1, input) in [("A5", "Biscuits"), ("B100", "further below")] {
            if let Some((r, c)) = gridcraft::parse_a1(a1) {
                gridcraft::set_cell(t, &sheet, r, c, input);
            }
        }
    });
    s.input("collab/status", json!({"room": room, "state": "online", "peers": [{"clientId": PEER, "user": peer_user()}]}));
    s.out("collab/sync1", json!({"room": room, "stateVector": "$b64"}));
    s.update(PEER, &u);
    let mut both = p.model();
    if let Some(sh) = both.sheets.first_mut() {
        sh.cells.insert("B2".into(), "4".into());
        sh.cells.insert("A70".into(), "far below".into());
    }
    s.model(&both);
    s.note("The peer asks with its state vector: the app's answer holds its offline edits.");
    let sv = p.sv();
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&sv), "from": PEER}));
    s.out_decodes(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": PEER}}), &both, false);
    s
}

/// Every transcript: (file name, contents).
pub fn generate() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = [join_transcript(), share_transcript(), encrypted_transcript(), robustness_transcript(), offline_transcript()]
        .into_iter()
        .map(|s| {
            let name = s.lines.first().and_then(|m| m.get("name")).and_then(Json::as_str).unwrap_or("transcript").to_string();
            (format!("{name}.jsonl"), s.text())
        })
        .collect();
    out.extend(crate::deck_transcripts::generate());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let mut v = HashMap::new();
        assert!(matches(&json!({"a": "$clientId", "b": "$b64"}), Some(&json!({"a": 5, "b": "AA==", "c": 1})), &mut v).is_ok());
        assert!(matches(&json!({"a": "$clientId"}), Some(&json!({"a": 6})), &mut v).is_err());
        assert!(matches(&json!({"to": "$absent"}), Some(&json!({})), &mut v).is_ok());
        assert!(matches(&json!([1, "$any"]), Some(&json!([1, null])), &mut v).is_ok());
        assert!(matches(&json!({"x": 1}), Some(&json!({"x": 2})), &mut v).is_err());
    }

    #[test]
    fn every_transcript_passes_with_the_reference_app() {
        for (name, text) in generate() {
            let lines = parse(&text).unwrap();
            let mut app = reference_app(&lines, Some(42));
            if let Err(e) = replay(&lines, app.as_mut()) {
                panic!("{name}: {e}");
            }
        }
    }
}
