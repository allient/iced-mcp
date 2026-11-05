//! Scripts through the real binary: worker processes, the deadline that
//! kills them, cancellation over MCP and `run-script`'s exit status.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_iced-mcp");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iced-mcp-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_script(root: &Path, file: &str, source: &str, extra: &[&str]) -> (i32, String, Duration) {
    let path = root.join(file);
    std::fs::write(&path, source).unwrap();
    let started = Instant::now();
    let output = Command::new(BIN)
        .arg("run-script")
        .arg(&path)
        .arg("--root")
        .arg(root)
        .args(extra)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.code().unwrap_or(-1), text, started.elapsed())
}

/// `run-script` exits 0 when the script finishes, 1 when it fails and 2
/// when the file cannot be read.
#[test]
fn run_script_exit_status_follows_the_outcome() {
    let root = scratch("exit");
    let (code, report, _) = run_script(&root, "ok.luau", "print('hi')\nreturn 42", &[]);
    assert_eq!(code, 0, "{report}");
    assert!(report.starts_with("PASS ok.luau"), "{report}");
    assert!(report.contains("Return: 42"), "{report}");
    let (code, report, _) = run_script(&root, "bad.luau", "local x = 1\nerror('nope')", &[]);
    assert_eq!(code, 1, "{report}");
    assert!(report.contains("Error: bad.luau:2: nope"), "{report}");
    assert!(report.contains("Failure bundle: "), "{report}");
    let output = Command::new(BIN)
        .args(["run-script"])
        .arg(root.join("missing.luau"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let output = Command::new(BIN).args(["run-script"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(root);
}

/// A plain `string.find` over megabytes never returns to the interpreter,
/// so the interrupt cannot stop it: the server kills the worker at the
/// deadline plus the grace period.
#[test]
fn a_builtin_that_ignores_the_deadline_is_killed() {
    let root = scratch("builtin");
    let source = "--!nocheck\n\
        local s = string.rep('a', 4 * 1024 * 1024)\n\
        local p = string.rep('a', 2 * 1024 * 1024) .. 'b'\n\
        return string.find(s, p, 1, true)";
    let (code, report, elapsed) = run_script(&root, "find.luau", source, &["--timeout-ms", "1000"]);
    assert_eq!(code, 1, "{report}");
    assert!(
        elapsed < Duration::from_secs(8),
        "took {elapsed:?}: {report}"
    );
    assert!(
        report.contains("timed out after 1000 ms and its process was killed"),
        "{report}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A minimal MCP client over the server's stdio.
struct Client {
    stdin: std::process::ChildStdin,
    replies: mpsc::Receiver<serde_json::Value>,
    /// Replies read while waiting for another.
    early: Vec<serde_json::Value>,
    child: std::process::Child,
}

impl Client {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(BIN)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    if value.get("id").is_some() && tx.send(value).is_err() {
                        return;
                    }
                }
            }
        });
        let mut client = Self {
            stdin: child.stdin.take().unwrap(),
            replies,
            early: Vec::new(),
            child,
        };
        client.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "test", "version": "0" } }
        }));
        client.wait(0, Duration::from_secs(10));
        client.send(serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        client
    }

    fn send(&mut self, message: serde_json::Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn eval(&mut self, id: u64, source: &str, timeout_ms: u64) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": "script_eval",
                        "arguments": { "source": source, "timeout_ms": timeout_ms } }
        }));
    }

    fn cancel(&mut self, id: u64) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": { "requestId": id, "reason": "test" }
        }));
    }

    /// The reply to `id`, keeping the others for later.
    fn wait(&mut self, id: u64, timeout: Duration) -> Option<serde_json::Value> {
        if let Some(at) = self.early.iter().position(|r| r["id"] == id) {
            return Some(self.early.remove(at));
        }
        let end = Instant::now() + timeout;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match self.replies.recv_timeout(left) {
                Ok(reply) if reply["id"] == id => return Some(reply),
                Ok(reply) => self.early.push(reply),
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn reply_text(reply: &serde_json::Value) -> String {
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Four endless scripts take every slot; a fifth is refused. Cancelling the
/// four frees their slots at once: the scripts really stopped.
#[test]
fn cancelled_scripts_stop_and_free_their_slots() {
    let root = scratch("cancel");
    let mut client = Client::start(&root);
    let endless = "--!nocheck\nwhile true do end";
    for id in 1..=4 {
        client.eval(id, endless, 1_800_000);
    }
    std::thread::sleep(Duration::from_millis(500));
    client.eval(5, "return 1", 10_000);
    let refused = client.wait(5, Duration::from_secs(10)).expect("a reply");
    assert!(
        reply_text(&refused).contains("too many scripts running: at most 4 at once"),
        "{refused}"
    );

    let cancelled = Instant::now();
    for id in 1..=4 {
        client.cancel(id);
    }
    // Every slot comes back within a moment of the cancellation.
    let mut freed = None;
    for attempt in 0..40 {
        client.eval(100 + attempt, "return 'free'", 10_000);
        let reply = client
            .wait(100 + attempt, Duration::from_secs(10))
            .expect("a reply");
        if reply_text(&reply).starts_with("PASS") {
            freed = Some(cancelled.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let freed = freed.expect("the cancelled scripts never released their slots");
    assert!(freed < Duration::from_secs(2), "took {freed:?}");
    // All four slots: four scripts at once run.
    for id in 200..204 {
        client.eval(id, "return app:sleep(500)", 10_000);
    }
    for id in 200..204 {
        let reply = client.wait(id, Duration::from_secs(10)).expect("a reply");
        assert!(reply_text(&reply).starts_with("PASS"), "{reply}");
    }
    drop(client);
    let _ = std::fs::remove_dir_all(root);
}
