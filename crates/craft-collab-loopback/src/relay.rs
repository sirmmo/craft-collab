//! The relay: JSON lines over TCP on 127.0.0.1.
//!
//! Client → relay: `{"op":"hello","room","client","presence"}` (first), `{"op":"msg","data"}`
//! (a message in the MQTT binding's framing, base64), `{"op":"snapshot","data"}`, `{"op":"bye"}`.
//!
//! Relay → client: `{"op":"msg","client","data"}`, `{"op":"snapshot","data"}`,
//! `{"op":"presence","client","info"}` (`info` null when the client left).
//!
//! On hello the relay sends the room's retained snapshot and everyone's presence; a client that
//! disconnects without `bye` gets its presence cleared, like an MQTT will.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

use craft_collab_kit::b64;
use craft_collab_kit::collab::{MAX_CLIENT_ID, MAX_UPDATE, valid_room};
use serde_json::{Value as Json, json};

/// Longest line: a 16 MiB update in base64, with room to spare.
pub const MAX_LINE: usize = 32 << 20;
/// Rooms whose snapshot the relay keeps.
const MAX_ROOMS: usize = 256;
/// Connections at once.
const MAX_CONNS: usize = 1024;

struct Conn {
    room: Option<String>,
    client: u64,
    tx: Sender<String>,
}

#[derive(Default)]
struct State {
    next: u64,
    conns: HashMap<u64, Conn>,
    snapshots: HashMap<String, String>,
    presence: HashMap<String, BTreeMap<u64, Json>>,
}

type Shared = Arc<Mutex<State>>;

fn lock(s: &Shared) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

impl State {
    /// Sends a line to every connection in `room` but `except`.
    fn broadcast(&self, room: &str, except: u64, line: &str) {
        for (id, c) in &self.conns {
            if *id != except && c.room.as_deref() == Some(room) {
                let _ = c.tx.send(line.to_string());
            }
        }
    }
}

/// Accepts connections until the listener fails. Each connection gets a reader (this function's
/// spawned thread) and a writer thread, so a slow client never holds up the others.
pub fn serve(listener: TcpListener) {
    let state: Shared = Arc::new(Mutex::new(State::default()));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if lock(&state).conns.len() >= MAX_CONNS {
            continue;
        }
        let st = state.clone();
        let _ = std::thread::Builder::new().name("relay conn".into()).spawn(move || handle(stream, st));
    }
}

/// Starts the relay in the background.
pub fn spawn(listener: TcpListener) -> bool {
    std::thread::Builder::new().name("relay".into()).spawn(move || serve(listener)).is_ok()
}

fn handle(stream: TcpStream, state: Shared) {
    let _ = stream.set_nodelay(true);
    let Ok(mut write_half) = stream.try_clone() else { return };
    let (tx, rx) = mpsc::channel::<String>();
    let writer = std::thread::Builder::new().name("relay writer".into()).spawn(move || {
        for line in rx {
            if writeln!(write_half, "{line}").and_then(|_| write_half.flush()).is_err() {
                break;
            }
        }
        let _ = write_half.shutdown(std::net::Shutdown::Both);
    });
    if writer.is_err() {
        return;
    }
    let id = {
        let mut s = lock(&state);
        s.next += 1;
        let id = s.next;
        s.conns.insert(id, Conn { room: None, client: 0, tx });
        id
    };
    let mut reader = BufReader::new(stream);
    let mut graceful = false;
    loop {
        let mut line = Vec::new();
        match (&mut reader).take(MAX_LINE as u64).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.last() != Some(&b'\n') && line.len() >= MAX_LINE {
            break;
        }
        let Ok(msg) = serde_json::from_slice::<Json>(&line) else { continue };
        if msg.get("op").and_then(Json::as_str) == Some("bye") {
            graceful = true;
            break;
        }
        on_line(&state, id, &msg);
    }
    let mut s = lock(&state);
    if let Some(c) = s.conns.remove(&id)
        && let Some(room) = c.room
    {
        let had = s.presence.get_mut(&room).and_then(|p| p.remove(&c.client)).is_some();
        if had || !graceful {
            s.broadcast(&room, id, &json!({"op": "presence", "client": c.client, "info": null}).to_string());
        }
        if s.presence.get(&room).is_some_and(BTreeMap::is_empty) {
            s.presence.remove(&room);
        }
    }
}

