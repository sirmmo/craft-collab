//! The loopback transport end to end: the conformance kit (plain and encrypted), and the relay
//! moving to another process when the one holding it exits.

use std::time::Duration;

use craft_collab_conformance::gridcraft::Model;
use craft_collab_conformance::harness::{Launch, SimApp, pump};
use craft_collab_conformance::transport::{self, Config, Outcome};
use serde_json::json;

fn launch() -> Launch {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let data = std::env::temp_dir().join(format!("craft-loopback-test-{}", craft_collab_kit::ids::base36(10)));
    Launch::new(env!("CARGO_BIN_EXE_craft-collab-loopback"))
        .env("CRAFT_COLLAB_LOOPBACK_PORT", &port.to_string())
        .env("CRAFT_COLLAB_LOOPBACK_DATA", &data.display().to_string())
        .env("CRAFT_COLLAB_SNAPSHOT_EVERY", "5")
}

fn conformance(encrypted: bool) {
    let mut cfg = Config::new(launch());
    cfg.encrypted = encrypted;
    cfg.require_persistence = true;
    let report = transport::run(&cfg);
    print!("{report}");
    assert!(report.passed(), "{:?}", report.failures());
    assert!(!report.checks.iter().any(|c| matches!(c.outcome, Outcome::Warn(_))), "{report}");
}

#[test]
fn conformance_plain() {
    conformance(false);
}

#[test]
fn conformance_encrypted() {
    conformance(true);
}

fn same(a: &SimApp, b: &SimApp) -> bool {
    !a.app.joining && !b.app.joining && a.app.model().ok() == b.app.model().ok()
}

#[test]
fn the_relay_moves_when_its_process_exits() {
    let l = launch();
    // A hosts the relay; B and C connect to it.
    let mut a = SimApp::start(&l, "A", None).unwrap();
    let res = a.command("share", json!({"room": "handover", "name": "Ada"})).unwrap();
    a.app.load(&Model::sheet("", "Sheet1", &[("A1", "x")]));
    a.join(&res["collab"]["join"]).unwrap();
    let mut b = SimApp::start(&l, "B", None).unwrap();
    let rb = b.command("join", json!({"room": "handover", "name": "Grace"})).unwrap();
    b.join(&rb["collab"]["join"]).unwrap();
    let mut c = SimApp::start(&l, "C", None).unwrap();
    let rc = c.command("join", json!({"link": "craft+loopback:handover", "name": "Lin"})).unwrap();
    c.join(&rc["collab"]["join"]).unwrap();
    assert!(pump(&mut [&mut a, &mut b, &mut c], Duration::from_secs(10), &mut |x| same(x[0], x[1]) && same(x[0], x[2])));

    a.proc.kill();
    // B and C go offline, one of them starts a relay, both come back and re-sync.
    assert!(pump(&mut [&mut b, &mut c], Duration::from_secs(10), &mut |x| x[0].states().contains(&"offline".to_string())));
    b.edit("cell.set", json!({"cell": "B1", "input": "after the handover"})).unwrap();
    c.edit("cell.set", json!({"cell": "C1", "input": "me too"})).unwrap();
    let ok = pump(&mut [&mut b, &mut c], Duration::from_secs(15), &mut |x| same(x[0], x[1]) && x[0].app.model().is_ok_and(|m| m.sheets[0].cells.len() == 3));
    assert!(ok, "B {:?} / C {:?}", b.app.model(), c.app.model());
}

#[test]
fn rooms_and_invitations() {
    let l = launch();
    let mut a = SimApp::start(&l, "A", None).unwrap();
    let dialog = a.command("share", json!({})).unwrap();
    assert_eq!(dialog["dialog"]["kind"], "form");
    let res = a.command("share", json!({"fields": {"room": "dev-x", "name": "Ada", "encrypt": true}})).unwrap();
    let link = res["result"]["link"].as_str().unwrap();
    assert!(link.starts_with("craft+loopback:dev-x#k="), "{link}");
    assert_eq!(res["collab"]["join"]["room"], "loopback:dev-x");
    let mut b = SimApp::start(&l, "B", None).unwrap();
    let j = b.command("join", json!({"fields": {"link": link, "name": "Grace"}})).unwrap();
    assert_eq!(j["collab"]["join"]["key"], res["collab"]["join"]["key"]);
    assert!(b.command("join", json!({"fields": {"link": "bad room!"}})).is_err());
    assert_eq!(a.command("leave", json!({})).unwrap()["message"], "You're not in a live session.");
}
