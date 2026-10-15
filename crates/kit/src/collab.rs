//! The `collab/*` messages an app sends a transport (protocol 2, section 3), checked: an app's
//! message is input like any other, and a bad one is logged and dropped.

use serde_json::{Value as Json, json};

use crate::b64;

/// Client ids are below 2^53 (they travel as JSON numbers).
pub const MAX_CLIENT_ID: u64 = 1 << 53;
/// An update before base64 (section 7), plus room for the encryption's nonce and tag.
pub const MAX_UPDATE: usize = (16 << 20) + 64;
/// An awareness update before base64 (section 7), plus the encryption's nonce and tag.
pub const MAX_AWARENESS: usize = (64 << 10) + 64;
/// A state vector is a few bytes per client that ever edited; 1 MiB is generous.
pub const MAX_STATE_VECTOR: usize = 1 << 20;
/// A room id (section 2).
pub const MAX_ROOM: usize = 256;
/// Rooms a transport holds at once (section 7).
pub const MAX_ROOMS: usize = 16;

/// What the app said in `collab/joined`.
#[derive(Clone, Debug, PartialEq)]
pub struct Joined {
    pub room: String,
    pub client_id: u64,
    pub user: Json,
    pub app: String,
    pub schema: i64,
    pub encrypted: bool,
}

/// A `collab/*` notification from the app.
#[derive(Clone, Debug, PartialEq)]
pub enum FromApp {
    Joined(Joined),
    Sync1 { room: String, state_vector: Vec<u8> },
    Sync2 { room: String, update: Vec<u8>, to: Option<u64> },
    Update { room: String, update: Vec<u8> },
    Awareness { room: String, update: Vec<u8> },
    Leave { room: String, reason: Option<String> },
}

impl FromApp {
    pub fn room(&self) -> &str {
        match self {
            FromApp::Joined(j) => &j.room,
            FromApp::Sync1 { room, .. }
            | FromApp::Sync2 { room, .. }
            | FromApp::Update { room, .. }
            | FromApp::Awareness { room, .. }
            | FromApp::Leave { room, .. } => room,
        }
    }
}

/// A room id: 1–256 characters, no control characters.
pub fn valid_room(room: &str) -> bool {
    !room.is_empty() && room.chars().count() <= MAX_ROOM && !room.chars().any(char::is_control)
}

/// `#RRGGBB`.
pub fn valid_color(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c.bytes().skip(1).all(|b| b.is_ascii_hexdigit())
}

/// A user: `{id (1–128), name (1–128), color (#RRGGBB)}`.
pub fn valid_user(u: &Json) -> bool {
    let len_ok = |k: &str| u.get(k).and_then(Json::as_str).is_some_and(|s| (1..=128).contains(&s.chars().count()));
    len_ok("id") && len_ok("name") && u.get("color").and_then(Json::as_str).is_some_and(valid_color)
}

/// Sets `obj[key]` when `obj` is an object (indexing a JSON value could panic).
pub fn put(obj: &mut Json, key: &str, value: Json) {
    if let Some(o) = obj.as_object_mut() {
        o.insert(key.to_string(), value);
    }
}

/// The user object of a join.
pub fn user(id: &str, name: &str, color: &str) -> Json {
    json!({"id": id, "name": name, "color": color})
}

fn bytes(params: &Json, key: &str, max: usize) -> Result<Vec<u8>, String> {
    let text = params.get(key).and_then(Json::as_str).ok_or_else(|| format!("missing `{key}`"))?;
    // Base64 is 4/3 the size: refuse before decoding something huge.
    if text.len() > max.saturating_mul(4) / 3 + 8 {
        return Err(format!("`{key}` is too large"));
    }
    let data = b64::decode(text).ok_or_else(|| format!("`{key}` isn't base64"))?;
    if data.len() > max {
        return Err(format!("`{key}` is too large"));
    }
    Ok(data)
}

fn client(v: Option<&Json>) -> Option<u64> {
    v.and_then(Json::as_u64).filter(|c| *c < MAX_CLIENT_ID)
}

/// Parses one `collab/*` notification from the app.
pub fn parse(method: &str, params: &Json) -> Result<FromApp, String> {
    let room = params.get("room").and_then(Json::as_str).filter(|r| valid_room(r)).ok_or("missing or bad `room`")?.to_string();
    Ok(match method {
        "collab/joined" => {
            let client_id = client(params.get("clientId")).ok_or("missing or bad `clientId`")?;
            let user = params.get("user").cloned().filter(valid_user).ok_or("missing or bad `user`")?;
            let app = params.get("app").and_then(Json::as_str).unwrap_or("").chars().take(64).collect();
            let schema = params.get("schema").and_then(Json::as_i64).unwrap_or(0);
            let encrypted = params.get("encrypted").and_then(Json::as_bool).unwrap_or(false);
            FromApp::Joined(Joined { room, client_id, user, app, schema, encrypted })
        }
        "collab/sync1" => FromApp::Sync1 { state_vector: bytes(params, "stateVector", MAX_STATE_VECTOR)?, room },
        "collab/sync2" => {
            let to = match params.get("to") {
                None | Some(Json::Null) => None,
                Some(v) => Some(client(Some(v)).ok_or("bad `to`")?),
            };
            FromApp::Sync2 { update: bytes(params, "update", MAX_UPDATE)?, to, room }
        }
        "collab/update" => FromApp::Update { update: bytes(params, "update", MAX_UPDATE)?, room },
        "collab/awareness" => FromApp::Awareness { update: bytes(params, "update", MAX_AWARENESS)?, room },
        "collab/leave" => FromApp::Leave { reason: params.get("reason").and_then(Json::as_str).map(|s| s.chars().take(500).collect()), room },
        _ => return Err(format!("unknown method {method}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_refuses() {
        let j = parse(
            "collab/joined",
            &json!({"room": "mqtt:x", "clientId": 7, "user": user("u-1", "Ada", "#3B7DD8"), "app": "gridcraft", "schema": 1, "encrypted": true}),
        )
        .unwrap();
        assert!(matches!(j, FromApp::Joined(Joined { client_id: 7, encrypted: true, .. })));
        let u = parse("collab/update", &json!({"room": "r", "update": b64::encode(&[1, 2])})).unwrap();
        assert_eq!(u, FromApp::Update { room: "r".into(), update: vec![1, 2] });
        assert!(parse("collab/update", &json!({"room": "r", "update": "%%%"})).is_err());
        assert!(parse("collab/update", &json!({"update": "AA=="})).is_err());
        assert!(parse("collab/joined", &json!({"room": "r", "clientId": 1u64 << 60, "user": user("a", "b", "#000000")})).is_err());
        assert!(parse("collab/joined", &json!({"room": "r", "clientId": 1, "user": user("a", "b", "red")})).is_err());
        assert!(parse("collab/sync2", &json!({"room": "r", "update": "AA==", "to": "x"})).is_err());
        assert!(parse("collab/nope", &json!({"room": "r"})).is_err());
        let big = "A".repeat(200_000);
        assert!(parse("collab/awareness", &json!({"room": "r", "update": big})).is_err());
    }
}
