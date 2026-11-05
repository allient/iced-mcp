//! Luau scripts: one script instead of many tool calls (`script_eval`,
//! `iced-mcp run-script`, `.iced-mcp/scripts/*.luau` in `run-suite`).
//!
//! **Sandbox.** A fresh Luau VM per script, in Luau's sandbox mode (read-only
//! globals and libraries), with only `coroutine`, `table`, `string`, `utf8`,
//! `bit32`, `math` and `buffer`: no `io`, `os`, `debug`, `require`,
//! `loadstring`, `getfenv`/`setfenv`, files or network. The only way out is
//! the `app` / `apps` globals, which reach the connected apps through the
//! same step functions as `run_sequence` and flows ([`ops::run_step`]). A
//! wall-clock deadline is enforced by Luau's interrupt (loops and calls),
//! around every app call and, as a last resort, by killing the worker
//! process; memory, output and the return value are capped.
//!
//! **Process.** The VM runs in a child process, `iced-mcp script-worker`
//! ([`worker`]), which has no app and no files: each `app:method(...)` is a
//! message to the server, which runs the step (locking the shared [`Apps`]
//! for that one call, so other MCP requests interleave between a script's
//! steps) and sends back the result. The server waits on the worker's
//! messages asynchronously and kills the process at the deadline plus
//! [`GRACE`], or as soon as the request is cancelled, whatever the script
//! is doing: even a built-in call that never returns to the interpreter
//! (a plain `string.find` over megabytes) cannot outlive its deadline.
//! A thread could not be stopped there, only abandoned, still burning a
//! core.

pub(crate) mod worker;

use crate::bundle;
use crate::flow::{self, FlowStep};
use crate::ops::{self, App, Ctx};
use crate::params::*;
use crate::tools::Apps;
use crate::view::{self, Selector};

use serde::Serialize;
use std::fmt::Write as _;
use std::future::Future;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use worker::{ToHost, ToWorker};

/// The typed API reference (`script_api`), also what scripts are checked
/// against.
pub(crate) const API: &str = include_str!("../iced-mcp.d.luau");

/// The methods of `app`. The `.d.luau` declares exactly these (tested).
pub(crate) const METHODS: &[&str] = &[
    "click",
    "type_text",
    "press_key",
    "scroll",
    "drag",
    "hover",
    "paste",
    "wait_for",
    "wait_idle",
    "sleep",
    "run_app_action",
    "query",
    "load_fixture",
    "fixture",
    "messages",
    "state",
    "logs",
    "snapshot",
    "find",
    "find_all",
    "diagnose",
    "screenshot",
    "screenshot_diff",
    "expect",
    "expect_pixel",
    "expect_no_diagnostics",
    "expect_message",
    "expect_query",
];

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const MAX_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub(crate) const DEFAULT_MEMORY: usize = 64 * 1024 * 1024;
pub(crate) const DEFAULT_OUTPUT: usize = 64 * 1024;
/// `sleep` waits at most this long, as the step does.
const MAX_SLEEP_MS: u64 = 30_000;
/// The environment variable naming the type checker.
pub(crate) const ENV_ANALYZE: &str = "ICED_MCP_LUAU_ANALYZE";
const ANALYZE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long past its deadline a script's worker may take to stop on its
/// own before it is killed.
pub(crate) const GRACE: Duration = Duration::from_secs(2);
/// The largest return value, as JSON bytes.
pub(crate) const RETURN_LIMIT: usize = 1024 * 1024;
/// The longest message a worker may send.
const MAX_LINE: usize = 2 * RETURN_LIMIT;
/// Scripts `script_eval` runs at once.
pub(crate) const MAX_SCRIPTS: usize = 4;
/// The hidden subcommand that runs a worker.
pub(crate) const WORKER_COMMAND: &str = "script-worker";

/// Where a script's VM runs.
#[derive(Debug, Clone)]
pub(crate) enum Worker {
    /// `<exe> script-worker`: the server's own binary.
    Process(PathBuf),
    /// A thread in this process (unit tests, whose binary is the test
    /// harness): it cannot be killed.
    #[cfg(test)]
    Thread,
}

/// What to run and within which limits.
#[derive(Debug, Clone)]
pub(crate) struct Script {
    /// The chunk name errors are reported under: `script`, or a file name.
    pub(crate) name: String,
    pub(crate) source: String,
    /// The app `app` means; the default app when `None`.
    pub(crate) app: Option<String>,
    pub(crate) timeout: Duration,
    pub(crate) memory: usize,
    pub(crate) output: usize,
    pub(crate) update_baselines: bool,
    /// Skip `luau-analyze` (tests).
    pub(crate) typecheck: bool,
    pub(crate) worker: Worker,
}

impl Script {
    pub(crate) fn new(name: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            app: None,
            timeout: DEFAULT_TIMEOUT,
            memory: DEFAULT_MEMORY,
            output: DEFAULT_OUTPUT,
            update_baselines: false,
            typecheck: true,
            worker: Worker::Process(
                std::env::current_exe().unwrap_or_else(|_| PathBuf::from("iced-mcp")),
            ),
        }
    }
}

impl Script {
    /// A script from a `.luau` file, named after the file.
    pub(crate) fn from_file(path: &Path) -> Result<Self, String> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("script")
            .to_owned();
        Ok(Self::new(name, source))
    }
}

pub(crate) const USAGE: &str = "\
iced-mcp run-script FILE      run a Luau script (see `script_api` / iced-mcp.d.luau)
    --launch CMD              start CMD and run the script on it (`app`), stop it after
    --root DIR                project root holding .iced-mcp/ (default: current dir)
    --timeout-ms N            wall-clock limit (default 60000)
    --update-baselines        rewrite screenshot_diff baselines from this run
Type-checked first with luau-analyze when it is on PATH (or ICED_MCP_LUAU_ANALYZE).
Exit status: 0 when the script finished, 1 when it failed (an error, a failed step,
a type error, a launch that failed), 2 on bad arguments or an unreadable file.";

/// `run-script`'s options, from the command line.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct CliOptions {
    pub(crate) file: PathBuf,
    pub(crate) launch: Option<String>,
    pub(crate) root: Option<PathBuf>,
    pub(crate) timeout_ms: Option<u64>,
    pub(crate) update_baselines: bool,
}

