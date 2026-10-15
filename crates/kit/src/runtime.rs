//! The push channel (protocol 2, section 1) and the rooms a transport holds.
//!
//! One thread reads the app's lines; network threads (one per room, owned by the transport's
//! [`Link`]) report [`NetEvent`]s; the main loop owns standard output, so a plugin writes one
//! whole line at a time and may write at any moment. The app's requests are answered in order;
//! answers to the plugin's own requests (`collab/snapshot`) are matched by id.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};

use crate::b64;
use crate::collab::{self, FromApp, Joined, MAX_ROOMS, MAX_UPDATE, put};
use crate::frame::Msg;

/// Longest line read from the app (section 1).
pub const MAX_LINE: usize = 64 << 20;
/// Most peers listed for one room.
const MAX_PEERS: usize = 1000;

/// What a room's network reports to the main loop.
#[derive(Debug)]
pub enum NetEvent {
    /// Connected (or reconnected): the room is online once [`Link::connected`] succeeds.
    Connected,
    /// The connection dropped or couldn't be made; the link keeps retrying.
    Disconnected(String),
    /// A message from another client.
    Message { from: u64, msg: Msg },
    /// The room's stored snapshot (Appendix A), or a newer one someone stored.
    Snapshot(Vec<u8>),
    /// A client's presence (`{user, app, schema, encrypted}`), or `None` when it left.
    Presence { client: u64, info: Option<Json> },
}

/// What the main loop waits for.
#[derive(Debug)]
pub enum Event {
    App(Json),
    AppClosed,
    Net { room: String, epoch: u64, ev: NetEvent },
}

/// Where a room's network thread sends its events.
#[derive(Clone, Debug)]
pub struct NetSink {
    tx: Sender<Event>,
    room: String,
    epoch: u64,
}

impl NetSink {
    /// `false` once the main loop is gone (the network thread should stop).
    pub fn send(&self, ev: NetEvent) -> bool {
        self.tx.send(Event::Net { room: self.room.clone(), epoch: self.epoch, ev }).is_ok()
    }

    pub fn room(&self) -> &str {
        &self.room
    }
}

/// A room's connection to the others, made by the transport.
pub trait Link: Send {
    /// Called on the main thread after [`NetEvent::Connected`]: subscribe, announce presence.
    fn connected(&mut self) -> Result<(), String>;
    /// Sends one message to the others.
    fn send(&mut self, msg: &Msg) -> Result<(), String>;
    /// Replaces the room's stored snapshot.
    fn store_snapshot(&mut self, data: &[u8]) -> Result<(), String>;
    /// Says goodbye (leave message, presence cleared) and disconnects. Never blocks for long.
    fn close(&mut self);
    /// Where the room lives, for the status dialog (`broker.example:8883, TLS`).
    fn describe(&self) -> String;
}

/// What [`Plugin::open`] returns.
pub struct Opened {
    pub link: Box<dyn Link>,
    /// Ask the app for a snapshot as soon as the room is online (the person who shared it), so
    /// the room has a stored state from the start.
    pub seed: bool,
}

/// A request's error: (code, message).
pub type Fail = (i64, String);

pub fn fail(message: impl Into<String>) -> Fail {
    (1, message.into())
}

/// A transport.
pub trait Plugin {
    /// For logs (`craft-mqtt`).
    fn name(&self) -> &'static str;
    /// `commands/run`.
    fn command(&mut self, cx: &mut Cx, command: &str, params: &Json, context: &Json) -> Result<Json, Fail>;
    /// The app joined a room (`collab/joined`): connect it. An error makes the transport send
    /// `collab/leave` with it as the reason.
    fn open(&mut self, joined: &Joined, sink: NetSink) -> Result<Opened, String>;
}

/// What `initialize` said about the app.
#[derive(Clone, Debug, Default)]
pub struct Session {
    /// `host.id` (`gridcraft`).
    pub app: String,
    /// `host.language`.
    pub language: String,
    /// `host.features`.
    pub features: Vec<String>,
    /// `host.collab`.
    pub collab: Json,
}

