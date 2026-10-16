//! The reference app: what a craft app does in a room (protocol 2, section 6), for any schema
//! ([`Schema`]: GridCraft 1, DeckCraft 1, WordCraft 1), with the document kept in a Yjs doc. The
//! transcripts are checked against it, and the transport checks use it as the simulated apps on
//! both ends.

use std::collections::BTreeMap;

use craft_collab_kit::b64;
use craft_collab_kit::collab::put;
use serde_json::{Value as Json, json};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, StateVector, Transact, TransactionMut, Update};

use crate::awareness::{self, Entry};
use crate::chat::{self, Message};
use crate::crypto;
use crate::gridcraft::{GridCraft, Ids};
use crate::schema::Schema;

/// After this many messages that fail to decrypt, the app says "Wrong room key?".
pub const WRONG_KEY_AFTER: u32 = 10;

/// A simulated app (GridCraft unless said otherwise).
pub struct RefApp<S: Schema = GridCraft> {
    pub doc: Doc,
    pub client_id: u64,
    pub room: Option<String>,
    pub key: Option<[u8; 32]>,
    pub user: Json,
    /// The last `collab/status` state.
    pub state: String,
    /// Waiting for the first state of a joined room (read-only).
    pub joining: bool,
    /// What the app showed in its status bar.
    pub notices: Vec<String>,
    pub decrypt_failures: u32,
    /// Others' awareness states by client id.
    pub awareness: BTreeMap<u64, Json>,
    /// Plaintext updates this app sent (sync2, update, snapshot answers).
    pub sent: Vec<Vec<u8>>,
    /// The model at each snapshot answer.
    pub snapshot_models: Vec<S::Model>,
    /// Updates applied from others.
    pub applied: u32,
    /// The view: active sheet, selection.
    pub view: S::View,
    /// The app's own user, the author of chat messages outside a room.
    pub local_user: Json,
    /// Typing in the chat (`chat.typing`): the next awareness state carries `typing: true`.
    pub typing: bool,
    /// What the last command returned (`chat.send` → `{id}`, `chat.list`, `chat.clear`).
    pub last_result: Json,
    /// Chat message times: `None` is the system clock, `Some(t)` a clock that starts at `t` and
    /// ticks a second per message (deterministic tests).
    pub clock: Option<i64>,
    ids: Ids,
    chat_ids: Ids,
    nonces: Ids,
    awareness_clock: u64,
}

fn client_ids(seed: u64) -> u64 {
    let mut i = Ids::seeded(seed);
    (i.bits() & ((1 << 53) - 1)).max(1)
}

impl<S: Schema> RefApp<S> {
    /// A new app with an empty document. `seed` makes its client id, row ids and nonces
    /// deterministic; `None` makes them random.
    pub fn new(seed: Option<u64>) -> RefApp<S> {
        let seed = seed.unwrap_or_else(craft_collab_kit::ids::random_u64);
        let client_id = client_ids(seed);
        let mut app = RefApp {
            doc: S::new_doc(client_id),
            client_id,
            room: None,
            key: None,
            user: Json::Null,
            state: String::new(),
            joining: false,
            notices: Vec::new(),
            decrypt_failures: 0,
            awareness: BTreeMap::new(),
            sent: Vec::new(),
            snapshot_models: Vec::new(),
            applied: 0,
            view: S::View::default(),
            local_user: json!({"id": "u-local", "name": "Local User", "color": "#2E8B57"}),
            typing: false,
            last_result: Json::Null,
            clock: None,
            ids: Ids::seeded(seed ^ 0x5151),
            chat_ids: Ids::seeded(seed ^ 0xC4A7),
            nonces: Ids::seeded(seed ^ 0xA0A0),
            awareness_clock: 0,
        };
        // A new document, as in the app (GridCraft: one empty sheet; DeckCraft: one empty slide;
        // WordCraft: one empty paragraph).
        app.load(&S::blank());
        app
    }

    /// Starts from a model (as if it had been opened from a file), replacing the document.
    pub fn load(&mut self, model: &S::Model) {
        self.doc = S::new_doc(self.client_id);
        self.view = S::View::default();
        let mut t = self.doc.transact_mut_with("seed");
        S::seed(&mut t, model, &mut self.ids);
    }

