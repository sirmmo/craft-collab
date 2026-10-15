//! The reference app for DeckCraft schema 1: what DeckCraft does in a room (protocol 2,
//! section 6), with the presentation kept in a Yjs document. The `deckcraft-1-*` transcripts are
//! checked against it. Its commands are DeckCraft's, with the same parameters, so the app replays
//! the transcripts with its own engine.

use std::collections::BTreeMap;

use craft_collab_kit::b64;
use craft_collab_kit::collab::put;
use serde_json::{Value as Json, json};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Transact, TransactionMut, Update};

use crate::awareness::{self, Entry};
use crate::crypto;
use crate::deckcraft::{self, Model};
use crate::gridcraft::Ids;
use crate::refapp::WRONG_KEY_AFTER;

/// Text being edited: the slide, the shape (`None`: the notes), and the selection.
#[derive(Clone, Debug, PartialEq)]
struct Editing {
    slide: String,
    shape: Option<String>,
    anchor: (usize, usize),
    caret: (usize, usize),
}

/// A simulated DeckCraft.
pub struct DeckRefApp {
    pub doc: Doc,
    pub client_id: u64,
    pub room: Option<String>,
    pub key: Option<[u8; 32]>,
    pub user: Json,
    pub state: String,
    pub joining: bool,
    pub notices: Vec<String>,
    pub decrypt_failures: u32,
    pub awareness: BTreeMap<u64, Json>,
    ids: Ids,
    nonces: Ids,
    awareness_clock: u64,
    /// The current slide (index).
    current: usize,
    /// Selected shapes on the current slide.
    selected: Vec<String>,
    editing: Option<Editing>,
}

fn client_ids(seed: u64) -> u64 {
    let mut i = Ids::seeded(seed);
    (i.bits() & ((1 << 53) - 1)).max(1)
}

/// `(paragraph, char)` from JSON `[p, c]`.
fn pos(v: Option<&Json>) -> Option<(usize, usize)> {
    let a = v?.as_array()?;
    Some((a.first()?.as_u64()? as usize, a.get(1)?.as_u64()? as usize))
}

impl DeckRefApp {
    /// A new app with a new presentation: one empty slide (DeckCraft's own new presentation has a
    /// title slide; the transcripts replace it before they look). `seed` makes ids and nonces
    /// deterministic; `None` makes them random.
    pub fn new(seed: Option<u64>) -> DeckRefApp {
        let seed = seed.unwrap_or_else(craft_collab_kit::ids::random_u64);
        let client_id = client_ids(seed);
        let mut app = DeckRefApp {
            doc: deckcraft::new_doc(client_id),
            client_id,
            room: None,
            key: None,
            user: Json::Null,
            state: String::new(),
            joining: false,
            notices: Vec::new(),
            decrypt_failures: 0,
            awareness: BTreeMap::new(),
            ids: Ids::seeded(seed ^ 0x5151),
            nonces: Ids::seeded(seed ^ 0xA0A0),
            awareness_clock: 0,
            current: 0,
            selected: Vec::new(),
            editing: None,
        };
        let model = Model { title: String::new(), slides: vec![Default::default()] };
        let mut t = app.doc.transact_mut_with("seed");
        deckcraft::seed(&mut t, &model, &mut app.ids);
        drop(t);
        app
    }

    pub fn model(&self) -> Result<Model, String> {
        deckcraft::read(&self.doc.transact())
    }

    fn nonce(&mut self) -> [u8; 12] {
        let a = self.nonces.bits().to_le_bytes();
        let b = self.nonces.bits().to_le_bytes();
        let mut n = [0u8; 12];
        for (i, x) in n.iter_mut().enumerate() {
            *x = if i < 8 { a.get(i).copied().unwrap_or(0) } else { b.get(i - 8).copied().unwrap_or(0) };
        }
        n
    }

