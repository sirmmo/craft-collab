//! The commands (Share, Join, Leave, Status, Settings) and the transport itself.
//!
//! Settings live in the data folder (`CRAFT_MQTT_DATA`, else `craft-mqtt` in the platform's data
//! folder, shared by the three apps): `settings.json` (default broker, user name for the broker,
//! your name and colour, a stable person id) and `rooms.json` (the rooms joined lately and their
//! broker, so a restarted transport can take the app's re-join). Passwords and room keys are
//! never written.

use std::collections::HashMap;

use craft_collab_kit::collab::Joined;
use craft_collab_kit::runtime::{Cx, Fail, NetSink, Opened, Plugin, fail};
use craft_collab_kit::store::Store;
use craft_collab_kit::{b64, collab, ids, ui};
use serde_json::{Value as Json, json};

use crate::invite::{Broker, DEFAULT_PREFIX, Invitation, valid_prefix};
use crate::link::{MqttLink, Target};

/// Rooms remembered in `rooms.json`.
const MAX_RECENT: usize = 64;

/// What a share or join set up, until the app's `collab/joined`.
#[derive(Clone, Debug)]
struct RoomConfig {
    invitation: Invitation,
    target: Target,
    share: bool,
}

/// The transport.
pub struct Mqtt {
    store: Option<Store>,
    rooms: HashMap<String, RoomConfig>,
    /// `broker url + user` → password, for this run only.
    passwords: HashMap<String, String>,
}

impl Default for Mqtt {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Mqtt {
    pub fn new() -> Mqtt {
        Mqtt { store: Store::open("craft-mqtt", "CRAFT_MQTT_DATA").ok(), rooms: HashMap::new(), passwords: HashMap::new() }
    }

    fn settings(&self) -> Json {
        self.store.as_ref().map(|s| s.load("settings")).unwrap_or_else(|| json!({}))
    }

    fn save_settings(&self, value: &Json) -> Result<(), String> {
        match &self.store {
            Some(s) => s.save("settings", value),
            None => Err("there's no folder for settings (no HOME or APPDATA)".into()),
        }
    }

    /// The person's stable id, made once and kept in the settings.
    fn user_id(&self) -> String {
        let mut s = self.settings();
        if let Some(id) = s.get("userId").and_then(Json::as_str).filter(|i| (1..=128).contains(&i.len())) {
            return id.to_string();
        }
        let id = format!("u-{}", ids::base36(12));
        collab::put(&mut s, "userId", json!(id));
        let _ = self.save_settings(&s);
        id
    }

    fn remember(&mut self, cfg: &RoomConfig) {
        if let (Some(u), Some(p)) = (&cfg.target.username, &cfg.target.password) {
            self.passwords.insert(format!("{} {u}", cfg.target.broker.url()), p.clone());
        }
        let Some(store) = &self.store else { return };
        let mut recent = store.load("rooms");
        let room = cfg.invitation.room();
        collab::put(
            &mut recent,
            &room,
            json!({"broker": cfg.target.broker.url(), "prefix": cfg.target.prefix, "username": cfg.target.username, "at": now_secs()}),
        );
        if let Some(map) = recent.as_object_mut() {
            while map.len() > MAX_RECENT {
                let oldest = map.iter().min_by_key(|(_, v)| v.get("at").and_then(Json::as_u64).unwrap_or(0)).map(|(k, _)| k.clone());
                match oldest {
                    Some(k) => map.remove(&k),
                    None => break,
                };
            }
        }
        let _ = store.save("rooms", &recent);
        self.rooms.insert(room, cfg.clone());
    }

    /// How to reach a room the app joins: from this run's share or join, else from `rooms.json`.
    fn target_for(&self, room: &str) -> Option<(Target, bool)> {
        if let Some(c) = self.rooms.get(room) {
            return Some((c.target.clone(), c.share));
        }
        let recent = self.store.as_ref()?.load("rooms");
        let r = recent.get(room)?;
        let broker = Broker::parse(r.get("broker")?.as_str()?).ok()?;
        let prefix = r.get("prefix").and_then(Json::as_str).filter(|p| valid_prefix(p)).unwrap_or(DEFAULT_PREFIX).to_string();
        let username = r.get("username").and_then(Json::as_str).map(str::to_string);
        let password = username.as_ref().and_then(|u| self.passwords.get(&format!("{} {u}", broker.url())).cloned());
        Some((Target { broker, prefix, username, password }, false))
    }

    fn identity(&self, inputs: &Json) -> Result<(String, String), Fail> {
        let s = self.settings();
        let name = ui::text(inputs, "name").or_else(|| ui::text(&s, "name")).unwrap_or_default();
        let name = ui::check_name(&name)?;
        let color = match ui::text(inputs, "color").or_else(|| ui::text(&s, "color")) {
            Some(c) => ui::check_color(&c)?,
            None => ui::any_color().to_string(),
        };
        Ok((name, color))
    }

