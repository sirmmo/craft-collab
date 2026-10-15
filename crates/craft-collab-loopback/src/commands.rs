//! Share and Join with a room name (`craft+loopback:<room>#k=<key>` when encrypted), Leave,
//! Status. Your name, colour and person id are kept in the data folder
//! (`CRAFT_COLLAB_LOOPBACK_DATA`, else `craft-collab-loopback` in the platform's data folder).

use std::collections::HashMap;

use craft_collab_kit::collab::Joined;
use craft_collab_kit::runtime::{Cx, Fail, NetSink, Opened, Plugin, fail};
use craft_collab_kit::store::Store;
use craft_collab_kit::{b64, collab, ids, ui};
use serde_json::{Value as Json, json};

use crate::link::LoopLink;

/// The transport.
pub struct Loopback {
    store: Option<Store>,
    /// room → (invitation link, shared by this instance).
    rooms: HashMap<String, (String, bool)>,
}

impl Default for Loopback {
    fn default() -> Self {
        Self::new()
    }
}

/// `craft+loopback:<room>#k=<key>`, `loopback:<room>` or a bare room name → (name, key).
pub fn parse_invitation(text: &str) -> Result<(String, Option<[u8; 32]>), String> {
    let t = text.trim();
    let (body, fragment) = match t.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (t, None),
    };
    let name = body.strip_prefix("craft+loopback:").or_else(|| body.strip_prefix("loopback:")).unwrap_or(body).trim_start_matches('/');
    if !ids::valid_room_name(name) {
        return Err(format!("“{name}” isn't a room name: use letters, digits, '.', '_' and '-'."));
    }
    let mut key = None;
    for pair in fragment.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        if let Some(v) = pair.strip_prefix("k=") {
            let bytes = b64::decode_lenient(v).ok_or("the invitation's key isn't readable")?;
            key = Some(<[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "the invitation's key isn't 32 bytes".to_string())?);
        }
    }
    Ok((name.to_string(), key))
}

/// The invitation for a room.
pub fn invitation(name: &str, key: Option<&[u8; 32]>) -> String {
    match key {
        Some(k) => format!("craft+loopback:{name}#k={}", b64::encode_url(k)),
        None => format!("craft+loopback:{name}"),
    }
}

impl Loopback {
    pub fn new() -> Loopback {
        Loopback { store: Store::open("craft-collab-loopback", "CRAFT_COLLAB_LOOPBACK_DATA").ok(), rooms: HashMap::new() }
    }

    fn settings(&self) -> Json {
        self.store.as_ref().map(|s| s.load("settings")).unwrap_or_else(|| json!({}))
    }