pub(crate) fn parse_cli(args: &[String]) -> Result<CliOptions, String> {
    let mut options = CliOptions::default();
    let mut file = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--launch" => options.launch = Some(value("--launch")?),
            "--root" => options.root = Some(PathBuf::from(value("--root")?)),
            "--timeout-ms" => {
                let text = value("--timeout-ms")?;
                options.timeout_ms =
                    Some(text.parse::<u64>().ok().filter(|n| *n > 0).ok_or_else(|| {
                        format!("--timeout-ms needs a positive number, not `{text}`")
                    })?);
            }
            "--update-baselines" => options.update_baselines = true,
            path if !path.starts_with('-') && file.is_none() => file = Some(PathBuf::from(path)),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    options.file = file.ok_or("give the script file: iced-mcp run-script FILE")?;
    Ok(options)
}

/// One app call the script made.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StepLog {
    pub(crate) line: Option<usize>,
    pub(crate) app: Option<String>,
    pub(crate) action: String,
    pub(crate) label: String,
    pub(crate) ok: bool,
    pub(crate) ms: u64,
}

/// Why a script stopped.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScriptError {
    pub(crate) message: String,
    pub(crate) line: Option<usize>,
}

/// A static error `luau-analyze` found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct TypeError {
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) message: String,
}

/// What the type checker said.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum TypeCheck {
    Passed,
    Failed {
        errors: Vec<TypeError>,
    },
    /// Not run: why (no `luau-analyze`, or it failed).
    Unavailable {
        reason: String,
    },
    Skipped,
}

/// The result of a script.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Outcome {
    pub(crate) ok: bool,
    pub(crate) output: String,
    pub(crate) value: serde_json::Value,
    pub(crate) steps: Vec<StepLog>,
    pub(crate) duration_ms: u64,
    pub(crate) error: Option<ScriptError>,
    pub(crate) typecheck: TypeCheck,
    pub(crate) bundle: Option<PathBuf>,
}

impl Outcome {
    /// The report agents and the CLI read.
    pub(crate) fn render(&self, name: &str) -> String {
        let mut out = String::new();
        let verdict = if self.ok { "PASS" } else { "FAIL" };
        let _ = writeln!(
            out,
            "{verdict} {name} — {} step(s) in {:.1}s",
            self.steps.len(),
            self.duration_ms as f64 / 1000.0
        );
        match &self.typecheck {
            TypeCheck::Passed => out.push_str("Type check: passed (luau-analyze).\n"),
            TypeCheck::Failed { errors } => {
                let _ = writeln!(
                    out,
                    "Type check: {} error(s); the app was not touched.",
                    errors.len()
                );
                for e in errors {
                    let _ = writeln!(out, "  {name}:{}:{}: {}", e.line, e.column, e.message);
                }
            }
            TypeCheck::Unavailable { reason } => {
                let _ = writeln!(out, "Type check: not run ({reason}); runtime errors only.");
            }
            TypeCheck::Skipped => {}
        }
        if let Some(error) = &self.error {
            let _ = writeln!(out, "Error: {}", error.message);
        }
        if !self.steps.is_empty() {
            out.push_str("Steps:\n");
            for (i, step) in self.steps.iter().enumerate() {
                let line = step.line.map(|l| format!("line {l}: ")).unwrap_or_default();
                let _ = writeln!(
                    out,
                    "  {}. {} {line}{} ({} ms)",
                    i + 1,
                    if step.ok { "✓" } else { "✗" },
                    step.label,
                    step.ms
                );
            }
        }
        if !self.output.is_empty() {
            let _ = write!(out, "Output:\n{}", self.output);
            if !self.output.ends_with('\n') {
                out.push('\n');
            }
        }
        if !self.value.is_null() {
            let _ = writeln!(out, "Return: {}", ops::clip_json(&self.value, 4000));
        }
        if let Some(path) = &self.bundle {
            let _ = writeln!(out, "Failure bundle: {}", path.display());
        }
        out.trim_end().to_owned()
    }
}

/// Type-checks, then runs `script` against `apps`. On a failure, writes a
/// bundle from the app the script last used. `cancel` resolving stops it.
pub(crate) async fn run(apps: Arc<Mutex<Apps>>, script: Script, cancel: Option<Cancel>) -> Outcome {
    match precheck(&script).await {
        Ok(typecheck) => run_checked(apps, script, typecheck, cancel).await,
        Err(failed) => *failed,
    }
}

/// Type-checks `script` (unless told not to): the check's result, or the
/// failed outcome to report without touching any app.
pub(crate) async fn precheck(script: &Script) -> Result<TypeCheck, Box<Outcome>> {
    let started = Instant::now();
    let typecheck = if script.typecheck {
        typecheck(&script.source).await
    } else {
        TypeCheck::Skipped
    };
    if let TypeCheck::Failed { errors } = &typecheck {
        let first = errors.first();
        return Err(Box::new(Outcome {
            ok: false,
            output: String::new(),
            value: serde_json::Value::Null,
            steps: Vec::new(),
            duration_ms: elapsed_ms(started),
            error: Some(ScriptError {
                message: match first {
                    Some(e) => format!("{}:{}: type error: {}", script.name, e.line, e.message),
                    None => "type error".into(),
                },
                line: first.map(|e| e.line),
            }),
            typecheck,
            bundle: None,
        }));
    }
    Ok(typecheck)
}

