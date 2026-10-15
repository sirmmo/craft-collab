//! The manifest (`plugin.json`) that `--manifest` prints: one for the three apps.

use serde_json::{Value as Json, json};

/// The manifest.
pub fn manifest() -> Json {
    let cmd = |id: &str, label: &str, description: &str, params: &str| json!({"id": id, "label": label, "description": description, "params": params});
    json!({
        "id": "org.craft.collab.loopback",
        "name": "Live (Loopback)",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Live co-editing between apps on this computer, through a local relay: for development and tests. Needs craft plugin protocol 2.",
        "author": "Marco Montanari and the craft-collab contributors",
        "homepage": "https://github.com/sirmmo/craft-collab",
        "apps": ["gridcraft", "wordcraft", "deckcraft"],
        "run": {"command": "./craft-collab-loopback"},
        "collab": {"transport": "loopback", "label": "Live (Loopback)"},
        "commands": [
            cmd("share", "Share Live (Loopback)", "Start a live session between apps on this computer", "{room?, name?, color?, encrypt?} or {fields}"),
            cmd("join", "Join Live (Loopback)", "Join a live session on this computer by room name or invitation", "{link | room, key?, name?, color?} or {fields}"),
            cmd("leave", "Leave Live Session", "Leave the live session (the document stays open)", "{room?}"),
            cmd("status", "Live Status", "The live sessions and who's in them", "{}"),
            cmd("dismiss", "Close", "Close a dialog (from the invitation dialog)", "{}"),
        ],
        "ui": {
            "ribbon": [
                {"tab": "Live", "group": "Loopback", "buttons": [
                    {"command": "share", "icon": "share"},
                    {"command": "join", "icon": "link"},
                    {"command": "leave", "icon": "minus", "size": "small"},
                    {"command": "status", "icon": "info", "size": "small"},
                ]},
            ]
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn buttons_name_commands() {
        let m = super::manifest();
        let ids: Vec<&str> = m["commands"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
        for g in m["ui"]["ribbon"].as_array().unwrap() {
            for b in g["buttons"].as_array().unwrap() {
                assert!(ids.contains(&b["command"].as_str().unwrap()));
            }
        }
        assert_eq!(m["collab"]["transport"], "loopback");
    }

    #[test]
    fn plugin_json_matches() {
        let file: serde_json::Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(file, super::manifest(), "run `craft-collab-loopback --manifest > crates/craft-collab-loopback/plugin.json`");
    }
}