    pub fn model(&self) -> Result<S::Model, String> {
        S::read(&self.doc.transact())
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

    /// Plaintext → what goes on the wire for `method` (encrypted when keyed), base64.
    fn wire(&mut self, method: &str, plain: &[u8]) -> String {
        match (self.key, self.room.clone()) {
            (Some(k), Some(room)) => {
                let n = self.nonce();
                b64::encode(&crypto::seal(&k, &room, method, plain, n).unwrap_or_default())
            }
            _ => b64::encode(plain),
        }
    }

    /// The wire form of `update` → plaintext, counting failures.
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

    fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    fn sync1(&self) -> Json {
        json!({"jsonrpc": "2.0", "method": "collab/sync1", "params": {"room": self.room, "stateVector": b64::encode(&self.state_vector())}})
    }

    /// Joins a room as `collab.join` says (`{room, mode, user, role, key?}`): the messages the
    /// app sends (`collab/joined`, `collab/sync1`).
    pub fn join(&mut self, join: &Json) -> Result<Vec<Json>, String> {
        let room = join.get("room").and_then(Json::as_str).ok_or("a join needs `room`")?.to_string();
        if self.room.is_some() {
            return Err(format!("this {} is already in a room", S::DOCUMENT));
        }
        let key = match join.get("key").and_then(Json::as_str) {
            Some(k) => Some(<[u8; 32]>::try_from(b64::decode(k).ok_or("the key isn't base64")?.as_slice()).map_err(|_| "the key isn't 32 bytes")?),
            None => None,
        };
        let mode = join.get("mode").and_then(Json::as_str).unwrap_or("share");
        // A new client id per join (section 3), and for `share` the seed in one transaction.
        let model = self.model().unwrap_or_default();
        self.client_id = client_ids(self.ids.bits());
        let old = std::mem::replace(&mut self.doc, S::new_doc(self.client_id));
        let share = mode == "share";
        if share {
            // The file's chat goes into the room with the document, in the same transaction.
            let chat = chat::read(&old.transact());
            let mut t = self.doc.transact_mut_with("seed");
            S::seed(&mut t, &model, &mut self.ids);
            chat::seed(&mut t, &chat);
        } else {
            self.joining = true;
        }
        self.view = S::rejoin_view(&self.view, &old, &self.doc, share);
        self.room = Some(room.clone());
        self.key = key;
        self.user = join.get("user").cloned().unwrap_or(Json::Null);
        self.state = "connecting".into();
        let joined = json!({"jsonrpc": "2.0", "method": "collab/joined", "params": {
            "room": room, "clientId": self.client_id, "user": self.user, "app": S::APP, "schema": S::SCHEMA, "encrypted": self.key.is_some()
        }});
        Ok(vec![joined, self.sync1()])
    }

    /// Applies a plaintext update from a peer, unless it's for another app or schema.
    fn apply(&mut self, plain: &[u8]) -> Result<(), String> {
        let update = Update::decode_v1(plain).map_err(|e| format!("not a Yjs update: {e}"))?;
        // Check it on a copy first: an update for another app never enters the document.
        let probe = Doc::new();
        {
            let mut t = probe.transact_mut();
            let mine = Update::decode_v1(&self.doc.transact().encode_state_as_update_v1(&StateVector::default())).map_err(|e| e.to_string())?;
            t.apply_update(mine).map_err(|e| e.to_string())?;
            let theirs = Update::decode_v1(plain).map_err(|e| e.to_string())?;
            t.apply_update(theirs).map_err(|e| e.to_string())?;
        }
        S::check_meta(&probe.transact())?;
        let mut t = self.doc.transact_mut_with("remote");
        t.apply_update(update).map_err(|e| e.to_string())?;
        drop(t);
        self.applied += 1;
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

    fn mine(&self, msg: &Json) -> bool {
        let room = msg.get("params").and_then(|p| p.get("room")).and_then(Json::as_str);
        room.is_some() && room == self.room.as_deref()
    }

    /// Handles one message from the transport; the messages the app sends back.
    pub fn receive(&mut self, msg: &Json) -> Vec<Json> {
        let method = msg.get("method").and_then(Json::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Json::Null);
        if !self.mine(msg) {
            return Vec::new();
        }
        let room = self.room.clone().unwrap_or_default();
        let mut out = Vec::new();
        match (method, msg.get("id")) {
            ("collab/snapshot", Some(id)) => {
                let full = self.doc.transact().encode_state_as_update_v1(&StateVector::default());
                if let Ok(m) = self.model() {
                    self.snapshot_models.push(m);
                }
                self.sent.push(full.clone());
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
                self.sent.push(diff.clone());
                let update = self.wire("collab/sync2", &diff);
                let mut p = json!({"room": room, "update": update});
                if let Some(from) = params.get("from").and_then(Json::as_u64) {
                    put(&mut p, "to", json!(from));
                }
                out.push(json!({"jsonrpc": "2.0", "method": "collab/sync2", "params": p}));
                // The asker has changes this app lacks: ask for them.
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

    /// Runs one of the app's commands as one transaction (the schema's
    /// [`Schema::view_command`]s and [`Schema::edit`]s, and `collab.join`); in a room, the
    /// `collab/update` it sends.
    pub fn run(&mut self, command: &str, params: &Json) -> Result<Vec<Json>, String> {
        self.last_result = Json::Null;
        if command == "collab.join" {
            return self.join(params);
        }
        if command.starts_with("chat.") {
            return self.chat_command(command, params);
        }
        let viewed = {
            let t = self.doc.transact();
            S::view_command(&t, &mut self.view, command, params)
        };
        if let Some(r) = viewed {
            return r.map(|_| Vec::new());
        }
        if self.joining {
            return Err(format!("the {} is read-only until the room's state arrives", S::DOCUMENT));
        }
        let mut ids = self.ids.clone();
        let mut view = self.view.clone();
        let update = {
            let mut t: TransactionMut = self.doc.transact_mut_with("local");
            S::edit(&mut t, &mut view, command, params, &mut ids)?;
            t.encode_update_v1()
        };
        self.ids = ids;
        self.view = view;
        Ok(self.send_update(update))
    }

    /// The `collab/update` for a local transaction, when in a room and it changed something.
    fn send_update(&mut self, update: Vec<u8>) -> Vec<Json> {
        if self.room.is_none() || update.len() <= 2 {
            return Vec::new();
        }
        self.sent.push(update.clone());
        let u = self.wire("collab/update", &update);
        vec![json!({"jsonrpc": "2.0", "method": "collab/update", "params": {"room": self.room, "update": u}})]
    }

    fn now(&mut self) -> i64 {
        match self.clock.as_mut() {
            Some(t) => {
                *t += 1000;
                *t
            }
            None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0),
        }
    }

    /// Who writes chat messages: the room's `user`, or the app's own outside a room.
    fn author(&self) -> chat::Author {
        let user = if self.room.is_some() { &self.user } else { &self.local_user };
        chat::Author::from_json(user).unwrap_or_else(|| chat::Author { id: "u-local".into(), name: "Local User".into(), color: "#2E8B57".into() })
    }

    /// The chat commands (`docs/chat.md`): `chat.send {text, replyTo?}` → `{id}`,
    /// `chat.edit {id, text}`, `chat.delete {id}` (the author only), `chat.clear` → `{removed}`,
    /// `chat.list {since?, limit?}`, `chat.typing {typing}`. Each change is one transaction
    /// (origin `chat`: not undoable).
    fn chat_command(&mut self, command: &str, params: &Json) -> Result<Vec<Json>, String> {
        let text = |k: &str| params.get(k).and_then(Json::as_str).map(str::to_string);
        if command == "chat.typing" {
            self.typing = params.get("typing").and_then(Json::as_bool).unwrap_or(true);
            return Ok(Vec::new());
        }
        if command == "chat.list" {
            self.last_result = chat::list(&chat::of(&self.doc), params);
            return Ok(Vec::new());
        }
        if self.joining {
            return Err(format!("the {} is read-only until the room's state arrives", S::DOCUMENT));
        }
        let author = self.author();
        let at = self.now();
        let id = loop {
            let id = self.chat_ids.next_id();
            if !chat::of(&self.doc).messages.iter().any(|m| m.id == id) {
                break id;
            }
        };
        let update = {
            let mut t = self.doc.transact_mut_with("chat");
            match command {
                "chat.send" => {
                    let m =
                        Message { id: id.clone(), author, at, text: Some(text("text").unwrap_or_default()), reply_to: text("replyTo"), ..Message::default() };
                    chat::send(&mut t, &m)?;
                    self.last_result = json!({"id": id});
                    self.typing = false;
                }
                "chat.edit" => chat::edit(&mut t, &text("id").unwrap_or_default(), &author.id, &text("text").unwrap_or_default(), at)?,
                "chat.delete" => chat::delete(&mut t, &text("id").unwrap_or_default(), &author.id)?,
                "chat.clear" => {
                    let removed = chat::clear(&mut t, at);
                    self.last_result = json!({"removed": removed});
                }
                other => return Err(format!("unknown command {other}")),
            }
            t.encode_update_v1()
        };
        Ok(self.send_update(update))
    }

    /// Who's typing in the chat (the others' awareness with `typing: true`).
    pub fn typing(&self) -> Vec<String> {
        chat::typing(self.awareness.values())
    }

    /// An awareness update with this app's user and a selection.
    pub fn awareness_message(&mut self, selection: Json) -> Option<Json> {
        self.room.as_ref()?;
        self.awareness_clock += 1;
        let mut state = json!({"user": self.user, "selection": selection});
        if let (true, Some(o)) = (self.typing, state.as_object_mut()) {
            o.insert("typing".into(), json!(true));
        }
        let bytes = awareness::encode(&[Entry { client: self.client_id, clock: self.awareness_clock, state }]);
        let u = self.wire("collab/awareness", &bytes);
        Some(json!({"jsonrpc": "2.0", "method": "collab/awareness", "params": {"room": self.room, "update": u}}))
    }

    /// The model of a peer that has every update in `updates` (plaintext).
    /// Bytes that aren't updates (ciphertext, say) are an error; `yrs` can panic on some, which
    /// counts as one too.
    pub fn model_of(updates: &[Vec<u8>]) -> Result<S::Model, String> {
        std::panic::catch_unwind(|| {
            let doc = S::new_doc(1);
            let mut t = doc.transact_mut();
            for u in updates {
                let u = Update::decode_v1(u).map_err(|e| format!("not a Yjs update: {e}"))?;
                t.apply_update(u).map_err(|e| e.to_string())?;
            }
            S::read(&t)
        })
        .unwrap_or_else(|_| Err("not a Yjs update (yrs panicked)".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gridcraft::Model;

    /// Delivers every message between two apps until they're quiet.
    fn exchange<S: Schema>(a: &mut RefApp<S>, b: &mut RefApp<S>, mut to_b: Vec<Json>, mut to_a: Vec<Json>) {
        for _ in 0..20 {
            if to_a.is_empty() && to_b.is_empty() {
                return;
            }
            let mut next_a = Vec::new();
            let mut next_b = Vec::new();
            for m in to_b.drain(..) {
                next_a.extend(b.receive(&with_from(m, a.client_id)));
            }
            for m in to_a.drain(..) {
                next_b.extend(a.receive(&with_from(m, b.client_id)));
            }
            to_a = next_a;
            to_b = next_b;
        }
    }

    fn with_from(mut m: Json, from: u64) -> Json {
        if m.get("method").is_some() && m["method"] != "collab/joined" {
            m["params"]["from"] = json!(from);
        }
        m
    }

    #[test]
    fn two_apps_converge_encrypted() {
        let key = b64::encode(&[5u8; 32]);
        let mut a: RefApp = RefApp::new(Some(1));
        a.load(&Model::sheet("T", "Sheet1", &[("A1", "x")]));
        let mut b: RefApp = RefApp::new(Some(2));
        let ja = a.join(&json!({"room": "r", "mode": "share", "user": {"id": "a", "name": "A", "color": "#000000"}, "key": key})).unwrap();
        let jb = b.join(&json!({"room": "r", "mode": "join", "user": {"id": "b", "name": "B", "color": "#000000"}, "key": key})).unwrap();
        assert!(b.joining);
        exchange(&mut a, &mut b, ja.into_iter().skip(1).collect(), jb.into_iter().skip(1).collect());
        assert!(!b.joining);
        assert_eq!(a.model().unwrap(), b.model().unwrap());
        let ua = a.run("cell.set", &json!({"cell": "B2", "input": "=A1"})).unwrap();
        let ub = b.run("home.insertRows", &json!({"rows": "1:1"})).unwrap();
        exchange(&mut a, &mut b, ua, ub);
        let m = a.model().unwrap();
        assert_eq!(m, b.model().unwrap());
        assert_eq!(m.sheets[0].cells.get("A2").map(String::as_str), Some("x"));
        assert_eq!(m.sheets[0].cells.get("B3").map(String::as_str), Some("=A1"));
        assert_eq!(a.decrypt_failures + b.decrypt_failures, 0);
    }
}