/// Something that resolves when the caller gives up (an MCP request was
/// cancelled).
pub(crate) type Cancel = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Runs a script [`precheck`] passed: starts its worker, answers its app
/// calls, and kills it at the deadline plus [`GRACE`] or on `cancel`,
/// whatever it is doing.
pub(crate) async fn run_checked(
    apps: Arc<Mutex<Apps>>,
    script: Script,
    typecheck: TypeCheck,
    cancel: Option<Cancel>,
) -> Outcome {
    let started = Instant::now();
    let deadline = started + script.timeout;
    let chunk = script.name.clone();
    let host = Host {
        apps: apps.clone(),
        deadline,
        timeout: script.timeout,
        default_app: script.app.clone(),
        update_baselines: script.update_baselines,
    };
    let mut output = Output::new(script.output);
    let mut steps: Vec<StepLog> = Vec::new();
    let mut last_app: Option<String> = None;
    let mut cancel = cancel.unwrap_or_else(|| Box::pin(std::future::pending()));
    let hard_deadline = tokio::time::sleep_until((deadline + GRACE).into());
    tokio::pin!(hard_deadline);
    let killed = format!(
        "{chunk}: the script timed out after {} ms and its process was killed: it was busy in a built-in call that does not check the deadline (a huge string.find, a backtracking pattern…)",
        script.timeout.as_millis()
    );
    let cancelled = format!("{chunk}: the script was cancelled");

    let result: Result<serde_json::Value, String> = match Session::spawn(&script.worker) {
        Err(error) => Err(format!("{chunk}: cannot start the script worker: {error}")),
        Ok(mut session) => {
            let start = worker::Start {
                name: script.name.clone(),
                source: script.source.clone(),
                app: script.app.clone(),
                memory: script.memory,
                output: script.output,
                return_limit: RETURN_LIMIT,
                timeout_ms: u64::try_from(script.timeout.as_millis()).unwrap_or(u64::MAX),
                methods: METHODS.iter().map(|m| (*m).to_owned()).collect(),
            };
            session.send(&start);
            let result = loop {
                let message = tokio::select! {
                    message = session.rx.recv() => message,
                    () = &mut hard_deadline => break Err(killed.clone()),
                    () = &mut cancel => break Err(cancelled.clone()),
                };
                match message {
                    None => break Err(format!("{chunk}: {}", session.exit_reason())),
                    Some(Err(error)) => break Err(format!("{chunk}: {error}")),
                    Some(Ok(ToHost::Print(text))) => output.push(&text),
                    Some(Ok(ToHost::Done(value))) => break Ok(value),
                    Some(Ok(ToHost::Failed(message))) => break Err(message),
                    Some(Ok(ToHost::Call {
                        app,
                        method,
                        line,
                        args,
                    })) => {
                        let call_started = Instant::now();
                        let mut logged = None;
                        let reply = tokio::select! {
                            reply = host.call(app.as_deref(), &method, args, &mut logged, &mut last_app) => reply,
                            () = &mut cancel => break Err(cancelled.clone()),
                        };
                        if let Some((action, args)) = logged {
                            steps.push(StepLog {
                                line,
                                app: last_app.clone(),
                                action: method.clone(),
                                label: flow::label(&FlowStep { action, args }),
                                ok: reply.is_ok(),
                                ms: elapsed_ms(call_started),
                            });
                        }
                        session.send(&match reply {
                            Ok(value) => ToWorker::Ok(value),
                            Err(message) => ToWorker::Err(match line {
                                Some(line) => format!("{chunk}:{line}: {message}"),
                                None => format!("{chunk}: {message}"),
                            }),
                        });
                    }
                }
            };
            session.stop();
            result
        }
    };
    let (value, error) = match result {
        Ok(value) => (value, None),
        Err(message) => (
            serde_json::Value::Null,
            Some(ScriptError {
                line: line_of(&message, &chunk),
                message,
            }),
        ),
    };

    let mut bundle_path = None;
    if let Some(error) = &error {
        let mut apps = apps.lock().await;
        let root = apps.root().to_path_buf();
        let target = last_app.or_else(|| script.app.clone());
        let step = match (error.line, steps.iter().rev().find(|s| !s.ok)) {
            (Some(line), Some(step)) => format!("line {line}: {}", step.label),
            (Some(line), None) => format!("line {line}"),
            (None, _) => "script".to_owned(),
        };
        let failure = bundle::Failure {
            flow: &bundle_name(&script.name),
            step: &step,
            error: &error.message,
        };
        let app = apps.get(target.as_deref()).ok();
        bundle_path = bundle::write(&root, &failure, bundle::Source::Script(&script.source), app)
            .await
            .ok();
    }
    let outcome = Outcome {
        ok: error.is_none(),
        output: output.text,
        value,
        steps,
        duration_ms: elapsed_ms(started),
        error,
        typecheck,
        bundle: bundle_path,
    };
    if let Some(path) = &outcome.bundle {
        bundle::add_report(path, &outcome.render(&script.name));
    }
    outcome
}

/// A file-safe bundle name for a script: `script-<stem>`.
fn bundle_name(chunk: &str) -> String {
    let stem = chunk.strip_suffix(".luau").unwrap_or(chunk);
    let safe: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    if safe == "script" {
        safe
    } else {
        format!("script-{safe}")
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// What the script printed, cut at its limit.
struct Output {
    text: String,
    limit: usize,
    cut: bool,
}

impl Output {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
            cut: false,
        }
    }

    fn push(&mut self, line: &str) {
        if self.cut {
            return;
        }
        if self.text.len() + line.len() + 1 > self.limit {
            let room = self.limit.saturating_sub(self.text.len());
            let mut cut = room.min(line.len());
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            self.text.push_str(&line[..cut]);
            let _ = write!(self.text, "\n… output cut at {} bytes", self.limit);
            self.cut = true;
        } else {
            self.text.push_str(line);
            self.text.push('\n');
        }
    }
}

/// A running worker: its messages, and the way to answer and stop it.
struct Session {
    rx: tokio::sync::mpsc::Receiver<Result<ToHost, String>>,
    writer: Option<Box<dyn Write + Send>>,
    child: Option<std::process::Child>,
}

