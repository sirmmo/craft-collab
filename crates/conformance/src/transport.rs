//! Transport conformance: two (then three) simulated apps share a GridCraft document through a
//! transport plugin, and the plugin's behaviour is checked against protocol 2.
//!
//! A transport under test has `share` and `join` commands. `share` with the given params
//! (plus `name`, `color`, `encrypt`) answers `collab.join` and `result.link`; `join {link}` answers
//! `collab.join` for the same room.

// The scenario reads JSON with `value["key"]` (a missing key reads as null, it never panics) and
// indexes the fixed lists of apps it passes to `pump`.
#![allow(clippy::indexing_slicing)]

use std::fmt;
use std::time::Duration;

use craft_collab_kit::{b64, collab};
use serde_json::{Value as Json, json};

use crate::gridcraft::Model;
use crate::harness::{Launch, SimApp, pump};

/// What to test.
#[derive(Clone, Debug)]
pub struct Config {
    pub launch: Launch,
    /// Extra `share` params (`{"broker": "mqtt://127.0.0.1:1883"}`).
    pub share: Json,
    /// Extra `join` params.
    pub join: Json,
    /// Test an end-to-end encrypted room.
    pub encrypted: bool,
    /// How long to wait for the network.
    pub timeout: Duration,
    /// Fail (not just warn) when a newcomer can't get the room's state with nobody online.
    pub require_persistence: bool,
}

impl Config {
    pub fn new(launch: Launch) -> Config {
        Config { launch, share: json!({}), join: json!({}), encrypted: false, timeout: Duration::from_secs(20), require_persistence: false }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Warn(String),
    Skip(String),
    Fail(String),
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub outcome: Outcome,
}

/// The checks, in order.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    fn add(&mut self, name: &str, outcome: Outcome) {
        self.checks.push(Check { name: name.into(), outcome });
    }

    fn check(&mut self, name: &str, ok: bool, why: impl FnOnce() -> String) -> bool {
        self.add(name, if ok { Outcome::Pass } else { Outcome::Fail(why()) });
        ok
    }

    fn warn(&mut self, name: &str, ok: bool, why: impl FnOnce() -> String) {
        self.add(name, if ok { Outcome::Pass } else { Outcome::Warn(why()) });
    }

    /// No failures.
    pub fn passed(&self) -> bool {
        !self.checks.iter().any(|c| matches!(c.outcome, Outcome::Fail(_)))
    }

    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| matches!(c.outcome, Outcome::Fail(_))).collect()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in &self.checks {
            match &c.outcome {
                Outcome::Pass => writeln!(f, "pass  {}", c.name)?,
                Outcome::Warn(w) => writeln!(f, "warn  {}: {w}", c.name)?,
                Outcome::Skip(w) => writeln!(f, "skip  {}: {w}", c.name)?,
                Outcome::Fail(w) => writeln!(f, "FAIL  {}: {w}", c.name)?,
            }
        }
        let n = |p: fn(&Outcome) -> bool| self.checks.iter().filter(|c| p(&c.outcome)).count();
        writeln!(
            f,
            "{} passed, {} warnings, {} skipped, {} failed",
            n(|o| *o == Outcome::Pass),
            n(|o| matches!(o, Outcome::Warn(_))),
            n(|o| matches!(o, Outcome::Skip(_))),
            n(|o| matches!(o, Outcome::Fail(_)))
        )
    }
}

fn merge(mut base: Json, extra: &Json) -> Json {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    base
}

/// Checks a `collab.join`; the reason when it's wrong.
fn check_join(join: &Json, mode: &str, encrypted: bool) -> Result<(), String> {
    let room = join.get("room").and_then(Json::as_str).ok_or("no room")?;
    if !collab::valid_room(room) {
        return Err(format!("bad room id {room:?}"));
    }
    if join.get("mode").and_then(Json::as_str) != Some(mode) {
        return Err(format!("mode isn't {mode}"));
    }
    if !join.get("user").is_some_and(collab::valid_user) {
        return Err(format!("bad user {}", join.get("user").unwrap_or(&Json::Null)));
    }
    if !matches!(join.get("role").and_then(Json::as_str), None | Some("edit") | Some("view")) {
        return Err("role isn't edit or view".into());
    }
    match (join.get("key").and_then(Json::as_str), encrypted) {
        (Some(k), true) if b64::decode(k).is_some_and(|k| k.len() == 32) => Ok(()),
        (Some(_), true) => Err("the key isn't 32 bytes in base64".into()),
        (None, true) => Err("no key for an encrypted room".into()),
        (Some(_), false) => Err("a key for a room that isn't encrypted".into()),
        (None, false) => Ok(()),
    }
}