    /// Remembers name, colour and broker as the next defaults.
    fn keep_defaults(&self, name: &str, color: &str, broker: Option<&Broker>, username: Option<&str>) {
        let mut s = self.settings();
        collab::put(&mut s, "name", json!(name));
        collab::put(&mut s, "color", json!(color));
        if let Some(b) = broker {
            collab::put(&mut s, "broker", json!(b.url()));
            collab::put(&mut s, "username", json!(username));
        }
        let _ = self.save_settings(&s);
    }

    fn share(&mut self, cx: &Cx, params: &Json, context: &Json) -> Result<Json, Fail> {
        ui::require_collab(cx)?;
        let inputs = ui::inputs(params);
        let s = self.settings();
        if params.get("fields").is_none() && ui::text(inputs, "broker").is_none() {
            let fields = vec![
                ui::text_field("broker", "Broker (mqtt:// or mqtts://host:port)", &ui::text(&s, "broker").unwrap_or_default()),
                ui::text_field("username", "User name on the broker (optional)", &ui::text(&s, "username").unwrap_or_default()),
                ui::secret_field("password", "Password (optional)"),
                ui::text_field("name", "Your name", &ui::text(&s, "name").unwrap_or_default()),
                ui::text_field("color", "Your colour (#RRGGBB)", &ui::text(&s, "color").unwrap_or_else(|| ui::any_color().to_string())),
                ui::checkbox("encrypt", "Encrypt end to end (the broker sees only ciphertext)", true),
            ];
            let prompt = "Start a live session on an MQTT broker: the people you invite edit this document with you.";
            return Ok(json!({"dialog": ui::form("Share Live (MQTT)", prompt, fields, "share", "Start")}));
        }
        let broker_text = ui::text(inputs, "broker").ok_or_else(|| fail("Type the broker's address, for example mqtts://broker.example:8883."))?;
        let broker = Broker::parse(&broker_text).map_err(fail)?;
        let (name, color) = self.identity(inputs)?;
        let prefix = ui::text(inputs, "prefix").or_else(|| ui::text(&s, "prefix")).unwrap_or_else(|| DEFAULT_PREFIX.to_string());
        if !valid_prefix(&prefix) {
            return Err(fail(format!("“{prefix}” isn't a topic prefix (no + or #, no / at the ends).")));
        }
        let room_name = match ui::text(inputs, "room") {
            Some(r) if ids::valid_room_name(&r) => Some(r),
            Some(r) => return Err(fail(format!("“{r}” isn't a room name: use letters, digits, '.', '_' and '-'."))),
            None => None,
        };
        let encrypt = ui::flag(inputs, "encrypt").unwrap_or(true);
        let username = ui::text(inputs, "username");
        let password = inputs.get("password").and_then(Json::as_str).filter(|p| !p.is_empty()).map(str::to_string);
        let invitation = Invitation::create(broker.clone(), &prefix, room_name, encrypt);
        let target = Target { broker: broker.clone(), prefix, username: username.clone(), password };
        let cfg = RoomConfig { invitation: invitation.clone(), target, share: true };
        self.keep_defaults(&name, &color, Some(&broker), username.as_deref());
        self.remember(&cfg);
        let mut join = json!({"room": invitation.room(), "mode": "share", "user": collab::user(&self.user_id(), &name, &color), "role": "edit"});
        if let Some(k) = &invitation.key {
            collab::put(&mut join, "key", json!(b64::encode(k)));
        }
        if let Some(t) = context.get("document").and_then(|d| d.get("name")).and_then(Json::as_str) {
            collab::put(&mut join, "title", json!(t.chars().take(200).collect::<String>()));
        }
        let link = invitation.link();
        let prompt = if encrypt {
            "Send this invitation to the people you want to edit with. Anyone who has it can join. The part after #k= is the key that encrypts the session: send the whole link through a channel you trust."
        } else {
            "Send this invitation to the people you want to edit with. Anyone who has it can join. The session isn't encrypted: the broker can read it."
        };
        let dialog = ui::form("Live session started", prompt, vec![ui::text_field("link", "Invitation", &link)], "dismiss", "Done");
        Ok(json!({"collab": {"join": join}, "dialog": dialog, "message": "Live session started", "result": {"room": invitation.room(), "link": link}}))
    }