    fn wire(&mut self, method: &str, plain: &[u8]) -> String {
        match (self.key, self.room.clone()) {
            (Some(k), Some(room)) => {
                let n = self.nonce();
                b64::encode(&crypto::seal(&k, &room, method, plain, n).unwrap_or_default())
            }
            _ => b64::encode(plain),
        }
    }

    fn unwire(&mut self, method: &str, text: &str) -> Option<Vec<u8>> {
        let data = b64::decode(text)?;
        let (Some(k), Some(room)) = (self.key, self.room.as_deref()) else { return Some(data) };
        match crypto::open(&k, room, method, &data) {
            Some(p) => Some(p),
            None => {
                self.decrypt_failures += 1;
                if self.decrypt_failures == WRONG_KEY_AFTER {
                    self.notices.push("Wrong room key?".into());
                }
                None
            }
        }
    }

    fn sync1(&self) -> Json {
        let sv = self.doc.transact().state_vector().encode_v1();
        json!({"jsonrpc": "2.0", "method": "collab/sync1", "params": {"room": self.room, "stateVector": b64::encode(&sv)}})
    }

    /// Joins a room as `collab.join` says: the messages the app sends.
    pub fn join(&mut self, join: &Json) -> Result<Vec<Json>, String> {
        let room = join.get("room").and_then(Json::as_str).ok_or("a join needs `room`")?.to_string();
        if self.room.is_some() {
            return Err("this presentation is already in a room".into());
        }
        let key = match join.get("key").and_then(Json::as_str) {
            Some(k) => Some(<[u8; 32]>::try_from(b64::decode(k).ok_or("the key isn't base64")?.as_slice()).map_err(|_| "the key isn't 32 bytes")?),
            None => None,
        };
        let mode = join.get("mode").and_then(Json::as_str).unwrap_or("share");
        let model = self.model().unwrap_or_default();
        // The shared document is built from the open one with new ids: the selection follows
        // by position.
        let selected: Vec<usize> = {
            let txn = self.doc.transact();
            let shapes = deckcraft::slide_ids(&txn).get(self.current).map(|s| deckcraft::shape_ids(&txn, s)).unwrap_or_default();
            self.selected.iter().filter_map(|id| shapes.iter().position(|x| x == id)).collect()
        };
        self.client_id = client_ids(self.ids.bits());
        self.doc = deckcraft::new_doc(self.client_id);
        if mode == "share" {
            let mut t = self.doc.transact_mut_with("seed");
            deckcraft::seed(&mut t, &model, &mut self.ids);
            drop(t);
            let txn = self.doc.transact();
            let shapes = deckcraft::slide_ids(&txn).get(self.current).map(|s| deckcraft::shape_ids(&txn, s)).unwrap_or_default();
            self.selected = selected.into_iter().filter_map(|i| shapes.get(i).cloned()).collect();
        } else {
            self.joining = true;
            self.current = 0;
            self.selected.clear();
        }
        self.editing = None;
        self.room = Some(room.clone());
        self.key = key;
        self.user = join.get("user").cloned().unwrap_or(Json::Null);
        self.state = "connecting".into();
        let joined = json!({"jsonrpc": "2.0", "method": "collab/joined", "params": {
            "room": room, "clientId": self.client_id, "user": self.user, "app": deckcraft::APP, "schema": deckcraft::SCHEMA, "encrypted": self.key.is_some()
        }});
        Ok(vec![joined, self.sync1()])
    }

    fn apply(&mut self, plain: &[u8]) -> Result<(), String> {
        let update = Update::decode_v1(plain).map_err(|e| format!("not a Yjs update: {e}"))?;
        let probe = Doc::new();
        {
            let mut t = probe.transact_mut();
            let mine = Update::decode_v1(&self.doc.transact().encode_state_as_update_v1(&StateVector::default())).map_err(|e| e.to_string())?;
            t.apply_update(mine).map_err(|e| e.to_string())?;
            let theirs = Update::decode_v1(plain).map_err(|e| e.to_string())?;
            t.apply_update(theirs).map_err(|e| e.to_string())?;
        }
        deckcraft::check_meta(&probe.transact())?;
        let mut t = self.doc.transact_mut_with("remote");
        t.apply_update(update).map_err(|e| e.to_string())?;
        drop(t);
        if self.joining && self.doc.transact().get_map("meta").is_some() {
            self.joining = false;
        }
        Ok(())
    }