fn sample() -> Model {
    Model::sheet("", "Sheet1", &[("A1", "Item"), ("B1", "Cost"), ("A2", "Coffee"), ("B2", "3.5")])
}

fn online(a: &SimApp) -> bool {
    a.app.state == "online"
}

fn same(a: &SimApp, b: &SimApp) -> bool {
    !a.app.joining && !b.app.joining && a.app.model().ok() == b.app.model().ok()
}

fn has_peer(a: &SimApp, user_id: &str) -> bool {
    a.peers().iter().any(|p| p["user"]["id"] == user_id)
}

/// Runs every check.
pub fn run(cfg: &Config) -> Report {
    let mut r = Report::default();
    if let Err(e) = scenario(cfg, &mut r) {
        r.add("the scenario ran to the end", Outcome::Fail(e));
    }
    r
}

fn manifest_checks(cfg: &Config, r: &mut Report) {
    let out = std::process::Command::new(&cfg.launch.program).arg("--manifest").envs(cfg.launch.env.iter().map(|(k, v)| (k, v))).output();
    let m = out.ok().and_then(|o| serde_json::from_slice::<Json>(&o.stdout).ok());
    let Some(m) = m else {
        r.add("manifest declares collab", Outcome::Skip("the program doesn't print its manifest with --manifest".into()));
        return;
    };
    let transport = m.get("collab").and_then(|c| c.get("transport")).and_then(Json::as_str).is_some_and(|t| !t.is_empty());
    let label = m.get("collab").and_then(|c| c.get("label")).and_then(Json::as_str).is_some_and(|t| !t.is_empty());
    r.check("manifest declares collab {transport, label}", transport && label, || format!("collab is {}", m.get("collab").unwrap_or(&Json::Null)));
    let ids: Vec<&str> =
        m.get("commands").and_then(Json::as_array).map(|c| c.iter().filter_map(|c| c.get("id").and_then(Json::as_str)).collect()).unwrap_or_default();
    r.check("manifest has share and join commands", ids.contains(&"share") && ids.contains(&"join"), || format!("commands: {ids:?}"));
}