impl Session {
    fn spawn(worker: &Worker) -> Result<Self, String> {
        let (reader, writer, child): (Box<dyn Read + Send>, Box<dyn Write + Send>, _) = match worker
        {
            Worker::Process(exe) => {
                let mut child = std::process::Command::new(exe)
                    .arg(WORKER_COMMAND)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|e| format!("{}: {e}", exe.display()))?;
                let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                    let _ = child.kill();
                    return Err("no pipes to the worker".into());
                };
                (Box::new(stdout), Box::new(stdin), Some(child))
            }
            #[cfg(test)]
            Worker::Thread => {
                // pipe() is (read end, write end).
                let (worker_reads, server_writes) = std::io::pipe().map_err(|e| e.to_string())?;
                let (server_reads, worker_writes) = std::io::pipe().map_err(|e| e.to_string())?;
                std::thread::spawn(move || {
                    worker::main(std::io::BufReader::new(worker_reads), worker_writes)
                });
                (Box::new(server_reads), Box::new(server_writes), None)
            }
        };
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        std::thread::Builder::new()
            .name("iced-mcp-script-reader".into())
            .spawn(move || read_messages(reader, &tx))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            rx,
            writer: Some(writer),
            child,
        })
    }

    /// Sends one message; a worker that is gone shows up as the end of its
    /// messages.
    fn send(&mut self, message: &impl Serialize) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        let Ok(mut line) = serde_json::to_string(message) else {
            return;
        };
        line.push('\n');
        if writer
            .write_all(line.as_bytes())
            .and_then(|()| writer.flush())
            .is_err()
        {
            self.writer = None;
        }
    }

    fn exit_reason(&mut self) -> String {
        match self.child.as_mut().map(|c| c.wait()) {
            Some(Ok(status)) => format!("the script worker exited unexpectedly ({status})"),
            _ => "the script worker stopped unexpectedly".into(),
        }
    }

    /// Closes its input and kills it if it still runs.
    fn stop(&mut self) {
        self.writer = None;
        if let Some(child) = self.child.as_mut() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reads the worker's lines into `tx`, refusing any longer than [`MAX_LINE`].
fn read_messages(
    reader: Box<dyn Read + Send>,
    tx: &tokio::sync::mpsc::Sender<Result<ToHost, String>>,
) {
    let mut reader = std::io::BufReader::new(reader);
    loop {
        let mut line = Vec::new();
        let limit = u64::try_from(MAX_LINE + 1).unwrap_or(u64::MAX);
        match (&mut reader).take(limit).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let message = if line.len() > MAX_LINE {
            Err(format!(
                "the script sent a message over {} KiB (print or return less)",
                MAX_LINE / 1024
            ))
        } else {
            serde_json::from_slice::<ToHost>(&line)
                .map_err(|e| format!("bad message from the script worker: {e}"))
        };
        let last = message.is_err();
        if tx.blocking_send(message).is_err() || last {
            return;
        }
    }
}

/// What app calls need: the apps, the deadline, the defaults.
struct Host {
    apps: Arc<Mutex<Apps>>,
    deadline: Instant,
    timeout: Duration,
    default_app: Option<String>,
    update_baselines: bool,
}

impl Host {
    fn timed_out(&self) -> String {
        format!(
            "the script timed out after {} ms (raise timeout_ms)",
            self.timeout.as_millis()
        )
    }

    /// Runs `f` on the app `name` means, holding the apps lock for this one
    /// call, within what is left of the deadline.
    async fn with_app<T>(
        &self,
        name: Option<&str>,
        last_app: &mut Option<String>,
        f: impl AsyncFnOnce(&mut App, &Ctx) -> Result<T, String>,
    ) -> Result<T, String> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(self.timed_out());
        }
        let name = name.map(str::to_owned).or_else(|| self.default_app.clone());
        let work = async {
            let mut apps = self.apps.lock().await;
            let ctx = Ctx {
                root: apps.root().to_path_buf(),
                update_baselines: self.update_baselines,
            };
            let app = apps.get(name.as_deref())?;
            *last_app = Some(app.name.clone());
            f(app, &ctx).await
        };
        tokio::time::timeout(left, work)
            .await
            .unwrap_or_else(|_| Err(self.timed_out()))
    }

    /// A step from the shared vocabulary, run through `ops::run_step`; its
    /// reply as a JSON string.
    async fn step(
        &self,
        app: Option<&str>,
        action: &str,
        args: serde_json::Value,
        logged: &mut Option<(String, serde_json::Value)>,
        last_app: &mut Option<String>,
    ) -> Result<serde_json::Value, String> {
        *logged = Some((action.to_owned(), args.clone()));
        let step = Step {
            action: action.to_owned(),
            args,
        };
        let text = self
            .with_app(app, last_app, async |app, ctx| {
                ops::run_step(app, ctx, &step).await
            })
            .await?;
        Ok(serde_json::Value::String(text))
    }

    /// One `app:method(a, b)` call: its result as JSON, or why it failed.
    #[allow(clippy::too_many_lines)]
    async fn call(
        &self,
        app: Option<&str>,
        method: &str,
        args: Vec<serde_json::Value>,
        logged: &mut Option<(String, serde_json::Value)>,
        last_app: &mut Option<String>,
    ) -> Result<serde_json::Value, String> {
        let mut args = args.into_iter();
        let first = args.next().unwrap_or_default();
        let second = args.next().unwrap_or_default();
        match method {
            "click" | "hover" | "type_text" | "paste" | "wait_for" => {
                self.step(app, method, args_or(first, "text")?, logged, last_app)
                    .await
            }
            "press_key" => {
                self.step(app, method, args_or(first, "keys")?, logged, last_app)
                    .await
            }
            "scroll"
            | "drag"
            | "wait_idle"
            | "expect_pixel"
            | "expect_no_diagnostics"
            | "expect_query" => {
                self.step(app, method, args_json(first)?, logged, last_app)
                    .await
            }
            "expect_message" => {
                self.step(app, method, args_or(first, "contains")?, logged, last_app)
                    .await
            }
            "screenshot_diff" => {
                // A baseline is a file name under .iced-mcp/baselines: scripts
                // may not write elsewhere.
                let args = args_or(first, "baseline")?;
                if let Some(name) = args.get("baseline").and_then(serde_json::Value::as_str) {
                    file_name(name, "baseline")?;
                }
                self.step(app, method, args, logged, last_app).await
            }
            "run_app_action" | "load_fixture" | "fixture" => {
                let name = string_arg(&first, "the name")?;
                let action = if method == "fixture" {
                    "load_fixture"
                } else {
                    method
                };
                let mut json = serde_json::json!({ "name": name });
                if !second.is_null() {
                    json["args"] = second;
                }
                self.step(app, action, json, logged, last_app).await
            }
            "expect" => {
                let mut json = args_json(first)?;
                if !second.is_null() {
                    // `expect(node, checks)`: aim at the node by id, else by ref.
                    let node = json;
                    let mut checks = args_json(second)?;
                    let target =
                        match (node.get("id"), node.get("ref")) {
                            (Some(id), _) if id.is_string() => serde_json::json!({ "id": id }),
                            (_, Some(reference)) => serde_json::json!({ "ref": reference }),
                            _ => return Err(
                                "bad args: expect(node, checks) needs a node from find or snapshot"
                                    .into(),
                            ),
                        };
                    if let (Some(checks), Some(target)) =
                        (checks.as_object_mut(), target.as_object())
                    {
                        for (k, v) in target {
                            checks.insert(k.clone(), v.clone());
                        }
                        if let Some(window) = node.get("window") {
                            checks.entry("window").or_insert(window.clone());
                        }
                    }
                    json = checks;
                }
                self.step(app, "expect", json, logged, last_app).await
            }
            "sleep" => {
                let ms = match &first {
                    serde_json::Value::Null => 250,
                    serde_json::Value::Number(n) => n
                        .as_u64()
                        .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
                        .unwrap_or(0),
                    other => {
                        return Err(format!(
                            "bad args: sleep takes milliseconds, got {}",
                            type_name(other)
                        ))
                    }
                }
                .min(MAX_SLEEP_MS);
                *logged = Some(("sleep".into(), serde_json::json!({ "ms": ms })));
                let left = self.deadline.saturating_duration_since(Instant::now());
                let wanted = Duration::from_millis(ms);
                tokio::time::sleep(wanted.min(left)).await;
                if wanted > left {
                    return Err(self.timed_out());
                }
                Ok(serde_json::json!(format!("slept {ms} ms")))
            }
            "query" => {
                let name = string_arg(&first, "the query name")?;
                *logged = Some(("query".into(), serde_json::json!({ "name": name })));
                self.with_app(app, last_app, async |app, _| {
                    ops::query(app, &name, &second).await
                })
                .await
            }
            "snapshot" => {
                let window = match &first {
                    serde_json::Value::Null => None,
                    other => Some(string_arg(other, "the window")?),
                };
                *logged = Some(("snapshot".into(), serde_json::Value::Null));
                let snapshot = self
                    .with_app(app, last_app, async |app, _| {
                        ops::take_snapshot(app, window.as_deref()).await
                    })
                    .await?;
                let nodes: Vec<serde_json::Value> = snapshot.nodes.iter().map(node_json).collect();
                Ok(serde_json::json!({
                    "window": snapshot.window,
                    "width": snapshot.size.width,
                    "height": snapshot.size.height,
                    "nodes": nodes,
                }))
            }
            "find" | "find_all" => {
                let selector = args_or(first, "text")?;
                *logged = Some((method.to_owned(), selector.clone()));
                let get = |key: &str| selector.get(key).and_then(serde_json::Value::as_str);
                let (id, text, role, window) = (get("id"), get("text"), get("role"), get("window"));
                let exact = selector
                    .get("exact")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if id.is_none() && text.is_none() && role.is_none() {
                    return Err("bad args: find needs id, text or role".into());
                }
                let found = self
                    .with_app(app, last_app, async |app, _| {
                        let snapshot = ops::take_snapshot(app, window).await?;
                        Ok(find_nodes(&snapshot, id, text, exact, role))
                    })
                    .await?;
                Ok(if method == "find" {
                    found.into_iter().next().unwrap_or_default()
                } else {
                    serde_json::Value::Array(found)
                })
            }
            "diagnose" => {
                let json = args_json(first)?;
                *logged = Some(("diagnose".into(), json.clone()));
                let p: DiagnoseParams =
                    serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
                let (_, value) = self
                    .with_app(app, last_app, async |app, _| ops::diagnose(app, &p).await)
                    .await?;
                Ok(value["findings"].clone())
            }
            "screenshot" => {
                let mut json = args_or(first, "name")?;
                *logged = Some(("screenshot".into(), json.clone()));
                let name = json
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(
                        "bad args: screenshot needs a name (the file .iced-mcp/screenshots/<name>.png)",
                    )?
                    .to_owned();
                file_name(&name, "screenshot")?;
                let root = self.apps.lock().await.root().to_path_buf();
                let path = screenshots_dir(&root).join(format!("{name}.png"));
                if let Some(map) = json.as_object_mut() {
                    map.remove("name");
                    map.insert("save_to".into(), serde_json::json!(path.to_string_lossy()));
                }
                let p: ScreenshotToolParams =
                    serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
                let (_, text) = self
                    .with_app(app, last_app, async |app, _| ops::screenshot(app, &p).await)
                    .await?;
                Ok(serde_json::json!(text))
            }
            "messages" => {
                let json = args_json(first)?;
                *logged = Some(("messages".into(), serde_json::Value::Null));
                let p: MessagesToolParams =
                    serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
                let log = self
                    .with_app(app, last_app, async |app, _| {
                        ops::read_messages(
                            app,
                            p.since,
                            p.filter.clone(),
                            Some(p.limit.unwrap_or(30)),
                        )
                        .await
                    })
                    .await?;
                serde_json::to_value(&log.entries).map_err(|e| e.to_string())
            }
            "state" => {
                let json = args_json(first)?;
                *logged = Some(("state".into(), serde_json::Value::Null));
                let p: StateToolParams =
                    serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
                let text = self
                    .with_app(app, last_app, async |app, _| ops::state(app, &p).await)
                    .await?;
                Ok(serde_json::json!(text))
            }
            "logs" => {
                let json = args_json(first)?;
                *logged = Some(("logs".into(), serde_json::Value::Null));
                let p: LogsParams =
                    serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
                let lines = self
                    .with_app(app, last_app, async |app, _| match &app.launched {
                        Some(launched) => Ok(launched
                            .logs
                            .read(
                                p.since,
                                p.tail.unwrap_or(60),
                                p.max_chars.unwrap_or(8000),
                                p.filter.as_deref(),
                            )
                            .lines),
                        None => Err(format!(
                            "{} was connected, not launched here: its output is not captured",
                            app.name
                        )),
                    })
                    .await?;
                Ok(serde_json::json!(lines))
            }
            other => Err(format!("unknown method `{other}`")),
        }
    }
}