fn on_line(state: &Shared, id: u64, msg: &Json) {
    let op = msg.get("op").and_then(Json::as_str).unwrap_or("");
    let data = || msg.get("data").and_then(Json::as_str).filter(|d| d.len() <= MAX_UPDATE / 3 * 4 + 16 && b64::decode(d).is_some());
    let mut s = lock(state);
    match op {
        "hello" => {
            let Some(room) = msg.get("room").and_then(Json::as_str).filter(|r| valid_room(r)).map(str::to_string) else { return };
            let Some(client) = msg.get("client").and_then(Json::as_u64).filter(|c| *c < MAX_CLIENT_ID) else { return };
            let presence = msg.get("presence").cloned().filter(Json::is_object);
            let Some(conn) = s.conns.get_mut(&id) else { return };
            conn.room = Some(room.clone());
            conn.client = client;
            let tx = conn.tx.clone();
            if let Some(snap) = s.snapshots.get(&room) {
                let _ = tx.send(json!({"op": "snapshot", "data": snap}).to_string());
            }
            for (c, info) in s.presence.get(&room).into_iter().flatten() {
                if *c != client {
                    let _ = tx.send(json!({"op": "presence", "client": c, "info": info}).to_string());
                }
            }
            if let Some(p) = presence {
                if s.presence.len() < MAX_ROOMS || s.presence.contains_key(&room) {
                    s.presence.entry(room.clone()).or_default().insert(client, p.clone());
                }
                s.broadcast(&room, id, &json!({"op": "presence", "client": client, "info": p}).to_string());
            }
        }
        "msg" => {
            let Some(d) = data() else { return };
            let Some((room, client)) = s.conns.get(&id).and_then(|c| Some((c.room.clone()?, c.client))) else { return };
            s.broadcast(&room, id, &json!({"op": "msg", "client": client, "data": d}).to_string());
        }
        "snapshot" => {
            let Some(d) = data().map(str::to_string) else { return };
            let Some(room) = s.conns.get(&id).and_then(|c| c.room.clone()) else { return };
            if s.snapshots.len() >= MAX_ROOMS && !s.snapshots.contains_key(&room) {
                return;
            }
            s.broadcast(&room, id, &json!({"op": "snapshot", "data": d}).to_string());
            s.snapshots.insert(room, d);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn client(port: u16) -> (TcpStream, BufReader<TcpStream>) {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let r = BufReader::new(s.try_clone().unwrap());
        (s, r)
    }

    fn read(r: &mut BufReader<TcpStream>) -> Json {
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        serde_json::from_str(&l).unwrap()
    }

    #[test]
    fn fans_out_retains_and_wills() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(spawn(l));
        let (mut a, mut ar) = client(port);
        writeln!(a, r#"{{"op":"hello","room":"loopback:r","client":1,"presence":{{"user":"a"}}}}"#).unwrap();
        writeln!(a, r#"{{"op":"snapshot","data":"AQI="}}"#).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let (mut b, mut br) = client(port);
        writeln!(b, r#"{{"op":"hello","room":"loopback:r","client":2,"presence":{{"user":"b"}}}}"#).unwrap();
        assert_eq!(read(&mut br), json!({"op": "snapshot", "data": "AQI="}));
        assert_eq!(read(&mut br), json!({"op": "presence", "client": 1, "info": {"user": "a"}}));
        assert_eq!(read(&mut ar), json!({"op": "presence", "client": 2, "info": {"user": "b"}}));
        writeln!(b, r#"{{"op":"msg","data":"AgM="}}"#).unwrap();
        assert_eq!(read(&mut ar), json!({"op": "msg", "client": 2, "data": "AgM="}));
        drop(br);
        b.shutdown(std::net::Shutdown::Both).unwrap();
        assert_eq!(read(&mut ar), json!({"op": "presence", "client": 2, "info": null}));
    }
}