impl Session {
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    /// Whether the app speaks protocol 2 co-editing.
    pub fn can_collab(&self) -> bool {
        self.has("push") && self.has("collab")
    }
}

/// When to store snapshots (Appendix A).
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// After this many updates (default 500).
    pub snapshot_every: u32,
    /// After this long with any update (default 10 minutes).
    pub snapshot_interval: Duration,
    /// How long the app may take to answer `collab/snapshot`.
    pub snapshot_timeout: Duration,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning { snapshot_every: 500, snapshot_interval: Duration::from_secs(600), snapshot_timeout: Duration::from_secs(120) }
    }
}

impl Tuning {
    /// The defaults, with `CRAFT_COLLAB_SNAPSHOT_EVERY` (updates) and `CRAFT_COLLAB_SNAPSHOT_SECS`
    /// (seconds) for tests and debugging.
    pub fn from_env() -> Tuning {
        let mut t = Tuning::default();
        let num = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<u64>().ok());
        if let Some(n) = num("CRAFT_COLLAB_SNAPSHOT_EVERY").filter(|n| (1..=1_000_000).contains(n)) {
            t.snapshot_every = u32::try_from(n).unwrap_or(500);
        }
        if let Some(s) = num("CRAFT_COLLAB_SNAPSHOT_SECS").filter(|s| (1..=86_400).contains(s)) {
            t.snapshot_interval = Duration::from_secs(s);
        }
        t
    }
}

/// A room's state (`collab/status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Connecting,
    Online,
    Offline,
    Ended,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Connecting => "connecting",
            State::Online => "online",
            State::Offline => "offline",
            State::Ended => "ended",
        }
    }
}

struct Room {
    joined: Joined,
    link: Box<dyn Link>,
    epoch: u64,
    state: State,
    message: Option<String>,
    /// Presence of the others: client id → `{user, app, schema, encrypted}`.
    peers: BTreeMap<u64, Json>,
    seed: bool,
    ever_online: bool,
    /// Updates since the last stored snapshot.
    updates: u32,
    last_snapshot: Instant,
    snapshot_asked: Option<u64>,
    retry_at: Option<Instant>,
    /// Fingerprint of the snapshot this client stored, so its echo isn't sent back to the app.
    own_snapshot: Option<u64>,
    /// An update couldn't be sent: make the app sync again.
    lost: bool,
}

/// A room as the status dialog shows it.
#[derive(Clone, Debug)]
pub struct RoomInfo {
    pub room: String,
    pub state: State,
    pub message: Option<String>,
    pub user: Json,
    /// The others' users.
    pub peers: Vec<Json>,
    pub encrypted: bool,
    pub place: String,
}

enum Pending {
    Snapshot { room: String, epoch: u64, at: Instant },
}

/// The main loop's state, handed to the plugin's commands.
pub struct Cx {
    out: Box<dyn Write>,
    /// What `initialize` said.
    pub session: Session,
    rooms: BTreeMap<String, Room>,
    pending: HashMap<u64, Pending>,
    next_id: u64,
    tx: Sender<Event>,
    epoch: u64,
    tuning: Tuning,
    name: &'static str,
    broken: bool,
}

/// FNV-1a of the bytes.
fn fingerprint(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^ (data.len() as u64)
}

impl Cx {
    fn new(out: Box<dyn Write>, tx: Sender<Event>, name: &'static str, tuning: Tuning) -> Cx {
        Cx { out, session: Session::default(), rooms: BTreeMap::new(), pending: HashMap::new(), next_id: 1, tx, epoch: 0, tuning, name, broken: false }
    }

    fn write(&mut self, msg: &Json) {
        if self.broken {
            return;
        }
        if writeln!(self.out, "{msg}").and_then(|_| self.out.flush()).is_err() {
            self.broken = true;
        }
    }

