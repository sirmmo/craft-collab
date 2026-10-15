//! The committed transcripts are the generated ones, and the reference app passes them.

use craft_collab_conformance::gridcraft::GridCraft;
use craft_collab_conformance::refapp::RefApp;
use craft_collab_conformance::transcript;

fn dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

#[test]
fn committed_transcripts_are_up_to_date() {
    for (name, text) in transcript::generate() {
        let committed = std::fs::read_to_string(dir().join(&name)).unwrap_or_default();
        assert!(committed == text, "{name} is out of date: run `cargo run -p craft-collab-conformance -- generate conformance`");
    }
}

#[test]
fn committed_transcripts_pass() {
    let mut n = 0;
    for entry in std::fs::read_dir(dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            let lines = transcript::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for seed in [1, 2, 3] {
                if let Err(e) = transcript::replay_reference(&lines, Some(seed)) {
                    panic!("{}: {e}", path.display());
                }
            }
            n += 1;
        }
    }
    assert!(n >= 15);
}

#[test]
fn a_broken_app_fails_the_transcripts() {
    // An app that forgets to send sync1 when it comes online again must fail.
    let text = std::fs::read_to_string(dir().join("gridcraft-1-offline.jsonl")).unwrap();
    let mut lines = transcript::parse(&text).unwrap();
    // Expect an update the app never sends.
    lines.push(serde_json::json!({"t": "out", "msg": {"method": "collab/update", "params": {"room": "conformance:offline", "update": "$b64"}}}));
    assert!(transcript::replay(&lines, &mut RefApp::<GridCraft>::new(Some(1))).is_err());
    // A wrong model fails.
    let mut lines = transcript::parse(&text).unwrap();
    lines.push(serde_json::json!({"t": "model", "model": {"title": "", "sheets": []}}));
    assert!(transcript::replay(&lines, &mut RefApp::<GridCraft>::new(Some(1))).is_err());
}