    fn apply_wire(&mut self, method: &str, text: &str) {
        let Some(plain) = self.unwire(method, text) else { return };
        if let Err(e) = self.apply(&plain)
            && !self.notices.contains(&e)
        {
            self.notices.push(e);
        }
    }

    /// Handles one message from the transport; the messages the app sends back.
    pub fn receive(&mut self, msg: &Json) -> Vec<Json> {
        let method = msg.get("method").and_then(Json::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Json::Null);
        let room = params.get("room").and_then(Json::as_str);
        if room.is_none() || room != self.room.as_deref() {
            return Vec::new();
        }
        let room = self.room.clone().unwrap_or_default();
        let mut out = Vec::new();
        match (method, msg.get("id")) {
            ("collab/snapshot", Some(id)) => {
                let full = self.doc.transact().encode_state_as_update_v1(&StateVector::default());
                let update = self.wire("collab/snapshot", &full);
                out.push(json!({"jsonrpc": "2.0", "id": id, "result": {"update": update}}));
            }
            ("collab/status", None) => {
                let state = params.get("state").and_then(Json::as_str).unwrap_or("").to_string();
                if let Some(m) = params.get("message").and_then(Json::as_str) {
                    self.notices.push(m.to_string());
                }
                if state == "online" && self.state != "online" {
                    out.push(self.sync1());
                }
                if matches!(state.as_str(), "connecting" | "online" | "offline" | "ended") {
                    self.state = state;
                }
            }
            ("collab/sync1", None) => {
                let Some(sv) = params.get("stateVector").and_then(Json::as_str).and_then(b64::decode) else { return out };
                let Ok(sv) = StateVector::decode_v1(&sv) else { return out };
                let diff = self.doc.transact().encode_diff_v1(&sv);
                let update = self.wire("collab/sync2", &diff);
                let mut p = json!({"room": room, "update": update});
                if let Some(from) = params.get("from").and_then(Json::as_u64) {
                    put(&mut p, "to", json!(from));
                }
                out.push(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": p}));
                let mine = self.doc.transact().state_vector();
                if sv.iter().any(|(client, clock)| mine.get(client) < *clock) {
                    out.push(self.sync1());
                }
            }
            ("collab/sync2", None) => {
                let snapshot = params.get("snapshot").and_then(Json::as_bool).unwrap_or(false);
                if let Some(u) = params.get("update").and_then(Json::as_str) {
                    self.apply_wire(if snapshot { "collab/snapshot" } else { "collab/sync2" }, u);
                }
            }
            ("collab/update", None) => {
                if let Some(u) = params.get("update").and_then(Json::as_str) {
                    self.apply_wire("collab/update", u);
                }
            }
            ("collab/awareness", None) => {
                let Some(u) = params.get("update").and_then(Json::as_str) else { return out };
                let Some(plain) = self.unwire("collab/awareness", u) else { return out };
                for e in awareness::decode(&plain).unwrap_or_default() {
                    if e.state.is_null() {
                        self.awareness.remove(&e.client);
                    } else {
                        self.awareness.insert(e.client, e.state);
                    }
                }
            }
            ("collab/leave", None) => {
                self.room = None;
                self.key = None;
                self.state = "ended".into();
                self.joining = false;
            }
            _ => {}
        }
        out
    }

    fn current_slide(&self) -> Result<String, String> {
        deckcraft::slide_ids(&self.doc.transact()).get(self.current).cloned().ok_or_else(|| "no current slide".into())
    }

    /// The end of a text: `(last paragraph, its length)`.
    fn end_of(&self, slide: &str, shape: Option<&str>) -> (usize, usize) {
        let txn = self.doc.transact();
        let Some(t) = deckcraft::text_ref(&txn, slide, shape) else { return (0, 0) };
        let s = t.get_string(&txn);
        let s = s.strip_suffix('\n').unwrap_or(&s);
        let paras: Vec<&str> = s.split('\n').collect();
        (paras.len().saturating_sub(1), paras.last().map(|p| p.chars().count()).unwrap_or(0))
    }

    /// Runs one of DeckCraft's commands as one transaction; in a room, the `collab/update` it
    /// sends. Commands: `slide.new {layout?, at?}`, `slide.delete {index?}`, `slide.go {index}`,
    /// `shape.insert {preset, rect, text?}`, `shape.setBounds {ids?, x, y, w, h}`,
    /// `edit.select {ids}`, `text.edit {id?, at?}`, `text.select {anchor, caret}`,
    /// `text.insert {text}`, `slide.notes {text, index?}`.
    pub fn run(&mut self, command: &str, params: &Json) -> Result<Vec<Json>, String> {
        if command == "collab.join" {
            return self.join(params);
        }
        let ids_param = |params: &Json| -> Option<Vec<String>> {
            Some(params.get("ids")?.as_array()?.iter().filter_map(|v| v.as_u64().map(|n| deckcraft::id_str(n as u32))).collect())
        };
        match command {
            "slide.go" => {
                let n = deckcraft::slide_ids(&self.doc.transact()).len();
                self.current = params.get("index").and_then(Json::as_u64).map(|i| i as usize).filter(|i| *i < n).ok_or("no such slide")?;
                self.selected.clear();
                self.editing = None;
                return Ok(Vec::new());
            }
            "edit.select" => {
                self.selected = ids_param(params).ok_or("edit.select needs `ids`")?;
                self.editing = None;
                return Ok(Vec::new());
            }
            "text.edit" => {
                let slide = self.current_slide()?;
                let shape = match params.get("id").and_then(Json::as_u64) {
                    Some(n) => deckcraft::id_str(n as u32),
                    None => self.selected.first().cloned().ok_or("select a shape with text")?,
                };
                let at = pos(params.get("at")).unwrap_or_else(|| self.end_of(&slide, Some(&shape)));
                self.selected = vec![shape.clone()];
                self.editing = Some(Editing { slide, shape: Some(shape), anchor: at, caret: at });
                return Ok(Vec::new());
            }
            "text.select" => {
                let e = self.editing.as_mut().ok_or("not editing text")?;
                e.anchor = pos(params.get("anchor")).ok_or("text.select needs `anchor`")?;
                e.caret = pos(params.get("caret")).ok_or("text.select needs `caret`")?;
                return Ok(Vec::new());
            }
            _ => {}
        }
        if self.joining {
            return Err("the presentation is read-only until the room's state arrives".into());
        }
        let mut ids = self.ids.clone();
        let text = |k: &str| params.get(k).and_then(Json::as_str).map(str::to_string);
        let update = {
            let doc = self.doc.clone();
            let mut t: TransactionMut = doc.transact_mut_with("local");
            match command {
                "slide.new" => {
                    let n = deckcraft::slide_ids(&t).len();
                    let at = params.get("at").and_then(Json::as_u64).map(|a| a as usize).unwrap_or(if n == 0 { 0 } else { self.current + 1 }).min(n);
                    deckcraft::add_slide(&mut t, at, &mut ids);
                    self.current = at;
                    self.selected.clear();
                    self.editing = None;
                }
                "slide.delete" => {
                    let all = deckcraft::slide_ids(&t);
                    let i = params.get("index").and_then(Json::as_u64).map(|i| i as usize).unwrap_or(self.current);
                    let id = all.get(i).ok_or("no such slide")?;
                    deckcraft::delete_slide(&mut t, id);
                    self.current = i.min(all.len().saturating_sub(2));
                    self.selected.clear();
                    self.editing = None;
                }
                "shape.insert" => {
                    let r = params.get("rect").and_then(Json::as_array).ok_or("shape.insert needs `rect`")?;
                    let n = |i: usize| r.get(i).and_then(Json::as_f64).ok_or("`rect` is [x, y, w, h]");
                    let rect = [n(0)?, n(1)?, n(2)?, n(3)?];
                    let slide = deckcraft::slide_ids(&t).get(self.current).cloned().ok_or("no current slide")?;
                    let id = deckcraft::add_shape(&mut t, &slide, rect, text("text").as_deref(), &mut ids);
                    self.selected = vec![id];
                    self.editing = None;
                }
                "shape.setBounds" => {
                    let slide = deckcraft::slide_ids(&t).get(self.current).cloned().ok_or("no current slide")?;
                    let n = |k: &str| params.get(k).and_then(Json::as_f64).ok_or(format!("shape.setBounds needs `{k}`"));
                    let rect = [n("x")?, n("y")?, n("w")?, n("h")?];
                    for id in ids_param(params).unwrap_or_else(|| self.selected.clone()) {
                        deckcraft::set_rect(&mut t, &slide, &id, rect);
                    }
                }
                "text.insert" => {
                    let what = text("text").ok_or("text.insert needs `text`")?;
                    let e = self.editing.clone().ok_or("not editing text")?;
                    let (a, b) = if e.anchor <= e.caret { (e.anchor, e.caret) } else { (e.caret, e.anchor) };
                    if a != b
                        && let Some(tr) = deckcraft::text_ref(&t, &e.slide, e.shape.as_deref())
                    {
                        let (x, y) = (deckcraft::offset(&t, &tr, a), deckcraft::offset(&t, &tr, b));
                        deckcraft::delete_text(&mut t, &e.slide, e.shape.as_deref(), a, y.saturating_sub(x));
                    }
                    deckcraft::type_text(&mut t, &e.slide, e.shape.as_deref(), a, &what);
                    let lines: Vec<&str> = what.split('\n').collect();
                    let caret = if lines.len() > 1 {
                        (a.0 + lines.len() - 1, lines.last().map(|l| l.chars().count()).unwrap_or(0))
                    } else {
                        (a.0, a.1 + what.chars().count())
                    };
                    self.editing = Some(Editing { anchor: caret, caret, ..e });
                }
                "slide.notes" => {
                    let all = deckcraft::slide_ids(&t);
                    let i = params.get("index").and_then(Json::as_u64).map(|i| i as usize).unwrap_or(self.current);
                    let id = all.get(i).ok_or("no such slide")?;
                    deckcraft::set_notes(&mut t, id, &text("text").unwrap_or_default());
                }
                other => return Err(format!("the reference app can't run {other}")),
            }
            t.encode_update_v1()
        };
        self.ids = ids;
        if self.room.is_none() || update.len() <= 2 {
            return Ok(Vec::new());
        }
        let u = self.wire("collab/update", &update);
        Ok(vec![json!({"jsonrpc": "2.0", "method": "collab/update", "params": {"room": self.room, "update": u}})])
    }

    /// An awareness update with this app's user and a selection.
    pub fn awareness_message(&mut self, selection: Json) -> Option<Json> {
        self.room.as_ref()?;
        self.awareness_clock += 1;
        let state = json!({"user": self.user, "selection": selection});
        let bytes = awareness::encode(&[Entry { client: self.client_id, clock: self.awareness_clock, state }]);
        let u = self.wire("collab/awareness", &bytes);
        Some(json!({"jsonrpc": "2.0", "method": "collab/awareness", "params": {"room": self.room, "update": u}}))
    }

    /// The model of a peer that has every update in `updates` (plaintext).
    pub fn model_of(updates: &[Vec<u8>]) -> Result<Model, String> {
        let doc = deckcraft::new_doc(1);
        let mut t = doc.transact_mut();
        for u in updates {
            let u = Update::decode_v1(u).map_err(|e| format!("not a Yjs update: {e}"))?;
            t.apply_update(u).map_err(|e| e.to_string())?;
        }
        deckcraft::read(&t)
    }
}