    /// (person id, name, colour), remembering the name and colour for next time.
    fn identity(&self, inputs: &Json) -> Result<Json, Fail> {
        let mut s = self.settings();
        let fallback = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "Me".into());
        let name = ui::check_name(&ui::text(inputs, "name").or_else(|| ui::text(&s, "name")).unwrap_or(fallback))?;
        let color = match ui::text(inputs, "color").or_else(|| ui::text(&s, "color")) {
            Some(c) => ui::check_color(&c)?,
            None => ui::any_color().to_string(),
        };
        let id = match s.get("userId").and_then(Json::as_str).filter(|i| (1..=128).contains(&i.len())) {
            Some(i) => i.to_string(),
            None => format!("u-{}", ids::base36(12)),
        };
        collab::put(&mut s, "userId", json!(id));
        collab::put(&mut s, "name", json!(name));
        collab::put(&mut s, "color", json!(color));
        if let Some(store) = &self.store {
            let _ = store.save("settings", &s);
        }
        Ok(collab::user(&id, &name, &color))
    }

    fn share(&mut self, cx: &Cx, params: &Json, context: &Json) -> Result<Json, Fail> {
        ui::require_collab(cx)?;
        let inputs = ui::inputs(params);
        if params.as_object().is_none_or(|o| o.is_empty()) {
            let s = self.settings();
            let fields = vec![
                ui::text_field("room", "Room name", &format!("dev-{}", ids::base36(6))),
                ui::text_field("name", "Your name", &ui::text(&s, "name").unwrap_or_default()),
                ui::checkbox("encrypt", "Encrypt end to end", false),
            ];
            return Ok(json!({"dialog": ui::form("Share Live (Loopback)", "Start a live session between apps on this computer.", fields, "share", "Start")}));
        }
        let name = match ui::text(inputs, "room") {
            Some(r) => parse_invitation(&r).map_err(fail)?.0,
            None => format!("dev-{}", ids::base36(6)),
        };
        let user = self.identity(inputs)?;
        let key = ui::flag(inputs, "encrypt").unwrap_or(false).then(ids::key);
        let room = format!("loopback:{name}");
        let link = invitation(&name, key.as_ref());
        self.rooms.insert(room.clone(), (link.clone(), true));
        let mut join = json!({"room": room, "mode": "share", "user": user, "role": "edit"});
        if let Some(k) = &key {
            collab::put(&mut join, "key", json!(b64::encode(k)));
        }
        if let Some(t) = context.get("document").and_then(|d| d.get("name")).and_then(Json::as_str) {
            collab::put(&mut join, "title", json!(t.chars().take(200).collect::<String>()));
        }
        let dialog = ui::form(
            "Live session started",
            "Join it from another app on this computer with this invitation (or just the room name).",
            vec![ui::text_field("link", "Invitation", &link)],
            "dismiss",
            "Done",
        );
        Ok(json!({"collab": {"join": join}, "dialog": dialog, "message": "Live session started", "result": {"room": room, "link": link}}))
    }

    fn join(&mut self, cx: &Cx, params: &Json) -> Result<Json, Fail> {
        ui::require_collab(cx)?;
        let inputs = ui::inputs(params);
        let Some(text) = ui::text(inputs, "link").or_else(|| ui::text(inputs, "room")) else {
            if params.get("fields").is_some() {
                return Err(fail("Type the room name or paste the invitation."));
            }
            let s = self.settings();
            let fields =
                vec![ui::text_field("link", "Room name or invitation", ""), ui::text_field("name", "Your name", &ui::text(&s, "name").unwrap_or_default())];
            return Ok(json!({"dialog": ui::form("Join Live (Loopback)", "Join a live session started on this computer.", fields, "join", "Join")}));
        };
        let (name, mut key) = parse_invitation(&text).map_err(fail)?;
        if let Some(k) = ui::text(inputs, "key") {
            let bytes = b64::decode_lenient(&k).ok_or_else(|| fail("the key isn't base64"))?;
            key = Some(<[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| fail("the key isn't 32 bytes"))?);
        }
        let user = self.identity(inputs)?;
        let room = format!("loopback:{name}");
        let link = invitation(&name, key.as_ref());
        self.rooms.insert(room.clone(), (link.clone(), false));
        let mut join = json!({"room": room, "mode": "join", "user": user, "role": "edit"});
        if let Some(k) = &key {
            collab::put(&mut join, "key", json!(b64::encode(k)));
        }
        Ok(json!({"collab": {"join": join}, "message": "Joining the live session…", "result": {"room": room, "link": link}}))
    }
}

impl Plugin for Loopback {
    fn name(&self) -> &'static str {
        "craft-collab-loopback"
    }

    fn command(&mut self, cx: &mut Cx, command: &str, params: &Json, context: &Json) -> Result<Json, Fail> {
        match command {
            "share" => self.share(cx, params, context),
            "join" => self.join(cx, params),
            "leave" => ui::leave(cx, params),
            "status" => {
                let links: HashMap<String, String> = self.rooms.iter().map(|(r, (l, _))| (r.clone(), l.clone())).collect();
                Ok(ui::status(cx, "Live (Loopback)", &|room: &str| links.get(room).cloned()))
            }
            "dismiss" => Ok(json!({})),
            other => Err(fail(format!("craft-collab-loopback has no command {other}"))),
        }
    }

    fn open(&mut self, joined: &Joined, sink: NetSink) -> Result<Opened, String> {
        if !joined.room.starts_with("loopback:") {
            return Err(format!("{} isn't a loopback room", joined.room));
        }
        let seed = self.rooms.get(&joined.room).is_some_and(|(_, share)| *share);
        let link = LoopLink::start(crate::port(), joined, sink)?;
        Ok(Opened { link: Box::new(link), seed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitations() {
        let k = [3u8; 32];
        let l = invitation("dev-1", Some(&k));
        assert_eq!(parse_invitation(&l).unwrap(), ("dev-1".into(), Some(k)));
        assert_eq!(parse_invitation("loopback:dev-1").unwrap(), ("dev-1".into(), None));
        assert_eq!(parse_invitation(" dev-1 ").unwrap(), ("dev-1".into(), None));
        assert!(parse_invitation("a b").is_err());
        assert!(parse_invitation("craft+loopback:x#k=abc").is_err());
    }
}
