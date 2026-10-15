//! `craft-collab-conformance`: checks transports and transcripts against craft plugin protocol 2.
//!
//! ```text
//! craft-collab-conformance transport [--encrypted] [--share JSON] [--join JSON] [--env K=V]...
//!                                    [--timeout SECS] [--require-persistence] (--dir PLUGIN_DIR | -- PROGRAM ARGS...)
//! craft-collab-conformance transcripts [DIR]     replays DIR/*.jsonl (default conformance/) with the reference app
//! craft-collab-conformance generate [DIR]        writes the transcripts into DIR (default conformance/)
//! ```
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use craft_collab_conformance::harness::Launch;
use craft_collab_conformance::{transcript, transport};

const USAGE: &str = "usage:
  craft-collab-conformance transport [--encrypted] [--share JSON] [--join JSON] [--env K=V]... [--timeout SECS]
                                     [--require-persistence] (--dir PLUGIN_DIR | -- PROGRAM ARGS...)
  craft-collab-conformance transcripts [DIR]
  craft-collab-conformance generate [DIR]";

fn transport_cmd(args: &[String]) -> Result<bool, String> {
    let mut launch: Option<Launch> = None;
    let mut env = Vec::new();
    let mut cfg_share = serde_json::json!({});
    let mut cfg_join = serde_json::json!({});
    let mut encrypted = false;
    let mut persistence = false;
    let mut timeout = 20u64;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--encrypted" => encrypted = true,
            "--require-persistence" => persistence = true,
            "--share" => cfg_share = serde_json::from_str(&value("--share")?).map_err(|e| format!("--share: {e}"))?,
            "--join" => cfg_join = serde_json::from_str(&value("--join")?).map_err(|e| format!("--join: {e}"))?,
            "--timeout" => timeout = value("--timeout")?.parse().map_err(|_| "--timeout takes seconds".to_string())?,
            "--env" => {
                let kv = value("--env")?;
                let (k, v) = kv.split_once('=').ok_or("--env takes K=V")?;
                env.push((k.to_string(), v.to_string()));
            }
            "--dir" => launch = Some(Launch::from_dir(&PathBuf::from(value("--dir")?))?),
            "--" => {
                let rest: Vec<String> = it.by_ref().cloned().collect();
                let (program, args) = rest.split_first().ok_or("-- needs a program")?;
                launch = Some(Launch { program: program.into(), args: args.to_vec(), env: Vec::new(), dir: None });
            }
            other => return Err(format!("unknown option {other}\n{USAGE}")),
        }
    }
    let mut launch = launch.ok_or_else(|| format!("name the transport with --dir or --\n{USAGE}"))?;
    launch.env.extend(env);
    let mut cfg = transport::Config::new(launch);
    cfg.share = cfg_share;
    cfg.join = cfg_join;
    cfg.encrypted = encrypted;
    cfg.require_persistence = persistence;
    cfg.timeout = Duration::from_secs(timeout);
    let report = transport::run(&cfg);
    print!("{report}");
    Ok(report.passed())
}

fn transcripts_cmd(dir: PathBuf) -> Result<bool, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    let mut ok = true;
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let result = transcript::parse(&text).and_then(|lines| transcript::replay_reference(&lines, None));
        match result {
            Ok(()) => println!("pass  {}", f.display()),
            Err(e) => {
                ok = false;
                println!("FAIL  {}: {e}", f.display());
            }
        }
    }
    Ok(ok)
}

fn generate_cmd(dir: PathBuf) -> Result<bool, String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in transcript::generate() {
        let path = dir.join(&name);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(true)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = |i: usize| args.get(i).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("conformance"));
    let result = match args.first().map(String::as_str) {
        Some("transport") => transport_cmd(args.get(1..).unwrap_or_default()),
        Some("transcripts") => transcripts_cmd(dir(1)),
        Some("generate") => generate_cmd(dir(1)),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
