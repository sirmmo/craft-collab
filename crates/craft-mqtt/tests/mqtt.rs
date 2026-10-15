//! craft-mqtt against an in-process broker (rumqttd): the conformance kit (plain and encrypted
//! rooms), presence and the will, the retained snapshot as the broker stores it, and reconnecting
//! through a proxy that drops the connection.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use craft_collab_conformance::crypto;
use craft_collab_conformance::gridcraft::Model;
use craft_collab_conformance::harness::{Launch, SimApp, pump};
use craft_collab_conformance::refapp::RefApp;
use craft_collab_conformance::transport::{self, Config};
use craft_collab_kit::b64;
use serde_json::{Value as Json, json};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Starts a broker on a free port.
fn broker() -> u16 {
    let port = free_port();
    let config: rumqttd::Config = serde_json::from_value(json!({
        "id": 0,
        "router": {"max_connections": 1000, "max_outgoing_packet_count": 1000, "max_segment_size": 104857600, "max_segment_count": 10},
        "v4": {"1": {
            "name": "v4-1", "listen": format!("127.0.0.1:{port}"), "next_connection_delay_ms": 1,
            "connections": {"connection_timeout_ms": 60000, "max_payload_size": 20971520, "max_inflight_count": 200, "dynamic_filters": true}
        }}
    }))
    .unwrap();
    std::thread::spawn(move || {
        let mut b = rumqttd::Broker::new(config);
        let _ = b.start();
    });
    let end = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < end, "the broker didn't start");
        std::thread::sleep(Duration::from_millis(20));
    }
    port
}

fn data_dir() -> String {
    let d = std::env::temp_dir().join(format!("craft-mqtt-test-{}", craft_collab_kit::ids::base36(10)));
    std::fs::create_dir_all(&d).unwrap();
    d.display().to_string()
}

fn launch() -> Launch {
    Launch::new(env!("CARGO_BIN_EXE_craft-mqtt")).env("CRAFT_MQTT_DATA", &data_dir()).env("CRAFT_COLLAB_SNAPSHOT_EVERY", "5")
}

fn conformance(encrypted: bool) {
    let port = broker();
    let mut cfg = Config::new(launch());
    cfg.share = json!({"broker": format!("mqtt://127.0.0.1:{port}")});
    cfg.encrypted = encrypted;
    cfg.require_persistence = true;
    let report = transport::run(&cfg);
    print!("{report}");
    assert!(report.passed(), "{:?}", report.failures());
    assert!(!report.checks.iter().any(|c| matches!(c.outcome, transport::Outcome::Warn(_))), "{report}");
}

#[test]
fn conformance_plain() {
    conformance(false);
}

#[test]
fn conformance_encrypted() {
    conformance(true);
}

/// Shares a room on `broker` from a new simulated app; (app, link, join).
fn share(launch: &Launch, broker: &str, encrypt: bool) -> (SimApp, String, Json) {
    let mut a = SimApp::start(launch, "A", None).unwrap();
    let res = a.command("share", json!({"broker": broker, "name": "Ada", "color": "#3B7DD8", "encrypt": encrypt})).unwrap();
    let join = res["collab"]["join"].clone();
    a.app.load(&Model::sheet("", "Sheet1", &[("A1", "Item"), ("B1", "Cost")]));
    a.join(&join).unwrap();
    (a, res["result"]["link"].as_str().unwrap().to_string(), join)
}

fn join(launch: &Launch, link: &str, name: &str) -> SimApp {
    let mut b = SimApp::start(launch, name, None).unwrap();
    let res = b.command("join", json!({"link": link, "name": name, "color": "#2E9E5B"})).unwrap();
    b.join(&res["collab"]["join"]).unwrap();
    b
}

fn same(a: &SimApp, b: &SimApp) -> bool {
    !a.app.joining && !b.app.joining && a.app.model().ok() == b.app.model().ok()
}

#[test]
fn presence_and_will() {
    let port = broker();
    let l = launch();
    let (mut a, link, _) = share(&l, &format!("mqtt://127.0.0.1:{port}"), false);
    let mut b = join(&l, &link, "Grace");
    let b_id = b.app.client_id;
    let sees_b = |x: &[&mut SimApp]| x[0].peers().iter().any(|p| p["clientId"].as_u64() == Some(b_id) && p["user"]["name"] == "Grace");
    assert!(pump(&mut [&mut a, &mut b], Duration::from_secs(10), &mut |x| same(x[0], x[1]) && sees_b(x)), "A never listed B: {:?}", a.peers());
    // B's process dies without a goodbye: the broker publishes its will, which clears its presence.
    b.proc.kill();
    assert!(pump(&mut [&mut a], Duration::from_secs(10), &mut |x| x[0].peers().is_empty()), "B's presence stayed: {:?}", a.peers());
}