    fn join(&mut self, cx: &Cx, params: &Json) -> Result<Json, Fail> {
        ui::require_collab(cx)?;
        let inputs = ui::inputs(params);
        let s = self.settings();
        let Some(link) = ui::text(inputs, "link") else {
            if params.get("fields").is_some() {
                return Err(fail("Paste the invitation link you were sent."));
            }
            let fields = vec![
                ui::text_field("link", "Invitation link (craft+mqtt://…)", ""),
                ui::text_field("name", "Your name", &ui::text(&s, "name").unwrap_or_default()),
                ui::text_field("username", "User name on the broker (optional)", &ui::text(&s, "username").unwrap_or_default()),
                ui::secret_field("password", "Password (optional)"),
            ];
            return Ok(json!({"dialog": ui::form("Join Live (MQTT)", "Join a live session someone shared with you.", fields, "join", "Join")}));
        };
        let invitation = Invitation::parse(&link).map_err(fail)?;
        let (name, color) = self.identity(inputs)?;
        let username = ui::text(inputs, "username");
        let password = inputs.get("password").and_then(Json::as_str).filter(|p| !p.is_empty()).map(str::to_string);
        let target = Target { broker: invitation.broker.clone(), prefix: invitation.prefix.clone(), username, password };
        let cfg = RoomConfig { invitation: invitation.clone(), target, share: false };
        self.keep_defaults(&name, &color, None, None);
        self.remember(&cfg);
        let mut join = json!({"room": invitation.room(), "mode": "join", "user": collab::user(&self.user_id(), &name, &color), "role": "edit"});
        if let Some(k) = &invitation.key {
            collab::put(&mut join, "key", json!(b64::encode(k)));
        }
        Ok(json!({"collab": {"join": join}, "message": "Joining the live session…", "result": {"room": invitation.room(), "link": invitation.link()}}))
    }

    fn settings_dialog(&self) -> Json {
        let s = self.settings();
        let fields = vec![
            ui::text_field("broker", "Default broker", &ui::text(&s, "broker").unwrap_or_default()),
            ui::text_field("username", "User name on the broker", &ui::text(&s, "username").unwrap_or_default()),
            ui::text_field("name", "Your name", &ui::text(&s, "name").unwrap_or_default()),
            ui::text_field("color", "Your colour (#RRGGBB)", &ui::text(&s, "color").unwrap_or_default()),
            ui::text_field("prefix", "Topic prefix", &ui::text(&s, "prefix").unwrap_or_else(|| DEFAULT_PREFIX.to_string())),
        ];
        let dir = self.store.as_ref().map(|s| s.dir().display().to_string()).unwrap_or_else(|| "(none)".into());
        json!({"dialog": ui::form("Live (MQTT) Settings", &format!("Defaults for Share and Join. Saved in {dir}; passwords and room keys are never saved."), fields, "saveSettings", "Save")})
    }

    fn save_settings_command(&self, params: &Json) -> Result<Json, Fail> {
        let f = ui::inputs(params);
        let mut s = self.settings();
        if let Some(b) = ui::text(f, "broker") {
            collab::put(&mut s, "broker", json!(Broker::parse(&b).map_err(fail)?.url()));
        } else if f.get("broker").is_some() {
            collab::put(&mut s, "broker", Json::Null);
        }
        if let Some(c) = ui::text(f, "color") {
            collab::put(&mut s, "color", json!(ui::check_color(&c)?));
        }
        if let Some(n) = ui::text(f, "name") {
            collab::put(&mut s, "name", json!(ui::check_name(&n)?));
        }
        if f.get("username").is_some() {
            collab::put(&mut s, "username", json!(ui::text(f, "username")));
        }
        if let Some(p) = ui::text(f, "prefix") {
            if !valid_prefix(&p) {
                return Err(fail(format!("“{p}” isn't a topic prefix (no + or #, no / at the ends).")));
            }
            collab::put(&mut s, "prefix", json!(p));
        }
        self.save_settings(&s).map_err(fail)?;
        Ok(json!({"message": "Live (MQTT) settings saved."}))
    }
}

impl Plugin for Mqtt {
    fn name(&self) -> &'static str {
        "craft-mqtt"
    }

    fn command(&mut self, cx: &mut Cx, command: &str, params: &Json, context: &Json) -> Result<Json, Fail> {
        match command {
            "share" => self.share(cx, params, context),
            "join" => self.join(cx, params),
            "leave" => ui::leave(cx, params),
            "status" => {
                let links: HashMap<String, String> = self.rooms.iter().map(|(r, c)| (r.clone(), c.invitation.link())).collect();
                Ok(ui::status(cx, "Live (MQTT)", &|room: &str| links.get(room).cloned()))
            }
            "settings" => Ok(self.settings_dialog()),
            "saveSettings" => self.save_settings_command(params),
            "dismiss" => Ok(json!({})),
            other => Err(fail(format!("craft-mqtt has no command {other}"))),
        }
    }

    fn open(&mut self, joined: &Joined, sink: NetSink) -> Result<Opened, String> {
        let (target, share) =
            self.target_for(&joined.room).ok_or_else(|| format!("craft-mqtt doesn't know the room {}: join it again from its invitation.", joined.room))?;
        let link = MqttLink::start(&target, joined, sink)?;
        Ok(Opened { link: Box::new(link), seed: share })
    }
}
