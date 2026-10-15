//! Dialogs (protocol 1.2) and the commands every transport has: leave and status.

use serde_json::{Value as Json, json};

use crate::collab;
use crate::runtime::{Cx, Fail, fail};

/// Colours offered to people who haven't picked one (distinct on light and dark themes).
pub const PALETTE: [&str; 8] = ["#3B7DD8", "#D8513B", "#2E9E5B", "#9B51E0", "#E0A100", "#1F9FB4", "#D63F8C", "#6B7A8F"];

/// A colour from the palette.
pub fn any_color() -> &'static str {
    let i = (crate::ids::random_u64() % PALETTE.len() as u64) as usize;
    PALETTE.get(i).copied().unwrap_or("#3B7DD8")
}

pub fn text_field(id: &str, label: &str, value: &str) -> Json {
    json!({"id": id, "label": label, "value": value})
}

pub fn secret_field(id: &str, label: &str) -> Json {
    json!({"id": id, "label": label, "kind": "secret", "value": ""})
}

pub fn checkbox(id: &str, label: &str, value: bool) -> Json {
    json!({"id": id, "label": label, "kind": "checkbox", "value": value})
}

/// A form whose OK runs `command` with the fields.
pub fn form(title: &str, prompt: &str, fields: Vec<Json>, command: &str, ok: &str) -> Json {
    json!({"title": title, "kind": "form", "prompt": prompt, "fields": fields, "command": command, "okLabel": ok})
}

/// A message box.
pub fn message(title: &str, prompt: &str) -> Json {
    json!({"title": title, "kind": "message", "prompt": prompt})
}

/// The values a command works with: a form's `fields`, else the params themselves (a
/// programmatic run).
pub fn inputs(params: &Json) -> &Json {
    params.get("fields").filter(|f| f.is_object()).unwrap_or(params)
}

/// A non-empty, trimmed text input.
pub fn text(inputs: &Json, key: &str) -> Option<String> {
    inputs.get(key).and_then(Json::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// A yes/no input (`true`, `"true"`, `1`).
pub fn flag(inputs: &Json, key: &str) -> Option<bool> {
    match inputs.get(key)? {
        Json::Bool(b) => Some(*b),
        Json::Number(n) => Some(n.as_f64().is_some_and(|x| x != 0.0)),
        Json::String(s) => Some(matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "on")),
        _ => None,
    }
}

/// A person's name for a join: 1–128 characters.
pub fn check_name(name: &str) -> Result<String, Fail> {
    let n = name.trim();
    if n.is_empty() {
        return Err(fail("Type your name, so the others know who you are."));
    }
    Ok(n.chars().take(128).collect())
}

/// A colour for a join: `#RRGGBB` (upper case).
pub fn check_color(color: &str) -> Result<String, Fail> {
    let c = color.trim();
    let c = if c.starts_with('#') { c.to_string() } else { format!("#{c}") };
    if !collab::valid_color(&c) {
        return Err(fail(format!("“{color}” isn't a colour: use #RRGGBB, for example #3B7DD8.")));
    }
    Ok(c.to_ascii_uppercase())
}

/// The app speaks protocol 2, or an error saying so.
pub fn require_collab(cx: &Cx) -> Result<(), Fail> {
    if cx.session.can_collab() { Ok(()) } else { Err(fail("This app doesn't support live co-editing yet (it needs craft plugin protocol 2).")) }
}

/// `leave {room?}`: leaves the given room, or the only one.
pub fn leave(cx: &mut Cx, params: &Json) -> Result<Json, Fail> {
    let inputs = inputs(params);
    let rooms = cx.rooms();
    let room = match text(inputs, "room") {
        Some(r) => r,
        None => match rooms.as_slice() {
            [] => return Ok(json!({"message": "You're not in a live session."})),
            [one] => one.room.clone(),
            _ => return Err(fail("You're in several live sessions: leave from the app's status bar, or name the room.")),
        },
    };
    if cx.leave(&room, "You left the live session.") {
        Ok(json!({"message": "Left the live session.", "result": {"room": room}}))
    } else {
        Err(fail(format!("Not in the live session {room}.")))
    }
}

fn names(users: &[Json]) -> String {
    let n: Vec<&str> = users.iter().filter_map(|u| u.get("name").and_then(Json::as_str)).collect();
    if n.is_empty() { "nobody else yet".into() } else { n.join(", ") }
}

/// `status`: the rooms held, with their invitation links (`link_for`) in fields that can be
/// copied.
pub fn status(cx: &Cx, title: &str, link_for: &dyn Fn(&str) -> Option<String>) -> Json {
    let rooms = cx.rooms();
    if rooms.is_empty() {
        return json!({"dialog": message(title, "You're not in a live session."), "result": {"rooms": []}});
    }
    let mut prompt = String::new();
    let mut fields = Vec::new();
    let mut list = Vec::new();
    for (i, r) in rooms.iter().enumerate() {
        if !prompt.is_empty() {
            prompt.push('\n');
        }
        prompt.push_str(&format!(
            "{}\nServer: {}\nState: {}{}\nEncrypted: {}\nWith: {}\n",
            r.room,
            r.place,
            r.state.as_str(),
            r.message.as_deref().map(|m| format!(" ({m})")).unwrap_or_default(),
            if r.encrypted { "yes, end to end" } else { "no" },
            names(&r.peers)
        ));
        let link = link_for(&r.room);
        if let Some(l) = &link {
            fields.push(text_field(&format!("link{i}"), &format!("Invitation for {}", r.room), l));
        }
        list.push(json!({"room": r.room, "state": r.state.as_str(), "peers": r.peers, "encrypted": r.encrypted, "server": r.place, "link": link}));
    }
    let dialog = if fields.is_empty() { message(title, prompt.trim_end()) } else { form(title, prompt.trim_end(), fields, "dismiss", "Close") };
    json!({"dialog": dialog, "result": {"rooms": list}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_and_checks() {
        let p = json!({"fields": {"name": " Ada ", "encrypt": true}});
        assert_eq!(text(inputs(&p), "name").as_deref(), Some("Ada"));
        assert_eq!(flag(inputs(&p), "encrypt"), Some(true));
        assert_eq!(flag(&json!({"e": "no"}), "e"), Some(false));
        assert_eq!(check_color("3b7dd8").unwrap(), "#3B7DD8");
        assert!(check_color("blue").is_err());
        assert!(check_name("  ").is_err());
        assert!(collab::valid_color(any_color()));
    }
}