#[test]
fn the_broker_stores_ciphertext() {
    let port = broker();
    let l = launch();
    let (mut a, link, join_a) = share(&l, &format!("mqtt://127.0.0.1:{port}"), true);
    let room = join_a["room"].as_str().unwrap().to_string();
    let key: [u8; 32] = b64::decode(join_a["key"].as_str().unwrap()).unwrap().try_into().unwrap();
    assert!(link.starts_with(&format!("craft+mqtt://127.0.0.1:{port}/")) && link.contains("#k="), "{link}");
    assert!(pump(&mut [&mut a], Duration::from_secs(10), &mut |x| x[0].app.state == "online" && !x[0].app.snapshot_models.is_empty()));
    for i in 0..6 {
        a.edit("cell.set", json!({"cell": format!("A{}", i + 2), "input": format!("row {i}")})).unwrap();
    }
    assert!(pump(&mut [&mut a], Duration::from_secs(10), &mut |x| x[0].app.snapshot_models.len() >= 2));
    let expected = a.app.model().unwrap();
    pump(&mut [&mut a], Duration::from_millis(300), &mut |_| false);

    // Look at the broker with a plain MQTT client.
    let mut o = rumqttc::MqttOptions::new("inspector", "127.0.0.1", port);
    o.set_max_packet_size(20 << 20, 20 << 20);
    let (client, mut conn) = rumqttc::Client::new(o, 16);
    let base = format!("craft/v1/{}", craft_collab_kit::frame::topic_segment(&room));
    client.subscribe(format!("{base}/#"), rumqttc::QoS::AtLeastOnce).unwrap();
    let got: Arc<Mutex<Vec<(String, Vec<u8>, bool)>>> = Arc::default();
    let g = got.clone();
    std::thread::spawn(move || {
        for ev in conn.iter() {
            match ev {
                Ok(rumqttc::Event::Incoming(rumqttc::Packet::Publish(p))) => g.lock().unwrap().push((p.topic.clone(), p.payload.to_vec(), p.retain)),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    let end = Instant::now() + Duration::from_secs(10);
    let snapshot = loop {
        if let Some((_, data, retained)) = got.lock().unwrap().iter().find(|(t, _, _)| t.ends_with("/snapshot")).cloned() {
            assert!(retained, "the snapshot is retained");
            break data;
        }
        assert!(Instant::now() < end, "no retained snapshot");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(<RefApp>::model_of(std::slice::from_ref(&snapshot)).is_err(), "the broker can read the snapshot");
    let plain = crypto::open(&key, &room, "collab/snapshot", &snapshot).expect("the snapshot is encrypted for collab/snapshot");
    let snap_model = <RefApp>::model_of(&[plain]).unwrap();
    assert!(a.app.snapshot_models.contains(&snap_model));
    assert_eq!(snap_model, expected);
    // Presence is retained JSON naming the user, without the key.
    let presence = got.lock().unwrap().iter().find(|(t, _, _)| t.contains("/p/")).cloned().expect("presence");
    let p: Json = serde_json::from_slice(&presence.1).unwrap();
    assert_eq!(p["user"]["name"], "Ada");
    assert_eq!(p["encrypted"], true);
    assert!(!String::from_utf8_lossy(&presence.1).contains(join_a["key"].as_str().unwrap()));
    // A live update on m/<clientId>: kind 2, ciphertext.
    a.edit("cell.set", json!({"cell": "C1", "input": "live"})).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let m = got.lock().unwrap().iter().find(|(t, d, _)| t.ends_with(&format!("/m/{}", a.app.client_id)) && d.first() == Some(&2)).cloned();
        if let Some((_, d, retained)) = m {
            assert!(!retained);
            assert!(crypto::open(&key, &room, "collab/update", d.get(1..).unwrap()).is_some());
            break;
        }
        assert!(Instant::now() < end, "no update on the broker");
        a.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A TCP proxy that can drop every connection and refuse new ones for a while.
struct Proxy {
    port: u16,
    down: Arc<AtomicBool>,
    conns: Arc<Mutex<Vec<TcpStream>>>,
}

impl Proxy {
    fn start(upstream: u16) -> Proxy {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let down = Arc::new(AtomicBool::new(false));
        let conns: Arc<Mutex<Vec<TcpStream>>> = Arc::default();
        let (d, c) = (down.clone(), conns.clone());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(s) = s else { continue };
                if d.load(Ordering::SeqCst) {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let Ok(up) = TcpStream::connect(("127.0.0.1", upstream)) else { continue };
                c.lock().unwrap().extend([s.try_clone().unwrap(), up.try_clone().unwrap()]);
                pipe(s.try_clone().unwrap(), up.try_clone().unwrap());
                pipe(up, s);
            }
        });
        Proxy { port, down, conns }
    }

    fn cut(&self) {
        self.down.store(true, Ordering::SeqCst);
        for s in self.conns.lock().unwrap().drain(..) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    fn restore(&self) {
        self.down.store(false, Ordering::SeqCst);
    }
}

fn pipe(mut from: TcpStream, mut to: TcpStream) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 << 10];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = to.shutdown(std::net::Shutdown::Both);
    });
}

#[test]
fn reconnects_and_resyncs() {
    let port = broker();
    let proxy = Proxy::start(port);
    let l = launch();
    // A reaches the broker through the proxy; B directly (both use the same room id).
    let (mut a, link, _) = share(&l, &format!("mqtt://127.0.0.1:{}", proxy.port), false);
    let direct = link.replace(&format!(":{}/", proxy.port), &format!(":{port}/"));
    let mut b = join(&l, &direct, "Grace");
    assert!(pump(&mut [&mut a, &mut b], Duration::from_secs(10), &mut |x| same(x[0], x[1])));

    proxy.cut();
    assert!(pump(&mut [&mut a, &mut b], Duration::from_secs(10), &mut |x| x[0].app.state == "offline"), "A never went offline: {:?}", a.states());
    // Both edit while A is away.
    a.edit("cell.set", json!({"cell": "A5", "input": "offline edit"})).unwrap();
    b.edit("cell.set", json!({"cell": "B5", "input": "meanwhile"})).unwrap();
    pump(&mut [&mut a, &mut b], Duration::from_millis(500), &mut |_| false);
    assert_ne!(a.app.model().unwrap(), b.app.model().unwrap());

    proxy.restore();
    let ok = pump(&mut [&mut a, &mut b], Duration::from_secs(20), &mut |x| x[0].app.state == "online" && same(x[0], x[1]));
    assert!(ok, "after reconnecting: A {:?} B {:?}, A states {:?}", a.app.model(), b.app.model(), a.states());
    let cells = &a.app.model().unwrap().sheets[0].cells;
    assert_eq!(cells.get("A5").map(String::as_str), Some("offline edit"));
    assert_eq!(cells.get("B5").map(String::as_str), Some("meanwhile"));
    let states = a.states();
    let off = states.iter().position(|s| s == "offline").unwrap();
    assert!(states[off..].iter().any(|s| s == "online"), "{states:?}");
}

#[test]
fn rejoins_after_a_restart_from_the_recent_rooms() {
    // A transport that restarted doesn't know the room from a share or join: it finds the broker
    // in rooms.json and takes the app's re-join.
    let port = broker();
    let l = launch();
    let (mut a, _link, join_a) = share(&l, &format!("mqtt://127.0.0.1:{port}"), false);
    assert!(pump(&mut [&mut a], Duration::from_secs(10), &mut |x| x[0].app.state == "online"));
    let mut fresh = SimApp::start(&l, "A2", None).unwrap();
    let mut rejoin = join_a.clone();
    rejoin["mode"] = json!("join");
    fresh.join(&rejoin).unwrap();
    assert!(pump(&mut [&mut a, &mut fresh], Duration::from_secs(10), &mut |x| same(x[0], x[1])));
    // A room nobody told it about is refused with collab/leave.
    let mut stranger = SimApp::start(&l, "S", None).unwrap();
    stranger.join(&json!({"room": "mqtt:unknown-room", "mode": "join", "user": {"id": "u", "name": "S", "color": "#000000"}})).unwrap();
    assert!(pump(&mut [&mut stranger], Duration::from_secs(5), &mut |x| !x[0].received("collab/leave").is_empty()));
}

#[test]
fn dialogs_without_params() {
    let l = launch();
    let mut a = SimApp::start(&l, "A", None).unwrap();
    let share = a.command("share", json!({})).unwrap();
    assert_eq!(share["dialog"]["kind"], "form");
    assert_eq!(share["dialog"]["command"], "share");
    let join = a.command("join", json!({})).unwrap();
    assert_eq!(join["dialog"]["command"], "join");
    let settings = a.command("settings", json!({})).unwrap();
    assert_eq!(settings["dialog"]["command"], "saveSettings");
    a.command("saveSettings", json!({"fields": {"broker": "mqtts://broker.example", "name": "Ada", "color": "#112233", "username": "", "prefix": "craft/v1"}}))
        .unwrap();
    let share = a.command("share", json!({})).unwrap();
    let broker = share["dialog"]["fields"].as_array().unwrap().iter().find(|f| f["id"] == "broker").unwrap()["value"].clone();
    assert_eq!(broker, "mqtts://broker.example:8883");
    assert!(a.command("saveSettings", json!({"fields": {"color": "red"}})).is_err());
    assert!(a.command("join", json!({"fields": {"link": "https://example.org"}})).is_err());
    let status = a.command("status", json!({})).unwrap();
    assert_eq!(status["result"]["rooms"], json!([]));
    // An app without protocol 2 is told so.
    let mut old = craft_collab_conformance::harness::PluginProc::spawn(&l, "old").unwrap();
    let r = old.request("initialize", json!({"host": {"id": "gridcraft", "features": ["dialogs"]}}), Duration::from_secs(5), &mut |_, _| {}).unwrap();
    assert!(r.get("result").is_some());
    let r = old.request("commands/run", json!({"command": "share", "params": {"broker": "mqtt://x"}}), Duration::from_secs(5), &mut |_, _| {}).unwrap();
    assert!(r["error"]["message"].as_str().unwrap().contains("protocol 2"));
}
