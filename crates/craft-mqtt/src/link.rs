//! One MQTT connection per room (Appendix A). Topics under `<prefix>/<room>`:
//!
//! | Topic | QoS | Retained | Payload |
//! |---|---|---|---|
//! | `m/<clientId>` | 1 | no | a message ([`Msg`]) |
//! | `snapshot` | 1 | yes | the room's whole state (a Yjs update, ciphertext when keyed) |
//! | `p/<clientId>` | 0 | yes | presence JSON; empty (and the will) clears it |
//!
//! The connection runs on its own thread and reconnects with backoff (1 s doubling to 60 s,
//! with jitter); every (re)connection reports [`NetEvent::Connected`], and the main loop then
//! subscribes again and republishes the presence, so the app re-syncs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use craft_collab_kit::collab::Joined;
use craft_collab_kit::frame::{self, Msg};
use craft_collab_kit::ids;
use craft_collab_kit::runtime::{Link, NetEvent, NetSink};
use rumqttc::{Client, ConnectReturnCode, ConnectionError, Event, LastWill, MqttOptions, Outgoing, Packet, QoS, SubscribeFilter, TlsConfiguration, Transport};
use serde_json::{Value as Json, json};

use crate::invite::Broker;

/// Largest MQTT packet sent or accepted: a 16 MiB update plus framing, encryption and topic.
pub const MAX_PACKET: usize = 17 << 20;
/// Largest presence payload accepted.
const MAX_PRESENCE: usize = 64 << 10;
/// Requests queued while the connection is down.
const QUEUE: usize = 4096;

/// How to reach a room.
#[derive(Clone, Debug)]
pub struct Target {
    pub broker: Broker,
    pub prefix: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// A room's MQTT connection.
pub struct MqttLink {
    client: Option<Client>,
    base: String,
    me: u64,
    presence: Vec<u8>,
    stop: Arc<AtomicBool>,
    place: String,
}

fn describe(e: &ConnectionError) -> String {
    match e {
        ConnectionError::ConnectionRefused(code) => match code {
            ConnectReturnCode::BadUserNamePassword => "The broker refused the user name or password.".into(),
            ConnectReturnCode::NotAuthorized => "The broker says you aren't allowed in (not authorized).".into(),
            ConnectReturnCode::ServiceUnavailable => "The broker is unavailable.".into(),
            other => format!("The broker refused the connection ({other:?})."),
        },
        ConnectionError::Io(io) => format!("Can't reach the broker: {io}"),
        ConnectionError::Tls(t) => format!("Secure connection failed: {t}"),
        ConnectionError::NetworkTimeout => "The broker didn't answer in time.".into(),
        other => format!("Connection lost: {other}"),
    }
}

/// Topic level → event.
fn route(base: &str, topic: &str, payload: &[u8]) -> Option<NetEvent> {
    let rest = topic.strip_prefix(base)?.strip_prefix('/')?;
    if rest == "snapshot" {
        return (!payload.is_empty()).then(|| NetEvent::Snapshot(payload.to_vec()));
    }
    if let Some(c) = rest.strip_prefix("m/") {
        let from = frame::parse_client(c)?;
        return Some(NetEvent::Message { from, msg: Msg::decode(payload)? });
    }
    if let Some(c) = rest.strip_prefix("p/") {
        let client = frame::parse_client(c)?;
        if payload.is_empty() {
            return Some(NetEvent::Presence { client, info: None });
        }
        if payload.len() > MAX_PRESENCE {
            return None;
        }
        let info = serde_json::from_slice::<Json>(payload).ok().filter(Json::is_object)?;
        return Some(NetEvent::Presence { client, info: Some(info) });
    }
    None
}

/// Sleeps `d`, waking early when asked to stop.
fn nap(d: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut left = d;
    while !left.is_zero() && !stop.load(Ordering::Relaxed) {
        let s = left.min(step);
        std::thread::sleep(s);
        left = left.saturating_sub(s);
    }
}

fn net_loop(mut conn: rumqttc::Connection, sink: NetSink, stop: Arc<AtomicBool>, base: String) {
    let mut delay = Duration::from_secs(1);
    let max = Duration::from_secs(60);
    loop {
        let Ok(ev) = conn.recv() else { break };
        let stopping = stop.load(Ordering::Relaxed);
        match ev {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                delay = Duration::from_secs(1);
                if !stopping && !sink.send(NetEvent::Connected) {
                    break;
                }
            }
            Ok(Event::Incoming(Packet::Publish(p))) => {
                if stopping {
                    continue;
                }
                if let Some(ev) = route(&base, &p.topic, &p.payload)
                    && !sink.send(ev)
                {
                    break;
                }
            }
            Ok(Event::Outgoing(Outgoing::Disconnect)) => break,
            Ok(_) => {}
            Err(e) => {
                if stopping {
                    break;
                }
                if !sink.send(NetEvent::Disconnected(describe(&e))) {
                    break;
                }
                // Jitter: up to a quarter of the delay, so clients don't come back in lockstep.
                let jitter = Duration::from_millis(ids::random_u64() % (delay.as_millis() as u64 / 4 + 1));
                nap(delay + jitter, &stop);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                delay = (delay * 2).min(max);
            }
        }
    }
}

