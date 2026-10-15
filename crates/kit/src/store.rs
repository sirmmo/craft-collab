//! Settings on disk: one JSON file per kind in the plugin's data folder, shared by the three
//! apps. `<ENV>` (for example `CRAFT_MQTT_DATA`) when set, else `$XDG_DATA_HOME/<name>`
//! (`~/.local/share/<name>`) on Linux and BSD, `~/Library/Application Support/<name>` on macOS,
//! `%APPDATA%\<name>` on Windows.

use std::path::PathBuf;

use serde_json::{Value as Json, json};

/// Largest settings file read.
const MAX_FILE: u64 = 4 << 20;

/// The data folder of a plugin.
pub fn data_dir(name: &str, env_var: &str) -> Result<PathBuf, String> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(p) = env(env_var) {
        return Ok(p);
    }
    let base = if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local").join("share")))
    };
    base.map(|b| b.join(name)).ok_or_else(|| "can't find a folder for the plugin's data (no HOME or APPDATA)".to_string())
}

/// JSON files in a folder.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// The plugin's data folder (not created until something is saved).
    pub fn open(name: &str, env_var: &str) -> Result<Store, String> {
        data_dir(name, env_var).map(Store::new)
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// `<name>.json` as an object; `{}` when it's missing or unreadable.
    pub fn load(&self, name: &str) -> Json {
        use std::io::Read;
        let path = self.dir.join(format!("{name}.json"));
        let mut text = Vec::new();
        let read = std::fs::File::open(&path).and_then(|f| f.take(MAX_FILE).read_to_end(&mut text));
        match read {
            Ok(_) => serde_json::from_slice::<Json>(&text).ok().filter(Json::is_object).unwrap_or_else(|| json!({})),
            Err(_) => json!({}),
        }
    }

    /// Writes `<name>.json` (through a temporary file, so a crash never leaves half a file).
    pub fn save(&self, name: &str, value: &Json) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("can't create {}: {e}", self.dir.display()))?;
        let path = self.dir.join(format!("{name}.json"));
        let tmp = self.dir.join(format!(".{name}.json.tmp"));
        let text = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("can't write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads() {
        let dir = std::env::temp_dir().join(format!("craft-collab-store-{}", crate::ids::base36(8)));
        let s = Store::new(dir.clone());
        assert_eq!(s.load("settings"), json!({}));
        s.save("settings", &json!({"name": "Ada"})).unwrap();
        assert_eq!(s.load("settings"), json!({"name": "Ada"}));
        std::fs::write(dir.join("bad.json"), "[1").unwrap();
        assert_eq!(s.load("bad"), json!({}));
        let _ = std::fs::remove_dir_all(dir);
    }
}
