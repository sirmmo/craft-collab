//! One connection to the local relay per room, on its own thread, reconnecting (and starting a
//! relay when there's none) with a short backoff.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_collab_kit::b64;
use craft_collab_kit::collab::{Joined, MAX_CLIENT_ID};
use craft_collab_kit::frame::Msg;
use craft_collab_kit::runtime::{Link, NetEvent, NetSink};
use serde_json::{Value as Json, json};

use crate::relay;

type Writer = Arc<Mutex<Option<TcpStream>>>;

/// A room's connection to the relay.
pub struct LoopLink {
    writer: Writer,
    stop: Arc<AtomicBool>,
    room: String,
    me: u64,
    presence: Json,
    port: u16,
}

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// Connects to the relay, starting one in this process when nobody listens.
fn connect_or_host(port: u16) -> Result<TcpStream, String> {
    if let Ok(s) = TcpStream::connect_timeout(&addr(port), Duration::from_secs(1)) {
        return Ok(s);
    }
    if let Ok(listener) = TcpListener::bind(addr(port))
        && !relay::spawn(listener)
    {
        return Err("can't start the local relay".into());
    }
    TcpStream::connect_timeout(&addr(port), Duration::from_secs(1)).map_err(|e| format!("can't reach the local relay on 127.0.0.1:{port}: {e}"))
}

fn event(msg: &Json) -> Option<NetEvent> {
    let data = || msg.get("data").and_then(Json::as_str).and_then(b64::decode);
    let client = || msg.get("client").and_then(Json::as_u64).filter(|c| *c < MAX_CLIENT_ID);
    match msg.get("op").and_then(Json::as_str)? {
        "msg" => Some(NetEvent::Message { from: client()?, msg: Msg::decode(&data()?)? }),
        "snapshot" => Some(NetEvent::Snapshot(data()?)),
        "presence" => Some(NetEvent::Presence { client: client()?, info: msg.get("info").cloned().filter(Json::is_object) }),
        _ => None,
    }
}

fn nap(d: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(50);
    let mut left = d;
    while !left.is_zero() && !stop.load(Ordering::Relaxed) {
        let s = left.min(step);
        std::thread::sleep(s);
        left = left.saturating_sub(s);
    }
}

fn net_loop(port: u16, writer: Writer, sink: NetSink, stop: Arc<AtomicBool>) {
    let mut delay = Duration::from_millis(200);
    while !stop.load(Ordering::Relaxed) {
        match connect_or_host(port) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                let Ok(w) = stream.try_clone() else { continue };
                *writer.lock().unwrap_or_else(|p| p.into_inner()) = Some(w);
                delay = Duration::from_millis(200);
                if !sink.send(NetEvent::Connected) {
                    return;
                }
                let mut reader = BufReader::new(stream);
                loop {
                    let mut line = Vec::new();
                    match (&mut reader).take(relay::MAX_LINE as u64).read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let Ok(msg) = serde_json::from_slice::<Json>(&line) else { continue };
                    if let Some(ev) = event(&msg)
                        && !sink.send(ev)
                    {
                        return;
                    }
                }
                *writer.lock().unwrap_or_else(|p| p.into_inner()) = None;
                if stop.load(Ordering::Relaxed) || !sink.send(NetEvent::Disconnected("The local relay stopped; reconnecting…".into())) {
                    return;
                }
            }
            Err(e) => {
                if !sink.send(NetEvent::Disconnected(e)) {
                    return;
                }
            }
        }
        nap(delay, &stop);
        delay = (delay * 2).min(Duration::from_secs(2));
    }
}

impl LoopLink {
    pub fn start(port: u16, joined: &Joined, sink: NetSink) -> Result<LoopLink, String> {
        let writer: Writer = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (w, s) = (writer.clone(), stop.clone());
        std::thread::Builder::new().name("loopback".into()).spawn(move || net_loop(port, w, sink, s)).map_err(|e| format!("can't start a thread: {e}"))?;
        let presence = json!({"user": joined.user, "app": joined.app, "schema": joined.schema, "encrypted": joined.encrypted});
        Ok(LoopLink { writer, stop, room: joined.room.clone(), me: joined.client_id, presence, port })
    }

    fn write(&self, msg: &Json) -> Result<(), String> {
        let mut guard = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        let Some(stream) = guard.as_mut() else { return Err("not connected to the local relay".into()) };
        let r = writeln!(stream, "{msg}").and_then(|_| stream.flush());
        if let Err(e) = r {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            *guard = None;
            return Err(e.to_string());
        }
        Ok(())
    }
}

impl Link for LoopLink {
    fn connected(&mut self) -> Result<(), String> {
        self.write(&json!({"op": "hello", "room": self.room, "client": self.me, "presence": self.presence}))
    }

    fn send(&mut self, msg: &Msg) -> Result<(), String> {
        self.write(&json!({"op": "msg", "data": b64::encode(&msg.encode())}))
    }

    fn store_snapshot(&mut self, data: &[u8]) -> Result<(), String> {
        self.write(&json!({"op": "snapshot", "data": b64::encode(data)}))
    }

    fn close(&mut self) {
        if self.stop.swap(true, Ordering::Relaxed) {
            return;
        }
        let _ = self.write(&json!({"op": "msg", "data": b64::encode(&Msg::Leave.encode())}));
        let _ = self.write(&json!({"op": "bye"}));
        if let Some(s) = self.writer.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    fn describe(&self) -> String {
        format!("local relay on 127.0.0.1:{}", self.port)
    }
}

impl Drop for LoopLink {
    fn drop(&mut self) {
        self.close();
    }
}