impl MqttLink {
    /// Starts connecting a room (in the background).
    pub fn start(target: &Target, joined: &Joined, sink: NetSink) -> Result<MqttLink, String> {
        let base = format!("{}/{}", target.prefix, frame::topic_segment(&joined.room));
        let me = joined.client_id;
        // MQTT 3.1.1 brokers must accept ids of up to 23 characters.
        let mut o = MqttOptions::new(format!("craft-{}", ids::base36(17)), target.broker.host.clone(), target.broker.port);
        o.set_keep_alive(Duration::from_secs(30));
        o.set_clean_session(true);
        o.set_max_packet_size(MAX_PACKET, MAX_PACKET);
        if let Some(u) = &target.username {
            o.set_credentials(u.clone(), target.password.clone().unwrap_or_default());
        }
        o.set_last_will(LastWill::new(format!("{base}/p/{me}"), Vec::new(), QoS::AtMostOnce, true));
        if target.broker.tls {
            o.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(crate::tls::config()?)));
        }
        let (client, conn) = Client::new(o, QUEUE);
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_stop, thread_base) = (stop.clone(), base.clone());
        std::thread::Builder::new()
            .name(format!("mqtt {}", joined.room.chars().take(40).collect::<String>()))
            .spawn(move || net_loop(conn, sink, thread_stop, thread_base))
            .map_err(|e| format!("can't start a thread: {e}"))?;
        let presence = json!({"user": joined.user, "app": joined.app, "schema": joined.schema, "encrypted": joined.encrypted}).to_string().into_bytes();
        Ok(MqttLink { client: Some(client), base, me, presence, stop, place: target.broker.describe() })
    }

    fn client(&self) -> Result<&Client, String> {
        self.client.as_ref().ok_or_else(|| "the room is closed".to_string())
    }
}

impl Link for MqttLink {
    fn connected(&mut self) -> Result<(), String> {
        let c = self.client()?;
        let filters = vec![
            SubscribeFilter::new(format!("{}/m/+", self.base), QoS::AtLeastOnce),
            SubscribeFilter::new(format!("{}/snapshot", self.base), QoS::AtLeastOnce),
            SubscribeFilter::new(format!("{}/p/+", self.base), QoS::AtMostOnce),
        ];
        c.try_subscribe_many(filters).map_err(|e| format!("can't subscribe: {e}"))?;
        c.try_publish(format!("{}/p/{}", self.base, self.me), QoS::AtMostOnce, true, self.presence.clone()).map_err(|e| format!("can't announce presence: {e}"))
    }

    fn send(&mut self, msg: &Msg) -> Result<(), String> {
        let topic = format!("{}/m/{}", self.base, self.me);
        self.client()?.try_publish(topic, QoS::AtLeastOnce, false, msg.encode()).map_err(|e| e.to_string())
    }

    fn store_snapshot(&mut self, data: &[u8]) -> Result<(), String> {
        let topic = format!("{}/snapshot", self.base);
        self.client()?.try_publish(topic, QoS::AtLeastOnce, true, data.to_vec()).map_err(|e| e.to_string())
    }

    fn close(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.client.take() {
            let _ = c.try_publish(format!("{}/m/{}", self.base, self.me), QoS::AtLeastOnce, false, Msg::Leave.encode());
            let _ = c.try_publish(format!("{}/p/{}", self.base, self.me), QoS::AtMostOnce, true, Vec::<u8>::new());
            let _ = c.try_disconnect();
        }
    }

    fn describe(&self) -> String {
        self.place.clone()
    }
}

impl Drop for MqttLink {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_topics() {
        let base = "craft/v1/mqtt:r";
        assert!(matches!(route(base, "craft/v1/mqtt:r/m/42", &[2, 1, 2]), Some(NetEvent::Message { from: 42, msg: Msg::Update(_) })));
        assert!(matches!(route(base, "craft/v1/mqtt:r/snapshot", &[1]), Some(NetEvent::Snapshot(_))));
        assert!(route(base, "craft/v1/mqtt:r/snapshot", &[]).is_none());
        assert!(matches!(route(base, "craft/v1/mqtt:r/p/7", b""), Some(NetEvent::Presence { client: 7, info: None })));
        assert!(matches!(route(base, "craft/v1/mqtt:r/p/7", br#"{"user":{}}"#), Some(NetEvent::Presence { client: 7, info: Some(_) })));
        assert!(route(base, "craft/v1/mqtt:r/p/7", b"[]").is_none());
        assert!(route(base, "craft/v1/mqtt:r/m/x", &[2]).is_none());
        assert!(route(base, "craft/v1/mqtt:r/m/1", &[9]).is_none());
        assert!(route(base, "craft/v1/mqtt:other/m/1", &[2]).is_none());
        assert!(route(base, "craft/v1/mqtt:r2/m/1", &[2]).is_none());
    }
}