/// A file name scripts may write under `.iced-mcp/`: as flow names are.
fn file_name(name: &str, what: &str) -> Result<(), String> {
    flow::check_name(name).map_err(|e| e.replacen("flow name", &format!("{what} name"), 1))
}

/// The Lua type a JSON argument came from, for error messages.
fn type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "nil",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => "table",
    }
}

/// A method's table argument (`nil` → `{}`).
fn args_json(value: serde_json::Value) -> Result<serde_json::Value, String> {
    match value {
        serde_json::Value::Null => Ok(serde_json::json!({})),
        serde_json::Value::Array(list) if list.is_empty() => Ok(serde_json::json!({})),
        table @ (serde_json::Value::Object(_) | serde_json::Value::Array(_)) => Ok(table),
        other => Err(format!(
            "bad args: expected a table, got {}",
            type_name(&other)
        )),
    }
}

/// A table, or a string standing for `{ [key] = string }`.
fn args_or(value: serde_json::Value, key: &str) -> Result<serde_json::Value, String> {
    match value {
        serde_json::Value::String(s) => Ok(serde_json::json!({ key: s })),
        other => args_json(other),
    }
}

fn string_arg(value: &serde_json::Value, what: &str) -> Result<String, String> {
    match value {
        serde_json::Value::String(s) => Ok(s.clone()),
        other => Err(format!(
            "bad args: {what} must be a string, got {}",
            type_name(other)
        )),
    }
}