    /// Sends the app a notification.
    pub fn notify(&mut self, method: &str, params: Json) {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Writes to standard error, which the app logs.
    pub fn log(&self, text: &str) {
        eprintln!("{}: {text}", self.name);
    }

    /// The rooms held now.
    pub fn rooms(&self) -> Vec<RoomInfo> {
        self.rooms
            .values()
            .map(|r| RoomInfo {
                room: r.joined.room.clone(),
                state: r.state,
                message: r.message.clone(),
                user: r.joined.user.clone(),
                peers: r.peers.values().filter_map(|p| p.get("user").cloned()).collect(),
                encrypted: r.joined.encrypted,
                place: r.link.describe(),
            })
            .collect()
    }

    pub fn holds(&self, room: &str) -> bool {
        self.rooms.contains_key(room)
    }

    /// Leaves a room from the transport's side: says goodbye to the others and tells the app
    /// (`collab/leave`). `false` when the room isn't held.
    pub fn leave(&mut self, room: &str, reason: &str) -> bool {
        let Some(mut r) = self.rooms.remove(room) else { return false };
        r.link.close();
        self.notify("collab/leave", json!({"room": room, "reason": reason}));
        true
    }

    fn push_status(&mut self, room: &str) {
        let Some(r) = self.rooms.get(room) else { return };
        let peers: Vec<Json> = r.peers.iter().filter_map(|(c, p)| p.get("user").map(|u| json!({"clientId": c, "user": u}))).collect();
        let mut params = json!({"room": room, "state": r.state.as_str(), "peers": peers});
        if let Some(m) = &r.message {
            put(&mut params, "message", json!(m));
        }
        self.notify("collab/status", params);
    }

    fn ask_snapshot(&mut self, room: &str) {
        let id = self.next_id;
        let Some(r) = self.rooms.get_mut(room) else { return };
        if r.snapshot_asked.is_some() {
            return;
        }
        self.next_id += 1;
        r.snapshot_asked = Some(id);
        let epoch = r.epoch;
        self.pending.insert(id, Pending::Snapshot { room: room.to_string(), epoch, at: Instant::now() });
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": "collab/snapshot", "params": {"room": room}}));
    }

    fn join<P: Plugin>(&mut self, plugin: &mut P, j: Joined) {
        if let Some(mut old) = self.rooms.remove(&j.room) {
            old.link.close();
        }
        let room = j.room.clone();
        if self.rooms.len() >= MAX_ROOMS {
            let reason = format!("{} is already in {MAX_ROOMS} live sessions; leave one first", self.name);
            self.notify("collab/leave", json!({"room": room, "reason": reason}));
            return;
        }
        self.epoch += 1;
        let sink = NetSink { tx: self.tx.clone(), room: room.clone(), epoch: self.epoch };
        match plugin.open(&j, sink) {
            Ok(opened) => {
                let r = Room {
                    joined: j,
                    link: opened.link,
                    epoch: self.epoch,
                    state: State::Connecting,
                    message: None,
                    peers: BTreeMap::new(),
                    seed: opened.seed,
                    ever_online: false,
                    updates: 0,
                    last_snapshot: Instant::now(),
                    snapshot_asked: None,
                    retry_at: None,
                    own_snapshot: None,
                    lost: false,
                };
                self.rooms.insert(room.clone(), r);
                self.push_status(&room);
            }
            Err(e) => {
                self.log(&format!("can't open {room}: {e}"));
                self.notify("collab/leave", json!({"room": room, "reason": e}));
            }
        }
    }

    fn on_collab<P: Plugin>(&mut self, plugin: &mut P, method: &str, params: &Json) {
        let parsed = match collab::parse(method, params) {
            Ok(p) => p,
            Err(e) => {
                self.log(&format!("dropped {method}: {e}"));
                return;
            }
        };
        let room = parsed.room().to_string();
        let msg = match parsed {
            FromApp::Joined(j) => return self.join(plugin, j),
            FromApp::Leave { .. } => {
                if let Some(mut r) = self.rooms.remove(&room) {
                    r.link.close();
                }
                return;
            }
            FromApp::Sync1 { state_vector, .. } => Msg::Sync1(state_vector),
            FromApp::Sync2 { update, to, .. } => Msg::Sync2 { to: to.unwrap_or(0), update },
            FromApp::Update { update, .. } => Msg::Update(update),
            FromApp::Awareness { update, .. } => Msg::Awareness(update),
        };
        let Some(r) = self.rooms.get_mut(&room) else {
            self.log(&format!("dropped {method} for {room}, a room it isn't in"));
            return;
        };
        if matches!(msg, Msg::Update(_)) {
            r.updates = r.updates.saturating_add(1);
        }
        if let Err(e) = r.link.send(&msg) {
            if !matches!(msg, Msg::Awareness(_)) {
                r.lost = true;
            }
            self.log(&format!("couldn't send {method} to {room}: {e}"));
        }
    }

    fn on_answer(&mut self, id: &Json, msg: &Json) {
        let Some(id) = id.as_u64() else { return };
        let Some(Pending::Snapshot { room, epoch, .. }) = self.pending.remove(&id) else { return };
        let Some(r) = self.rooms.get_mut(&room) else { return };
        if r.epoch != epoch {
            return;
        }
        r.snapshot_asked = None;
        let update =
            msg.get("result").and_then(|r| r.get("update")).and_then(Json::as_str).and_then(b64::decode).filter(|u| !u.is_empty() && u.len() <= MAX_UPDATE);
        let Some(update) = update else {
            r.retry_at = Some(Instant::now() + Duration::from_secs(60));
            let why = msg.get("error").and_then(|e| e.get("message")).and_then(Json::as_str).unwrap_or("no update in the answer").to_string();
            self.log(&format!("the app didn't give a snapshot of {room}: {why}"));
            return;
        };
        match r.link.store_snapshot(&update) {
            Ok(()) => {
                r.own_snapshot = Some(fingerprint(&update));
                r.updates = 0;
                r.last_snapshot = Instant::now();
                r.retry_at = None;
            }
            Err(e) => {
                r.retry_at = Some(Instant::now() + Duration::from_secs(60));
                self.log(&format!("couldn't store the snapshot of {room}: {e}"));
            }
        }
    }

    fn on_net(&mut self, room: &str, epoch: u64, ev: NetEvent) {
        let Some(r) = self.rooms.get_mut(room) else { return };
        if r.epoch != epoch {
            return;
        }
        let me = r.joined.client_id;
        let enc = |b: &[u8]| b64::encode(b);
        match ev {
            NetEvent::Connected => {
                match r.link.connected() {
                    Ok(()) => {
                        r.state = State::Online;
                        r.message = None;
                        r.lost = false;
                    }
                    Err(e) => {
                        r.state = State::Offline;
                        r.message = Some(e);
                    }
                }
                let seed = r.state == State::Online && r.seed && !r.ever_online;
                if r.state == State::Online {
                    r.ever_online = true;
                }
                self.push_status(room);
                if seed {
                    self.ask_snapshot(room);
                }
            }
            NetEvent::Disconnected(m) => {
                if r.state != State::Offline || r.message.as_deref() != Some(m.as_str()) {
                    r.state = State::Offline;
                    r.message = Some(m);
                    self.push_status(room);
                }
            }
            NetEvent::Message { from, msg } => {
                if from == me {
                    return;
                }
                match msg {
                    Msg::Sync1(sv) => self.notify("collab/sync1", json!({"room": room, "stateVector": enc(&sv), "from": from})),
                    Msg::Sync2 { to, update } => {
                        if to == 0 || to == me {
                            self.notify("collab/sync2", json!({"room": room, "update": enc(&update), "from": from}));
                        }
                    }
                    Msg::Update(u) => {
                        r.updates = r.updates.saturating_add(1);
                        self.notify("collab/update", json!({"room": room, "update": enc(&u), "from": from}));
                    }
                    Msg::Awareness(u) => self.notify("collab/awareness", json!({"room": room, "update": enc(&u), "from": from})),
                    Msg::Leave => {
                        if r.peers.remove(&from).is_some() {
                            self.push_status(room);
                        }
                    }
                }
            }
            NetEvent::Snapshot(data) => {
                if data.is_empty() || data.len() > MAX_UPDATE {
                    return;
                }
                r.updates = 0;
                r.last_snapshot = Instant::now();
                if r.own_snapshot == Some(fingerprint(&data)) {
                    return;
                }
                self.notify("collab/sync2", json!({"room": room, "update": enc(&data), "snapshot": true}));
            }
            NetEvent::Presence { client, info } => {
                if client == me {
                    return;
                }
                let changed = match info.filter(|i| i.get("user").is_some_and(collab::valid_user)) {
                    Some(i) if r.peers.len() < MAX_PEERS || r.peers.contains_key(&client) => r.peers.insert(client, i.clone()).as_ref() != Some(&i),
                    Some(_) => false,
                    None => r.peers.remove(&client).is_some(),
                };
                if changed {
                    self.push_status(room);
                }
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let timeout = self.tuning.snapshot_timeout;
        let mut expired = Vec::new();
        self.pending.retain(|_, p| {
            let Pending::Snapshot { room, at, .. } = p;
            let keep = now.duration_since(*at) < timeout;
            if !keep {
                expired.push(room.clone());
            }
            keep
        });
        for room in expired {
            if let Some(r) = self.rooms.get_mut(&room) {
                r.snapshot_asked = None;
                r.retry_at = Some(now + Duration::from_secs(60));
            }
        }
        let (every, interval) = (self.tuning.snapshot_every, self.tuning.snapshot_interval);
        let mut ask = Vec::new();
        let mut resync = Vec::new();
        for (id, r) in &mut self.rooms {
            if r.state != State::Online {
                continue;
            }
            if r.lost {
                r.lost = false;
                resync.push(id.clone());
            }
            let due = r.updates >= every || (r.updates > 0 && now.duration_since(r.last_snapshot) >= interval);
            if due && r.snapshot_asked.is_none() && r.retry_at.is_none_or(|t| now >= t) {
                ask.push(id.clone());
            }
        }
        for room in resync {
            // offline, then online: the app sends sync1 again and the peers fill the gap.
            if let Some(r) = self.rooms.get_mut(&room) {
                r.state = State::Offline;
                r.message = Some("Catching up…".into());
            }
            self.push_status(&room);
            if let Some(r) = self.rooms.get_mut(&room) {
                r.state = State::Online;
                r.message = None;
            }
            self.push_status(&room);
        }
        for room in ask {
            self.ask_snapshot(&room);
        }
    }

    fn close_all(&mut self) {
        for (_, mut r) in std::mem::take(&mut self.rooms) {
            r.link.close();
        }
    }

    fn initialize(&mut self, params: &Json) -> Json {
        let host = params.get("host");
        let text = |k: &str| host.and_then(|h| h.get(k)).and_then(Json::as_str).map(|s| s.chars().take(64).collect::<String>());
        if let Some(id) = text("id") {
            self.session.app = id.to_ascii_lowercase();
        }
        if let Some(l) = text("language") {
            self.session.language = l;
        }
        self.session.features = host
            .and_then(|h| h.get("features"))
            .and_then(Json::as_array)
            .map(|a| a.iter().filter_map(Json::as_str).take(64).map(|x| x.chars().take(64).collect()).collect())
            .unwrap_or_default();
        self.session.collab = host.and_then(|h| h.get("collab")).cloned().unwrap_or(Json::Null);
        json!({"protocolVersion": 1, "features": ["push", "collab"]})
    }

    fn request<P: Plugin>(&mut self, plugin: &mut P, method: &str, params: &Json) -> Result<Json, Fail> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "commands/run" => {
                let command = params.get("command").and_then(Json::as_str).unwrap_or("").to_string();
                let p = params.get("params").cloned().filter(Json::is_object).unwrap_or_else(|| json!({}));
                let ctx = params.get("context").cloned().unwrap_or(Json::Null);
                plugin.command(self, &command, &p, &ctx)
            }
            _ => Err((-32601, format!("{} has no method {method}", self.name))),
        }
    }

    /// One message from the app; `false` to stop.
    fn on_app<P: Plugin>(&mut self, plugin: &mut P, msg: Json) -> bool {
        let method = msg.get("method").and_then(Json::as_str).map(str::to_string);
        let id = msg.get("id").cloned().filter(|v| !v.is_null());
        let params = msg.get("params").cloned().unwrap_or(Json::Null);
        match (method, id) {
            (Some(method), Some(id)) => {
                // A bug here must cost the request, never the plugin.
                let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.request(plugin, &method, &params)))
                    .unwrap_or_else(|_| Err(fail("the transport hit an internal error")));
                let reply = match answer {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
                };
                self.write(&reply);
            }
            (Some(method), None) => {
                if method == "shutdown" || method == "exit" {
                    return false;
                }
                if method.starts_with("collab/") {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.on_collab(plugin, &method, &params)));
                    if r.is_err() {
                        self.log(&format!("internal error on {method}"));
                    }
                }
            }
            (None, Some(id)) => self.on_answer(&id, &msg),
            (None, None) => self.log("ignored a message with neither method nor id"),
        }
        true
    }
}

