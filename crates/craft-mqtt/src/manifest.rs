//! The manifest (`plugin.json`) that `--manifest` prints: one for the three apps.

use serde_json::{Value as Json, json};

/// The manifest.
pub fn manifest() -> Json {
    let cmd = |id: &str, label: &str, description: &str, params: &str| json!({"id": id, "label": label, "description": description, "params": params});
    json!({
        "id": "org.craft.mqtt",
        "name": "Live (MQTT)",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Live co-editing through an MQTT broker (Mosquitto, EMQX, HiveMQ…), end-to-end encrypted. Needs craft plugin protocol 2.",
        "author": "Marco Montanari and the craft-collab contributors",
        "homepage": "https://github.com/sirmmo/craft-collab",
        "apps": ["gridcraft", "wordcraft", "deckcraft"],
        "run": {"command": "./craft-mqtt"},
        "collab": {"transport": "mqtt", "label": "Live (MQTT)"},
        "commands": [
            cmd("share", "Share Live", "Start a live session on an MQTT broker and get an invitation link", "{broker, username?, password?, name?, color?, encrypt?, room?, prefix?} or {fields}"),
            cmd("join", "Join Live", "Join a live session from its invitation link", "{link, name?, color?, username?, password?} or {fields}"),
            cmd("leave", "Leave Live Session", "Leave the live session (the document stays open)", "{room?}"),
            cmd("status", "Live Status", "The live sessions, who's in them and their invitation links", "{}"),
            cmd("settings", "Live Settings", "Default broker, your name and colour", "{}"),
            cmd("saveSettings", "Save Live Settings", "Store the settings (from the Settings dialog)", "{fields}"),
            cmd("dismiss", "Close", "Close a dialog (from the invitation dialog)", "{}"),
        ],
        "ui": {
            "ribbon": [
                {"tab": "Live", "group": "MQTT", "buttons": [
                    {"command": "share", "icon": "share"},
                    {"command": "join", "icon": "link"},
                    {"command": "leave", "icon": "minus", "size": "small"},
                    {"command": "status", "icon": "info", "size": "small"},
                    {"command": "settings", "icon": "settings", "size": "small"},
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
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
        assert_eq!(m["collab"]["transport"], "mqtt");
    }

    #[test]
    fn plugin_json_matches() {
        let file: serde_json::Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(file, super::manifest(), "run `craft-mqtt --manifest > crates/craft-mqtt/plugin.json`");
    }
}