fn scenario(cfg: &Config, r: &mut Report) -> Result<(), String> {
    let t = cfg.timeout;
    manifest_checks(cfg, r);
    let mut a = SimApp::start(&cfg.launch, "A", None)?;
    let mut b = SimApp::start(&cfg.launch, "B", None)?;
    r.add("initialize is answered", Outcome::Pass);

    // Share.
    let share_params = merge(json!({"name": "Ada", "color": "#3B7DD8", "encrypt": cfg.encrypted}), &cfg.share);
    let res = a.command("share", share_params)?;
    let join_a = res.get("collab").and_then(|c| c.get("join")).cloned().unwrap_or(Json::Null);
    if !r.check("share answers collab.join", check_join(&join_a, "share", cfg.encrypted).is_ok(), || {
        format!("{} ({res})", check_join(&join_a, "share", cfg.encrypted).err().unwrap_or_default())
    }) {
        return Ok(());
    }
    let room = join_a["room"].as_str().unwrap_or("").to_string();
    let Some(link) = res.get("result").and_then(|x| x.get("link")).and_then(Json::as_str).map(str::to_string) else {
        r.add("share returns result.link", Outcome::Fail(format!("{res}")));
        return Ok(());
    };
    r.add("share returns result.link", Outcome::Pass);
    a.app.load(&sample());
    a.join(&join_a)?;
    let ok = pump(&mut [&mut a], t, &mut |x| online(x[0]));
    r.check("the sharer's room goes online", ok, || format!("states: {:?}", a.states()));
    r.check("status starts with connecting or online", a.states().first().is_some_and(|s| s == "connecting" || s == "online"), || format!("{:?}", a.states()));

    // Join.
    let res = b.command("join", merge(json!({"link": link, "name": "Grace", "color": "#2E9E5B"}), &cfg.join))?;
    let join_b = res.get("collab").and_then(|c| c.get("join")).cloned().unwrap_or(Json::Null);
    let joined_ok = check_join(&join_b, "join", cfg.encrypted).is_ok() && join_b["room"] == join_a["room"] && join_b.get("key") == join_a.get("key");
    if !r.check("join answers collab.join for the same room and key", joined_ok, || format!("{join_b}")) {
        return Ok(());
    }
    b.join(&join_b)?;
    let ok = pump(&mut [&mut a, &mut b], t, &mut |x| online(x[1]) && same(x[0], x[1]));
    r.check("the joiner gets the document (sync1 → sync2)", ok, || format!("A {:?} / B {:?} (B states {:?})", a.app.model(), b.app.model(), b.states()));
    let b_id = b.app.client_id;
    let from_ok = pump(&mut [&mut a, &mut b], t, &mut |x| x[0].received("collab/sync1").iter().any(|m| m["params"]["from"].as_u64() == Some(b_id)));
    let froms: Vec<Json> = a.received("collab/sync1").iter().map(|m| m["params"]["from"].clone()).collect();
    r.warn("relayed sync1 carries `from`", from_ok, || format!("the app can't address its sync2 answer (sync1 from {froms:?}, B is {b_id})"));
    let b_user = join_b["user"]["id"].as_str().unwrap_or("").to_string();
    let ok = pump(&mut [&mut a, &mut b], t, &mut |x| has_peer(x[0], &b_user));
    r.warn("status lists the peers", ok, || format!("A's last peers: {:?}", a.peers()));

    // Edits both ways, then concurrent ones.
    a.edit("cell.set", json!({"cell": "C1", "input": "Notes"}))?;
    let ok = pump(&mut [&mut a, &mut b], t, &mut |x| same(x[0], x[1]));
    r.check("an edit reaches the other app", ok, || format!("A {:?} / B {:?}", a.app.model(), b.app.model()));
    b.edit("home.insertRows", json!({"rows": "2:2"}))?;
    b.edit("cell.set", json!({"cell": "A2", "input": "Milk"}))?;
    let ok = pump(&mut [&mut a, &mut b], t, &mut |x| same(x[0], x[1]));
    r.check("edits travel both ways", ok, || format!("A {:?} / B {:?}", a.app.model(), b.app.model()));
    for i in 0..20 {
        a.edit("cell.set", json!({"cell": format!("D{}", i + 1), "input": format!("a{i}")}))?;
        b.edit("cell.set", json!({"cell": format!("D{}", i + 1), "input": format!("b{i}")}))?;
        b.edit("cell.set", json!({"cell": format!("E{}", i + 1), "input": format!("b{i}")}))?;
    }
    let ok = pump(&mut [&mut a, &mut b], t, &mut |x| same(x[0], x[1]));
    r.check("concurrent edits converge", ok, || format!("A {:?} / B {:?}", a.app.model(), b.app.model()));
    let failures = a.app.decrypt_failures + b.app.decrypt_failures;
    if cfg.encrypted {
        r.check("ciphertext passes through unchanged", failures == 0, || format!("{failures} messages didn't decrypt"));
    }

    // Awareness.
    if let Some(m) = a.app.awareness_message(json!({"sheet": "s", "ranges": []})) {
        a.proc.send(&m)?;
    }
    let a_id = a.app.client_id;
    let ok = pump(&mut [&mut a, &mut b], t, &mut |x| x[1].app.awareness.contains_key(&a_id));
    r.check("awareness reaches the other app", ok, || "B never saw A's awareness".into());

    // Snapshots.
    let asked = !a.received("collab/snapshot").is_empty() || !b.received("collab/snapshot").is_empty();
    r.add(
        "the transport asks for snapshots",
        if asked { Outcome::Pass } else { Outcome::Skip("no collab/snapshot request (a transport without storage needn't)".into()) },
    );

    // Leave from the transport's command.
    let left = b.command("leave", json!({}));
    let ok = left.is_ok() && pump(&mut [&mut a, &mut b], t, &mut |x| x[1].received("collab/leave").iter().any(|m| m["params"]["room"] == room.as_str()));
    r.check("leave sends collab/leave to the app", ok, || format!("{left:?}"));
    let ok = pump(&mut [&mut a, &mut b], t, &mut |x| !has_peer(x[0], &b_user));
    r.warn("the others see the leaver go", ok, || format!("A's peers: {:?}", a.peers()));
    let before = b.app.model();
    a.edit("cell.set", json!({"cell": "F1", "input": "after B left"}))?;
    pump(&mut [&mut a, &mut b], Duration::from_millis(500), &mut |_| false);
    r.check("nothing reaches an app after it left", b.app.model() == before, || "B still gets updates".into());

    // A newcomer with nobody else online.
    a.proc.notify("collab/leave", json!({"room": room}))?;
    pump(&mut [&mut a], Duration::from_millis(300), &mut |_| false);
    let snapshots: Vec<Model> = a.app.snapshot_models.iter().chain(b.app.snapshot_models.iter()).cloned().collect();
    let mut c = SimApp::start(&cfg.launch, "C", None)?;
    let res = c.command("join", merge(json!({"link": link, "name": "Lin", "color": "#9B51E0"}), &cfg.join))?;
    let join_c = res.get("collab").and_then(|x| x.get("join")).cloned().unwrap_or(Json::Null);
    c.join(&join_c)?;
    let ok = pump(&mut [&mut c], t, &mut |x| !x[0].app.joining);
    let stored = ok && c.app.model().is_ok_and(|m| snapshots.contains(&m));
    let outcome = match (stored, snapshots.is_empty(), cfg.require_persistence) {
        (true, _, _) => Outcome::Pass,
        (false, true, false) => Outcome::Skip("no snapshot was stored".into()),
        (false, _, true) => Outcome::Fail(format!("C got {:?}", c.app.model())),
        (false, false, false) => Outcome::Warn(format!("C got {:?}", c.app.model())),
    };
    r.add("a newcomer gets the stored snapshot with nobody online", outcome);
    let got_snapshot_flag = c.received("collab/sync2").iter().any(|m| m["params"]["snapshot"] == true);
    if stored {
        r.check("a stored snapshot arrives as sync2 with snapshot: true", got_snapshot_flag, || "no sync2 had snapshot: true".into());
    }

    // Bad input.
    c.proc.send_raw("this is not json\n")?;
    c.proc.send_raw("{\"jsonrpc\":\"2.0\",\"method\":\"collab/update\",\"params\":{\"room\":\"nope\",\"update\":\"!!\"}}\n")?;
    c.proc.send_raw("{\"jsonrpc\":\"2.0\",\"method\":\"collab/joined\",\"params\":{}}\n")?;
    c.proc.send_raw(&format!("{}\n", json!({"jsonrpc": "2.0", "method": "collab/update", "params": {"room": join_c["room"], "update": "A".repeat(100)}})))?;
    c.proc.send_raw("{\"jsonrpc\":\"2.0\",\"id\":999999,\"result\":{\"update\":\"AA==\"}}\n")?;
    let answered = c.command("status", json!({}));
    r.check("bad input is dropped and the transport keeps answering", answered.is_ok() && c.proc.alive(), || format!("{answered:?}"));
    let manifest = std::process::Command::new(&cfg.launch.program)
        .arg("--manifest")
        .envs(cfg.launch.env.iter().map(|(k, v)| (k, v)))
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Json>(&o.stdout).ok());
    let ids: Vec<String> = manifest
        .and_then(|m| {
            m.get("commands").and_then(Json::as_array).map(|c| c.iter().filter_map(|c| c.get("id").and_then(Json::as_str).map(str::to_string)).collect())
        })
        .unwrap_or_default();
    let mut all_answer = true;
    for id in ids.iter().filter(|i| !matches!(i.as_str(), "share" | "join")) {
        for p in [json!({}), json!({"fields": 42, "room": [1], "link": {"x": 1}, "name": 7})] {
            let ans = c.call("commands/run", json!({"command": id, "params": p, "context": null}), Duration::from_secs(10));
            all_answer &= ans.is_ok();
        }
    }
    for id in ["share", "join"] {
        let ans = c.call("commands/run", json!({"command": id, "params": {"fields": {"link": 5, "broker": [], "name": ""}}}), Duration::from_secs(10));
        all_answer &= ans.is_ok();
    }
    r.check("every command answers odd params without crashing", all_answer && c.proc.alive(), || "a command didn't answer".into());

    // Shutdown.
    c.proc.notify("shutdown", json!({}))?;
    r.check("exits on shutdown", c.proc.wait_exit(Duration::from_secs(5)), || "still running 5 s after shutdown".into());
    b.proc.close_stdin();
    r.check("exits when its input closes", b.proc.wait_exit(Duration::from_secs(5)), || "still running 5 s after stdin closed".into());
    Ok(())
}