/// Reads the app's lines into the main loop until its stream closes.
fn read_lines<R: Read>(input: R, tx: Sender<Event>, name: &'static str) {
    let mut reader = BufReader::with_capacity(1 << 16, input);
    loop {
        let mut line = Vec::new();
        let limit = MAX_LINE as u64 + 1;
        match (&mut reader).take(limit).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.len() > MAX_LINE && line.last() != Some(&b'\n') {
            eprintln!("{name}: dropped a line over {} MiB", MAX_LINE >> 20);
            // Skip the rest of it.
            loop {
                let buf = match reader.fill_buf() {
                    Ok([]) | Err(_) => break,
                    Ok(b) => b,
                };
                match buf.iter().position(|b| *b == b'\n') {
                    Some(i) => {
                        reader.consume(i + 1);
                        break;
                    }
                    None => {
                        let n = buf.len();
                        reader.consume(n);
                    }
                }
            }
            continue;
        }
        match serde_json::from_slice::<Json>(&line) {
            Ok(msg) if msg.is_object() => {
                if tx.send(Event::App(msg)).is_err() {
                    return;
                }
            }
            _ => {
                if !String::from_utf8_lossy(&line).trim().is_empty() {
                    eprintln!("{name}: ignored a line that isn't a JSON object");
                }
            }
        }
    }
    let _ = tx.send(Event::AppClosed);
}