/// The line number in `chunk:N: …`.
pub(crate) fn line_of(message: &str, chunk: &str) -> Option<usize> {
    let prefix = format!("{chunk}:");
    let start = message.find(&prefix)? + prefix.len();
    let digits: String = message[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// A node as a Lua-ready JSON object, with `ref` beside `index`.
fn node_json(node: &iced_mcp_protocol::Node) -> serde_json::Value {
    let mut value = serde_json::to_value(node).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.insert("ref".into(), serde_json::json!(node.index));
    }
    value
}

/// Where `app:screenshot` writes: `<root>/.iced-mcp/screenshots`.
pub(crate) fn screenshots_dir(root: &Path) -> PathBuf {
    root.join(".iced-mcp").join("screenshots")
}

/// The visible nodes matching every given criterion, best first (as
/// targets resolve) when `id`/`text` is given, else in tree order.
fn find_nodes(
    snapshot: &iced_mcp_protocol::Snapshot,
    id: Option<&str>,
    text: Option<&str>,
    exact: bool,
    role: Option<&str>,
) -> Vec<serde_json::Value> {
    let candidates: Vec<&iced_mcp_protocol::Node> = match (id, text) {
        (Some(id), _) => view::find(snapshot, &Selector::Id(id)),
        (None, Some(text)) => view::find(snapshot, &Selector::Text { text, exact }),
        (None, None) => snapshot
            .nodes
            .iter()
            .filter(|n| n.visible.is_some())
            .collect(),
    };
    candidates
        .into_iter()
        .filter(|node| text.is_none_or(|t| view::selects(node, &Selector::Text { text: t, exact })))
        .filter(|node| {
            role.is_none_or(|role| {
                node.role.as_deref() == Some(role)
                    || serde_json::to_value(node.kind)
                        .ok()
                        .and_then(|k| k.as_str().map(|k| k == role))
                        .unwrap_or(false)
            })
        })
        .map(node_json)
        .collect()
}

// ---------------------------------------------------------------- types

/// The `.d.luau` as a prelude `luau-analyze` can read: `declare x: T`
/// becomes `local x: T = (nil :: any)`, and the mode line goes.
fn prelude() -> String {
    API.lines()
        .map(|line| {
            if line.starts_with("--!") {
                String::new()
            } else if let Some(rest) = line.strip_prefix("declare ") {
                format!("local {rest} = (nil :: any)")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// Checks `source` with `luau-analyze` (strict mode) against the API.
/// A first line `--!nonstrict` or `--!nocheck` in the script relaxes it.
pub(crate) async fn typecheck(source: &str) -> TypeCheck {
    let executable = std::env::var_os(ENV_ANALYZE).unwrap_or_else(|| "luau-analyze".into());
    typecheck_with(&executable, source).await
}

async fn typecheck_with(executable: &std::ffi::OsStr, source: &str) -> TypeCheck {
    let prelude = prelude();
    let offset = prelude.lines().count();
    let (mode, body) = match source.lines().next().map(str::trim) {
        Some(first @ ("--!nonstrict" | "--!nocheck" | "--!strict")) => (
            format!("{first}\n"),
            format!("\n{}", source.split_once('\n').map_or("", |(_, rest)| rest)),
        ),
        _ => (String::new(), source.to_owned()),
    };
    // The mode line, if any, sits above the prelude without moving lines:
    // it replaces the prelude's blank first line.
    let combined = if mode.is_empty() {
        format!("{prelude}{body}\n")
    } else {
        format!(
            "{mode}{}{body}\n",
            prelude.split_once('\n').map_or("", |(_, r)| r)
        )
    };
    let path = match write_new_temp(&combined) {
        Ok(path) => path,
        Err(error) => {
            return TypeCheck::Unavailable {
                reason: format!("cannot write a temporary file: {error}"),
            }
        }
    };
    let output = tokio::time::timeout(
        ANALYZE_TIMEOUT,
        tokio::process::Command::new(executable)
            .args(["--mode=strict", "--formatter=plain"])
            .arg(&path)
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let _ = std::fs::remove_file(&path);
    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return TypeCheck::Unavailable {
                reason: format!(
                    "`{}` not found: install Luau or set {ENV_ANALYZE}",
                    executable.to_string_lossy()
                ),
            }
        }
        Ok(Err(error)) => {
            return TypeCheck::Unavailable {
                reason: format!("cannot run `{}`: {error}", executable.to_string_lossy()),
            }
        }
        Err(_) => {
            return TypeCheck::Unavailable {
                reason: "luau-analyze took too long".into(),
            }
        }
    };
    let errors = parse_analysis(&String::from_utf8_lossy(&output.stdout), offset);
    if errors.is_empty() {
        TypeCheck::Passed
    } else {
        TypeCheck::Failed { errors }
    }
}

/// Writes `text` to a new file in the temp dir, never one that exists
/// (`create_new`), and returns its path.
fn write_new_temp(text: &str) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut last = None;
    for _ in 0..16 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "iced-mcp-script-{}-{nanos}-{n}.luau",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free temporary name")))
}

/// `luau-analyze --formatter=plain` output → the script's type and syntax
/// errors (lints dropped), lines relative to the script. A message's
/// continuation lines (`but got …`) are joined to it.
pub(crate) fn parse_analysis(text: &str, offset: usize) -> Vec<TypeError> {
    let mut errors: Vec<TypeError> = Vec::new();
    let mut continuing = false;
    for line in text.lines() {
        match parse_header(line) {
            Some((at, column, kind, message)) => {
                continuing = false;
                if !matches!(kind, "TypeError" | "SyntaxError") {
                    continue;
                }
                let Some(at) = at.checked_sub(offset).filter(|l| *l > 0) else {
                    continue;
                };
                errors.push(TypeError {
                    line: at,
                    column,
                    message: message.to_owned(),
                });
                continuing = true;
            }
            None if continuing => {
                if let Some(last) = errors.last_mut() {
                    last.message.push(' ');
                    last.message.push_str(line.trim());
                }
            }
            None => {}
        }
    }
    errors
}

/// `path:12:3-7: (W0) TypeError: message` → (12, 3, "TypeError", message).
fn parse_header(line: &str) -> Option<(usize, usize, &str, &str)> {
    let marker = line.find(": (")?;
    let (location, rest) = line.split_at(marker);
    let rest = rest.strip_prefix(": (")?;
    let (_, rest) = rest.split_once(") ")?;
    let (kind, message) = rest.split_once(": ")?;
    // location = path:LINE:COL-COL; the path may itself hold ':'.
    let mut parts = location.rsplitn(3, ':');
    let columns = parts.next()?;
    let line_no = parts.next()?.parse().ok()?;
    let column = columns.split('-').next()?.parse().ok()?;
    Some((line_no, column, kind, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A script on a thread worker: the test binary cannot be a worker
    /// process (`tests/scripts.rs` covers those through the real binary).
    fn script(source: &str) -> Script {
        Script {
            typecheck: false,
            worker: Worker::Thread,
            ..Script::new("script", source)
        }
    }

    fn sandbox_run(source: &str) -> Outcome {
        sandbox_run_with(script(source))
    }

    fn sandbox_run_with(script: Script) -> Outcome {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let root = std::env::temp_dir().join(format!("iced-mcp-script-{}", std::process::id()));
        let apps = Arc::new(Mutex::new(Apps::new(root)));
        runtime.block_on(run(apps, script, None))
    }

    #[test]
    fn a_script_returns_values_and_prints() {
        let out = sandbox_run("print('hello', 1, true)\nreturn { n = 3, list = {1, 2}, s = 'x' }");
        assert!(out.ok, "{out:?}");
        assert_eq!(out.output, "hello\t1\ttrue\n");
        assert_eq!(
            out.value,
            serde_json::json!({ "n": 3, "list": [1, 2], "s": "x" })
        );
        let out = sandbox_run("return 1, 'two'");
        assert_eq!(out.value, serde_json::json!([1, "two"]));
        assert!(sandbox_run("local x = 1").value.is_null());
    }

    #[test]
    fn sandbox_escapes_are_refused() {
        for global in [
            "io",
            "os",
            "debug",
            "require",
            "loadstring",
            "getfenv",
            "setfenv",
            "dofile",
            "load",
            "package",
        ] {
            let out = sandbox_run(&format!("return {global} == nil"));
            assert_eq!(out.value, serde_json::json!(true), "{global} is reachable");
        }
        // Library tables are read-only in sandbox mode.
        let out = sandbox_run("string.upper = nil");
        assert!(!out.ok);
        assert!(
            out.error.unwrap().message.contains("readonly"),
            "string tables must be read-only"
        );
        // So is the app global.
        let out = sandbox_run("app = nil\nreturn app ~= nil");
        assert!(out.ok, "{out:?}");
    }

    #[test]
    fn an_endless_loop_is_stopped_by_the_deadline() {
        let started = Instant::now();
        let out = sandbox_run_with(Script {
            timeout: Duration::from_millis(300),
            ..script("local n = 0\nwhile true do\n  n += 1\nend")
        });
        assert!(started.elapsed() < Duration::from_secs(5));
        let error = out.error.expect("must time out");
        assert!(
            error.message.contains("timed out after 300 ms"),
            "{}",
            error.message
        );
        assert!(matches!(error.line, Some(2..=3)), "{error:?}");
    }

    #[test]
    fn memory_is_capped() {
        let out = sandbox_run_with(Script {
            memory: 8 * 1024 * 1024,
            ..script("local t = {}\nfor i = 1, 1e8 do t[i] = string.rep('x', 64) .. i end")
        });
        let error = out.error.expect("must run out of memory");
        assert!(
            error.message.contains("out of memory (limit 8 MiB)"),
            "{}",
            error.message
        );
    }

    #[test]
    fn output_is_capped() {
        let out = sandbox_run_with(Script {
            output: 100,
            ..script("for i = 1, 1000 do print('line', i) end")
        });
        assert!(out.ok);
        assert!(out.output.len() < 150, "{}", out.output.len());
        assert!(out.output.ends_with("… output cut at 100 bytes"));
    }

    #[test]
    fn errors_carry_the_line() {
        let out = sandbox_run("local a = 1\n\nerror('boom')");
        let error = out.error.unwrap();
        assert_eq!(error.line, Some(3), "{error:?}");
        assert_eq!(error.message, "script:3: boom");
        let out = sandbox_run("local a = 1\nlocal b = a +");
        assert_eq!(out.error.unwrap().line, Some(2));
        let out = sandbox_run("local t = nil\nreturn t.x");
        assert_eq!(out.error.unwrap().line, Some(2));
    }

    #[test]
    fn app_failures_raise_the_step_message_at_the_line() {
        // No app is connected: the step's own error, prefixed with the line.
        let out = sandbox_run("print('start')\n\napp:click{ id = 'save' }\nprint('never')");
        let error = out.error.unwrap();
        assert_eq!(error.line, Some(3));
        assert_eq!(
            error.message,
            "script:3: no app connected: call `launch` or `connect` first (`setup_help` if the app has no bridge)"
        );
        assert_eq!(out.output, "start\n");
        assert_eq!(out.steps.len(), 1);
        assert_eq!(out.steps[0].label, "click #save");
        assert!(!out.steps[0].ok);
        assert_eq!(out.steps[0].line, Some(3));
        // A bundle was written even without an app.
        let bundle = out.bundle.expect("a failure bundle");
        assert!(bundle.join("failure.txt").exists());
        assert!(bundle.join("script.luau").exists());
        assert!(bundle.join("steps.log").exists());
        let _ = std::fs::remove_dir_all(bundle);
        // pcall sees the same message.
        let out = sandbox_run(
            "local ok, err = pcall(function() return apps['other']:wait_for('x') end)\nreturn ok, tostring(err)",
        );
        assert!(out.ok, "{out:?}");
        assert_eq!(out.value[0], false);
        let message = out.value[1].as_str().unwrap();
        assert!(
            message.starts_with("script:1: no app named `other`"),
            "{message}"
        );
    }

    #[test]
    fn bad_arguments_say_so() {
        let out = sandbox_run("app:scroll('down')");
        assert_eq!(
            out.error.unwrap().message,
            "script:1: bad args: expected a table, got string"
        );
        let out = sandbox_run("app:find{}");
        assert_eq!(
            out.error.unwrap().message,
            "script:1: bad args: find needs id, text or role"
        );
        let out = sandbox_run("app:screenshot_diff('../../outside')");
        assert!(
            out.error
                .unwrap()
                .message
                .contains("baseline name `../../outside` must be"),
            "baselines stay under .iced-mcp"
        );
        let out = sandbox_run("app:screenshot('../x')");
        assert_eq!(
            out.error.unwrap().message,
            "script:1: screenshot name `../x` must be 1–80 letters, digits, '-' or '_'"
        );
        let out = sandbox_run("app:clik('x')");
        assert!(out.error.unwrap().message.contains("clik"));
    }

    #[test]
    fn sleep_counts_against_the_deadline() {
        let out = sandbox_run("return app:sleep(20)");
        assert_eq!(out.value, serde_json::json!("slept 20 ms"));
        assert_eq!(out.steps[0].label, "sleep");
        let out = sandbox_run_with(Script {
            timeout: Duration::from_millis(100),
            ..script("app:sleep(5000)")
        });
        assert!(out.error.unwrap().message.contains("timed out"));
    }

    /// Every method the VM registers is declared in `iced-mcp.d.luau`, and
    /// nothing else is.
    #[test]
    fn the_d_luau_declares_exactly_the_methods() {
        let start = API.find("export type App = {").expect("App type");
        let body = &API[start..];
        let end = body.find("\n}").expect("end of App");
        let declared: std::collections::BTreeSet<&str> = body[..end]
            .lines()
            .skip(1)
            .filter_map(|line| {
                let line = line.trim();
                let (name, rest) = line.split_once(':')?;
                (rest.trim_start().starts_with('(')
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                .then_some(name)
            })
            .collect();
        let registered: std::collections::BTreeSet<&str> = METHODS.iter().copied().collect();
        assert_eq!(registered, declared, "METHODS and iced-mcp.d.luau differ");
        assert!(API.contains("declare app: App"));
        assert!(API.contains("declare apps: { [string]: App }"));
        // Every method dispatches (none falls through to "unknown method").
        for method in METHODS {
            let out = sandbox_run(&format!("app:{method}()"));
            let message = out.error.map(|e| e.message).unwrap_or_default();
            assert!(!message.contains("unknown method"), "{method}: {message}");
        }
    }

    #[test]
    fn analysis_output_is_parsed_to_script_lines() {
        let text = "/tmp/x.luau:170:3-9: (W0) TypeError: Key 'clik' not found in table 'App'\n\
                    /tmp/x.luau:172:11-43: (W0) TypeError: Expected this to be\n\
                    \t'number'\n\
                    but got\n\
                    \t'string'\n\
                    /tmp/x.luau:173:7-7: (W0) LocalUnused: Variable 'x' is never used\n\
                    /tmp/x.luau:5:1-2: (W0) TypeError: in the prelude\n\
                    C:\\tmp\\x.luau:174:1-2: (W0) SyntaxError: Expected identifier";
        let errors = parse_analysis(text, 168);
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert_eq!(errors[0].line, 2);
        assert_eq!(errors[0].column, 3);
        assert_eq!(errors[0].message, "Key 'clik' not found in table 'App'");
        assert_eq!(
            errors[1].message,
            "Expected this to be 'number' but got 'string'"
        );
        assert_eq!(errors[2].line, 6);
    }

    #[test]
    fn the_prelude_keeps_line_numbers_and_declares_locals() {
        let prelude = prelude();
        assert_eq!(prelude.lines().count(), API.lines().count());
        assert!(prelude.contains("local app: App = (nil :: any)"));
        assert!(!prelude.contains("declare "));
    }

    /// With `luau-analyze` installed, type errors are caught before anything
    /// runs; without it, the result says the check did not run.
    #[test]
    fn type_errors_stop_a_script_before_it_runs() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let check = runtime.block_on(typecheck(
            "app:click{ id = 'a' }\nlocal n: number = app:state()\nreturn n",
        ));
        match check {
            TypeCheck::Failed { errors } => {
                assert_eq!(errors.len(), 1, "{errors:?}");
                assert_eq!(errors[0].line, 2);
                assert!(errors[0].message.contains("number"), "{:?}", errors[0]);
                let out = sandbox_run_with(Script::new("script", "print('ran')\napp:clik('x')"));
                assert!(!out.ok);
                assert!(out.output.is_empty(), "nothing may run");
                assert!(out.steps.is_empty());
                assert_eq!(out.error.unwrap().line, Some(2));
            }
            TypeCheck::Unavailable { reason } => {
                assert!(reason.contains("not found"), "{reason}");
            }
            other => panic!("unexpected {other:?}"),
        }
        // A valid script passes (or the check is unavailable).
        let check = runtime.block_on(typecheck(
            "app:load_fixture('empty')\nlocal n = app:find{ id = 'x' }\nif n then app:expect(n, { visible = true }) end\nfor _, node in ipairs(app:find_all{ role = 'text' }) do print(node.text) end\nreturn #app:messages()",
        ));
        assert!(
            matches!(check, TypeCheck::Passed | TypeCheck::Unavailable { .. }),
            "{check:?}"
        );
        // A missing executable is reported, not fatal.
        let missing = runtime.block_on(typecheck_with(
            std::ffi::OsStr::new("/nonexistent/luau-analyze"),
            "return 1",
        ));
        assert!(
            matches!(&missing, TypeCheck::Unavailable { reason } if reason.contains("not found")),
            "{missing:?}"
        );
    }

    #[test]
    fn cli_arguments_parse_and_bad_ones_are_refused() {
        let args =
            |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_owned).collect() };
        let o = parse_cli(&args(
            "a.luau --launch app --root /r --timeout-ms 500 --update-baselines",
        ))
        .unwrap();
        assert_eq!(o.file, PathBuf::from("a.luau"));
        assert_eq!(o.launch.as_deref(), Some("app"));
        assert_eq!(o.root, Some(PathBuf::from("/r")));
        assert_eq!(o.timeout_ms, Some(500));
        assert!(o.update_baselines);
        assert!(parse_cli(&[]).is_err());
        assert!(parse_cli(&args("a.luau b.luau")).is_err());
        assert!(parse_cli(&args("a.luau --launch")).is_err());
        assert!(parse_cli(&args("a.luau --timeout-ms 0")).is_err());
        assert!(parse_cli(&args("a.luau --bogus")).is_err());
    }

    #[test]
    fn a_cancelled_script_stops_at_once() {
        let started = Instant::now();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let apps = Arc::new(Mutex::new(Apps::new(std::env::temp_dir())));
        let out = runtime.block_on(run(
            apps,
            Script {
                timeout: Duration::from_secs(3),
                ..script("while true do end")
            },
            Some(Box::pin(async {
                tokio::time::sleep(Duration::from_millis(100)).await;
            })),
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            out.error.unwrap().message,
            "script: the script was cancelled"
        );
    }

    #[test]
    fn the_return_value_and_prints_are_capped() {
        let out = sandbox_run("return string.rep('x', 2 * 1024 * 1024)");
        let message = out.error.unwrap().message;
        assert!(
            message.contains("over the 1024 KiB limit: return less"),
            "{message}"
        );
        let out = sandbox_run("print(string.rep('y', 5 * 1024 * 1024))\nreturn 1");
        assert!(out.ok, "{:?}", out.error);
        assert!(out.output.ends_with("… output cut at 65536 bytes"));
        assert!(out.output.len() < 70_000);
    }

    #[test]
    fn bundle_names_are_file_safe() {
        assert_eq!(bundle_name("script"), "script");
        assert_eq!(bundle_name("todos-add.luau"), "script-todos-add");
        assert_eq!(bundle_name("a b/c"), "script-a_b_c");
    }
}
