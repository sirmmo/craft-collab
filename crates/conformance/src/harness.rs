//! Drives a plugin process over standard input and output as an app does, and a simulated app
//! ([`SimApp`]: a plugin process plus the reference app) that answers the transport by itself.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};

use crate::refapp::RefApp;

/// How to start a plugin.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Working directory (the plugin's folder).
    pub dir: Option<PathBuf>,
}

impl Launch {
    pub fn new(program: impl Into<PathBuf>) -> Launch {
        Launch { program: program.into(), ..Launch::default() }
    }

    pub fn env(mut self, k: &str, v: &str) -> Launch {
        self.env.push((k.into(), v.into()));
        self
    }

    /// A plugin folder: reads `plugin.json` and runs its `run.command` (relative to the folder
    /// when it holds a `/`).
    pub fn from_dir(dir: &std::path::Path) -> Result<Launch, String> {
        let text = std::fs::read_to_string(dir.join("plugin.json")).map_err(|e| format!("{}: {e}", dir.join("plugin.json").display()))?;
        let m: Json = serde_json::from_str(&text).map_err(|e| format!("plugin.json: {e}"))?;
        let cmd = m.get("run").and_then(|r| r.get("command")).and_then(Json::as_str).ok_or("plugin.json has no run.command")?;
        let program = if cmd.contains('/') || cmd.contains('\\') { dir.join(cmd) } else { PathBuf::from(cmd) };
        let args = m
            .get("run")
            .and_then(|r| r.get("args"))
            .and_then(Json::as_array)
            .map(|a| a.iter().filter_map(Json::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        Ok(Launch { program, args, env: Vec::new(), dir: Some(dir.to_path_buf()) })
    }
}

/// A running plugin.
pub struct PluginProc {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Json>,
    next_id: u64,
    /// What it wrote to standard error.
    pub stderr: Arc<Mutex<Vec<String>>>,
    pub name: String,
}

impl PluginProc {
    pub fn spawn(launch: &Launch, name: &str) -> Result<PluginProc, String> {
        let mut cmd = Command::new(&launch.program);
        cmd.args(&launch.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(d) = &launch.dir {
            cmd.current_dir(d);
        }
        for (k, v) in &launch.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| format!("can't start {}: {e}", launch.program.display()))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match (&mut r).take(80 << 20).read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if let Ok(j) = serde_json::from_slice::<Json>(&line)
                    && tx.send(j).is_err()
                {
                    break;
                }
            }
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let log2 = log.clone();
        let verbose = std::env::var_os("CRAFT_COLLAB_CONFORMANCE_VERBOSE").is_some();
        let tag = name.to_string();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if verbose {
                    eprintln!("[{tag}] {line}");
                }
                if let Ok(mut l) = log2.lock() {
                    l.push(line);
                }
            }
        });
        Ok(PluginProc { child, stdin: Some(stdin), rx, next_id: 1000, stderr: log, name: name.to_string() })
    }

    pub fn send(&mut self, msg: &Json) -> Result<(), String> {
        self.send_raw(&format!("{msg}\n"))
    }

    pub fn send_raw(&mut self, text: &str) -> Result<(), String> {
        let s = self.stdin.as_mut().ok_or("stdin is closed")?;
        s.write_all(text.as_bytes()).and_then(|_| s.flush()).map_err(|e| format!("{}: can't write: {e}", self.name))
    }

    pub fn notify(&mut self, method: &str, params: Json) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    pub fn recv(&mut self, timeout: Duration) -> Option<Json> {
        self.rx.recv_timeout(timeout).ok()
    }

    pub fn try_recv(&mut self) -> Option<Json> {
        self.rx.try_recv().ok()
    }

    /// Sends a request and waits for its answer (the whole message), handing every other
    /// message to `other`.
    pub fn request(&mut self, method: &str, params: Json, timeout: Duration, other: &mut dyn FnMut(&mut PluginProc, Json)) -> Result<Json, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("{}: no answer to {method} in {} s", self.name, timeout.as_secs()));
            }
            let Some(m) = self.recv(left) else { continue };
            if m.get("id").and_then(Json::as_u64) == Some(id) && m.get("method").is_none() {
                return Ok(m);
            }
            other(self, m);
        }
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Whether it exited within `timeout`.
    pub fn wait_exit(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for PluginProc {
    fn drop(&mut self) {
        self.stdin = None;
        if !self.wait_exit(Duration::from_millis(500)) {
            self.kill();
        }
    }
}

/// The host part of `initialize` for a protocol-2 app.
pub fn host(app: &str) -> Json {
    json!({
        "id": app, "name": "Conformance", "version": env!("CARGO_PKG_VERSION"), "language": "en",
        "features": ["dialogs", "open", "events", "document", "push", "collab"],
        "collab": {"formats": ["yjs-v1"], "app": app, "schema": 1, "encryption": ["aes-256-gcm"]}
    })
}

/// A simulated app: a transport process and the reference app, answering each other.
pub struct SimApp {
    pub proc: PluginProc,
    pub app: RefApp,
    /// Every message the transport sent, in order.
    pub log: Vec<Json>,
}

impl SimApp {
    pub fn start(launch: &Launch, name: &str, seed: Option<u64>) -> Result<SimApp, String> {
        let proc = PluginProc::spawn(launch, name)?;
        let mut s = SimApp { proc, app: RefApp::new(seed), log: Vec::new() };
        let init = s.call(
            "initialize",
            json!({"protocolVersion": 1, "host": host("gridcraft"), "plugin": {"id": "conformance", "dir": "."}}),
            Duration::from_secs(10),
        )?;
        if init.get("error").is_some() {
            return Err(format!("{name}: initialize failed: {init}"));
        }
        Ok(s)
    }

    /// Handles one message from the transport.
    pub fn handle(&mut self, msg: Json) {
        let out = self.app.receive(&msg);
        self.log.push(msg);
        for m in out {
            let _ = self.proc.send(&m);
        }
    }

    /// A request to the transport; other messages are handled meanwhile. The whole answer.
    pub fn call(&mut self, method: &str, params: Json, timeout: Duration) -> Result<Json, String> {
        let mut pending = Vec::new();
        let r = self.proc.request(method, params, timeout, &mut |_, m| pending.push(m));
        for m in pending {
            self.handle(m);
        }
        r
    }

    /// `commands/run`: the result, or the error's message.
    pub fn command(&mut self, command: &str, params: Json) -> Result<Json, String> {
        let ctx = json!({"document": {"path": null, "name": "Book1.xlsx", "dirty": false}, "workbook": "Book1", "sheet": "Sheet1"});
        let answer = self.call("commands/run", json!({"command": command, "params": params, "context": ctx}), Duration::from_secs(30))?;
        match (answer.get("result"), answer.get("error")) {
            (Some(r), _) => Ok(r.clone()),
            (_, Some(e)) => Err(e.get("message").and_then(Json::as_str).unwrap_or("error").to_string()),
            _ => Err(format!("a malformed answer: {answer}")),
        }
    }

    /// Joins as a `collab.join` says.
    pub fn join(&mut self, join: &Json) -> Result<(), String> {
        for m in self.app.join(join)? {
            self.proc.send(&m)?;
        }
        Ok(())
    }

    /// Runs an app command and sends what it produces.
    pub fn edit(&mut self, command: &str, params: Json) -> Result<(), String> {
        for m in self.app.run(command, &params)? {
            self.proc.send(&m)?;
        }
        Ok(())
    }

    /// Handles whatever the transport has sent.
    pub fn poll(&mut self) -> usize {
        let mut n = 0;
        while let Some(m) = self.proc.try_recv() {
            self.handle(m);
            n += 1;
        }
        n
    }

    /// The `collab/status` states seen, in order.
    pub fn states(&self) -> Vec<String> {
        self.log
            .iter()
            .filter(|m| m.get("method").and_then(Json::as_str) == Some("collab/status"))
            .filter_map(|m| m.get("params")?.get("state")?.as_str().map(str::to_string))
            .collect()
    }

    /// The peers in the last `collab/status`.
    pub fn peers(&self) -> Vec<Json> {
        self.log
            .iter()
            .rev()
            .find(|m| m.get("method").and_then(Json::as_str) == Some("collab/status"))
            .and_then(|m| m.get("params")?.get("peers")?.as_array().cloned())
            .unwrap_or_default()
    }

    pub fn received(&self, method: &str) -> Vec<&Json> {
        self.log.iter().filter(|m| m.get("method").and_then(Json::as_str) == Some(method)).collect()
    }
}

/// Polls every app until `done` holds (true) or `timeout` passes (false).
pub fn pump(apps: &mut [&mut SimApp], timeout: Duration, done: &mut dyn FnMut(&[&mut SimApp]) -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        for a in apps.iter_mut() {
            a.poll();
        }
        if done(apps) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