/// Serves the app until its stream closes or it says `shutdown`.
pub fn serve<P: Plugin, R: Read + Send + 'static, W: Write + 'static>(mut plugin: P, input: R, output: W) {
    let (tx, rx) = mpsc::channel();
    let reader_tx = tx.clone();
    let name = plugin.name();
    let spawned = std::thread::Builder::new().name("app-reader".into()).spawn(move || read_lines(input, reader_tx, name));
    if spawned.is_err() {
        eprintln!("{name}: can't start a thread");
        return;
    }
    let mut cx = Cx::new(Box::new(output), tx, name, Tuning::from_env());
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Event::App(msg)) => {
                if !cx.on_app(&mut plugin, msg) {
                    break;
                }
            }
            Ok(Event::AppClosed) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Event::Net { room, epoch, ev }) => cx.on_net(&room, epoch, ev),
            Err(RecvTimeoutError::Timeout) => {}
        }
        cx.tick();
        if cx.broken {
            break;
        }
    }
    cx.close_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A link that records what it's asked to do.
    struct Rec(Arc<Mutex<Vec<String>>>);

    impl Link for Rec {
        fn connected(&mut self) -> Result<(), String> {
            self.0.lock().unwrap().push("connected".into());
            Ok(())
        }
        fn send(&mut self, msg: &Msg) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("send {}", msg.kind()));
            Ok(())
        }
        fn store_snapshot(&mut self, data: &[u8]) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("snapshot {}", data.len()));
            Ok(())
        }
        fn close(&mut self) {
            self.0.lock().unwrap().push("close".into());
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }

    struct P {
        log: Arc<Mutex<Vec<String>>>,
        sink: Arc<Mutex<Option<NetSink>>>,
    }

    impl Plugin for P {
        fn name(&self) -> &'static str {
            "test"
        }
        fn command(&mut self, cx: &mut Cx, command: &str, _: &Json, _: &Json) -> Result<Json, Fail> {
            match command {
                "rooms" => Ok(json!(cx.rooms().len())),
                "boom" => Err(fail("no")),
                _ => Err(fail("unknown")),
            }
        }
        fn open(&mut self, _: &Joined, sink: NetSink) -> Result<Opened, String> {
            *self.sink.lock().unwrap() = Some(sink);
            Ok(Opened { link: Box::new(Rec(self.log.clone())), seed: true })
        }
    }

    #[derive(Clone, Default)]
    struct Out(Arc<Mutex<Vec<u8>>>);
    impl Write for Out {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn lines(out: &Out) -> Vec<Json> {
        String::from_utf8(out.0.lock().unwrap().clone()).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn relays_and_compacts() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::new(Mutex::new(None));
        let mut p = P { log: log.clone(), sink: sink.clone() };
        let out = Out::default();
        let (tx, rx) = mpsc::channel();
        let mut cx = Cx::new(Box::new(out.clone()), tx, "test", Tuning { snapshot_every: 2, ..Tuning::default() });
        let user = json!({"id": "u", "name": "Ada", "color": "#112233"});
        assert!(cx.on_app(
            &mut p,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"host": {"id": "GridCraft", "features": ["push", "collab"]}}})
        ));
        assert!(cx.session.can_collab());
        cx.on_app(&mut p, json!({"method": "collab/joined", "params": {"room": "r", "clientId": 5, "user": user, "app": "gridcraft", "schema": 1}}));
        let s = sink.lock().unwrap().clone().unwrap();
        s.send(NetEvent::Connected);
        s.send(NetEvent::Message { from: 9, msg: Msg::Update(vec![1, 2]) });
        s.send(NetEvent::Message { from: 5, msg: Msg::Update(vec![3]) });
        s.send(NetEvent::Message { from: 9, msg: Msg::Sync2 { to: 6, update: vec![3] } });
        s.send(NetEvent::Presence { client: 9, info: Some(json!({"user": user})) });
        while let Ok(Event::Net { room, epoch, ev }) = rx.try_recv() {
            cx.on_net(&room, epoch, ev);
        }
        cx.on_app(&mut p, json!({"method": "collab/update", "params": {"room": "r", "update": "AQI="}}));
        cx.tick();
        let msgs = lines(&out);
        let methods: Vec<&str> = msgs.iter().filter_map(|m| m.get("method").and_then(Json::as_str)).collect();
        assert_eq!(methods, ["collab/status", "collab/status", "collab/snapshot", "collab/update", "collab/status"]);
        let snap_id = msgs.iter().find(|m| m["method"] == "collab/snapshot").unwrap()["id"].clone();
        assert_eq!(msgs[4]["params"]["from"], 9);
        assert_eq!(msgs[5]["params"]["peers"][0]["clientId"], 9);
        cx.on_app(&mut p, json!({"jsonrpc": "2.0", "id": snap_id, "result": {"update": "AQID"}}));
        cx.on_net("r", 1, NetEvent::Snapshot(vec![1, 2, 3]));
        assert_eq!(lines(&out).len(), 6, "the echo of its own snapshot isn't sent back");
        cx.on_net("r", 1, NetEvent::Snapshot(vec![4]));
        assert_eq!(lines(&out).last().unwrap()["params"]["snapshot"], true);
        assert_eq!(*log.lock().unwrap(), ["connected", "send 2", "snapshot 3"]);
        cx.on_app(&mut p, json!({"jsonrpc": "2.0", "id": 2, "method": "commands/run", "params": {"command": "rooms"}}));
        assert_eq!(lines(&out).last().unwrap()["result"], 1);
        assert!(cx.leave("r", "bye"));
        assert_eq!(lines(&out).last().unwrap()["method"], "collab/leave");
        assert!(!cx.on_app(&mut p, json!({"method": "shutdown"})));
    }

    #[test]
    fn reads_lines_and_skips_bad_ones() {
        let (tx, rx) = mpsc::channel();
        let input = b"{\"a\":1}\nnot json\n[1]\n{\"b\":2}".to_vec();
        read_lines(std::io::Cursor::new(input), tx, "test");
        let got: Vec<Event> = rx.try_iter().collect();
        assert_eq!(got.len(), 3);
        assert!(matches!(got.last(), Some(Event::AppClosed)));
    }
}
