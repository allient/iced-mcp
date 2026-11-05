//! Luau scripts: one script instead of many tool calls (`script_eval`,
//! `iced-mcp run-script`, `.iced-mcp/scripts/*.luau` in `run-suite`).
//!
//! **Sandbox.** A fresh Luau VM per script, in Luau's sandbox mode (read-only
//! globals and libraries), with only `coroutine`, `table`, `string`, `utf8`,
//! `bit32`, `math` and `buffer`: no `io`, `os`, `debug`, `require`,
//! `loadstring`, `getfenv`/`setfenv`, files or network. The only way out is
//! the `app` / `apps` globals, which reach the connected apps through the
//! same step functions as `run_sequence` and flows ([`ops::run_step`]). A
//! wall-clock deadline is enforced by Luau's interrupt (loops and calls) and
//! around every app call; memory and output are capped.
//!
//! **Async bridging.** The step functions are async and need the shared
//! [`Apps`] behind a tokio mutex, while an `mlua::Lua` is neither `Send` nor
//! async here. Each script therefore runs on its own OS thread that owns the
//! VM; an app call blocks that thread on [`Handle::block_on`] of the server's
//! multi-threaded runtime, which locks `Apps` for that one call only. So a
//! long script never parks a runtime worker, and other MCP requests (other
//! tools, other scripts) interleave between its steps exactly as they do
//! between separate tool calls. The tool awaits the script's result on a
//! oneshot channel; dropping that wait (the request is cancelled) sets a
//! flag the interrupt and the next app call honour.

use crate::bundle;
use crate::flow::{self, FlowStep};
use crate::ops::{self, App, Ctx};
use crate::params::*;
use crate::tools::Apps;
use crate::view::{self, Selector};

use mlua::{Lua, LuaOptions, LuaSerdeExt, MultiValue, StdLib, Value, VmState};
use serde::Serialize;
use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::Mutex;

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

/// Type-checks, then runs `script` on its own thread against `apps`. On a
/// failure, writes a bundle from the app the script last used.
pub(crate) async fn run(apps: Arc<Mutex<Apps>>, script: Script) -> Outcome {
    match precheck(&script).await {
        Ok(typecheck) => run_checked(apps, script, typecheck).await,
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

/// Runs a script [`precheck`] passed.
pub(crate) async fn run_checked(
    apps: Arc<Mutex<Apps>>,
    script: Script,
    typecheck: TypeCheck,
) -> Outcome {
    let started = Instant::now();
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel_on_drop = CancelOnDrop(cancelled.clone());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let host = Host {
        apps: apps.clone(),
        handle: Handle::current(),
        deadline: started + script.timeout,
        timeout: script.timeout,
        cancelled,
        chunk: script.name.clone(),
        default_app: script.app.clone(),
        update_baselines: script.update_baselines,
        output_limit: script.output,
        output: RefCell::new(String::new()),
        output_cut: RefCell::new(false),
        steps: RefCell::new(Vec::new()),
        last_app: RefCell::new(None),
    };
    let thread_script = script.clone();
    let spawned = std::thread::Builder::new()
        .name("iced-mcp-luau".into())
        .spawn(move || {
            let result = execute(host, &thread_script);
            let _ = tx.send(result);
        });
    let finished = match spawned {
        Ok(_) => rx.await.map_err(|_| "the script thread stopped".to_owned()),
        Err(error) => Err(format!("cannot start the script thread: {error}")),
    };
    let (output, value, steps, last_app, error) = match finished {
        Ok(done) => done,
        Err(message) => (
            String::new(),
            serde_json::Value::Null,
            Vec::new(),
            None,
            Some(ScriptError {
                message,
                line: None,
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
        output,
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

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

type Finished = (
    String,
    serde_json::Value,
    Vec<StepLog>,
    Option<String>,
    Option<ScriptError>,
);

/// What app calls need, owned by the script thread.
struct Host {
    apps: Arc<Mutex<Apps>>,
    handle: Handle,
    deadline: Instant,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
    chunk: String,
    default_app: Option<String>,
    update_baselines: bool,
    output_limit: usize,
    output: RefCell<String>,
    output_cut: RefCell<bool>,
    steps: RefCell<Vec<StepLog>>,
    last_app: RefCell<Option<String>>,
}

impl Host {
    /// Why the script must stop now, if it must.
    fn stop_reason(&self) -> Option<String> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Some("the script was cancelled".into());
        }
        (Instant::now() >= self.deadline).then(|| {
            format!(
                "the script timed out after {} ms (raise timeout_ms)",
                self.timeout.as_millis()
            )
        })
    }

    /// Runs `f` on the app `name` means, holding the apps lock for this one
    /// call, within what is left of the deadline.
    fn with_app<T>(
        &self,
        name: Option<&str>,
        f: impl AsyncFnOnce(&mut App, &Ctx) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(reason) = self.stop_reason() {
            return Err(reason);
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        let name = name.map(str::to_owned).or_else(|| self.default_app.clone());
        let work = async {
            let mut apps = self.apps.lock().await;
            let ctx = Ctx {
                root: apps.root().to_path_buf(),
                update_baselines: self.update_baselines,
            };
            let app = apps.get(name.as_deref())?;
            *self.last_app.borrow_mut() = Some(app.name.clone());
            f(app, &ctx).await
        };
        self.handle
            .block_on(async { tokio::time::timeout(left, work).await })
            .unwrap_or_else(|_| {
                Err(format!(
                    "the script timed out after {} ms (raise timeout_ms)",
                    self.timeout.as_millis()
                ))
            })
    }

    fn print(&self, text: &str) {
        if *self.output_cut.borrow() {
            return;
        }
        let mut output = self.output.borrow_mut();
        if output.len() + text.len() + 1 > self.output_limit {
            let room = self.output_limit.saturating_sub(output.len());
            let mut cut = room.min(text.len());
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            output.push_str(&text[..cut]);
            let _ = write!(output, "\n… output cut at {} bytes", self.output_limit);
            *self.output_cut.borrow_mut() = true;
        } else {
            output.push_str(text);
            output.push('\n');
        }
    }
}

/// Builds `app` and `apps` in Lua around one Rust entry point. Each method
/// is a Lua wrapper that raises the entry point's failure with
/// `error(message, 0)`, so a script sees a plain string (`pcall` gets the
/// message, not a wrapped Rust error) whose line was already added.
const HANDLES: &str = r#"
local host, methods = ...
local function handle(name)
  local t = {}
  for _, method in ipairs(methods) do
    t[method] = function(_, a, b)
      local ok, value = host(name, method, a, b)
      if not ok then error(value, 0) end
      return value
    end
  end
  return setmetatable(t, { __tostring = function() return "app<" .. (name or "default") .. ">" end })
end
local apps = setmetatable({}, { __index = function(_, name) return handle(name) end })
return handle(nil), apps
"#;

/// Builds the sandboxed VM, runs the script and collects what it left.
fn execute(host: Host, script: &Script) -> Finished {
    let host = Rc::new(host);
    let result = run_vm(&host, script);
    let output = host.output.borrow().clone();
    let steps = host.steps.borrow().clone();
    let last_app = host.last_app.borrow().clone();
    match result {
        Ok(value) => (output, value, steps, last_app, None),
        Err(error) => (
            output,
            serde_json::Value::Null,
            steps,
            last_app,
            Some(error),
        ),
    }
}

fn run_vm(host: &Rc<Host>, script: &Script) -> Result<serde_json::Value, ScriptError> {
    let fail = |message: String| ScriptError {
        line: line_of(&message, &script.name),
        message,
    };
    let lua = new_sandbox(host, script).map_err(|e| fail(format!("cannot start Luau: {e}")))?;
    let values = lua
        .load(script.source.as_str())
        .set_name(format!("={}", script.name))
        .eval::<MultiValue>()
        .map_err(|e| fail(error_text(&e, script.memory)))?;
    Ok(return_value(&lua, values))
}

/// A Luau VM with only safe libraries, the `app`/`apps`/`print` globals,
/// the deadline interrupt and the memory limit, in sandbox mode.
fn new_sandbox(host: &Rc<Host>, script: &Script) -> mlua::Result<Lua> {
    let libs = StdLib::COROUTINE
        | StdLib::TABLE
        | StdLib::STRING
        | StdLib::UTF8
        | StdLib::BIT
        | StdLib::MATH
        | StdLib::BUFFER;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    let globals = lua.globals();
    // mlua installs a filesystem `require` for Luau: scripts get none.
    for name in [
        "require",
        "loadstring",
        "getfenv",
        "setfenv",
        "dofile",
        "load",
    ] {
        globals.raw_set(name, Value::Nil)?;
    }
    lua.set_app_data(host.clone());

    let printer = host.clone();
    globals.set(
        "print",
        lua.create_function(move |_, args: MultiValue| {
            let parts: Vec<String> = args
                .iter()
                .map(|v| v.to_string().unwrap_or_else(|_| format!("{v:?}")))
                .collect();
            printer.print(&parts.join("\t"));
            Ok(())
        })?,
    )?;
    let entry = lua.create_function(
        |lua, (name, method, first, second): (Option<String>, String, Value, Value)| {
            let Some(&method) = METHODS.iter().find(|m| **m == method) else {
                return Ok((
                    false,
                    Value::String(lua.create_string(format!("unknown method `{method}`"))?),
                ));
            };
            match call(lua, name.as_deref(), method, first, second) {
                Ok(value) => Ok((true, value)),
                Err(message) => Ok((false, Value::String(lua.create_string(message)?))),
            }
        },
    )?;
    let (app, apps): (Value, Value) = lua
        .load(HANDLES)
        .set_name("=iced-mcp")
        .call((entry, METHODS.to_vec()))?;
    // `app` names the app `script_eval {app}` asked for.
    let app = match &script.app {
        Some(name) => {
            let apps_table = apps.as_table().cloned();
            match apps_table {
                Some(table) => table.get::<Value>(name.as_str())?,
                None => app,
            }
        }
        None => app,
    };
    globals.set("app", app)?;
    globals.set("apps", apps)?;

    lua.sandbox(true)?;
    lua.set_memory_limit(script.memory)?;
    let stopper = host.clone();
    lua.set_interrupt(move |lua| match stopper.stop_reason() {
        Some(reason) => Err(mlua::Error::RuntimeError(located(
            lua,
            &stopper.chunk,
            &reason,
            0,
        ))),
        None => Ok(VmState::Continue),
    });
    Ok(lua)
}

/// `chunk:line: message`, with the line of the Lua code at `level`.
fn located(lua: &Lua, chunk: &str, message: &str, level: usize) -> String {
    match current_line(lua, level) {
        Some(line) => format!("{chunk}:{line}: {message}"),
        None => format!("{chunk}: {message}"),
    }
}

/// The line the Lua code at stack `level` is on (1 = the caller of a Rust
/// function).
fn current_line(lua: &Lua, level: usize) -> Option<usize> {
    lua.inspect_stack(level, |debug| debug.current_line())
        .flatten()
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

/// An mlua error as one readable message: the innermost cause, without
/// mlua's callback wrapping or tracebacks.
fn error_text(error: &mlua::Error, memory: usize) -> String {
    match error {
        mlua::Error::CallbackError { cause, .. } => error_text(cause, memory),
        mlua::Error::WithContext { cause, .. } => error_text(cause, memory),
        mlua::Error::RuntimeError(message) => message
            .split("\nstack traceback:")
            .next()
            .unwrap_or(message)
            .to_owned(),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::MemoryError(_) => format!(
            "the script ran out of memory (limit {} MiB)",
            memory / (1024 * 1024)
        ),
        other => other.to_string(),
    }
}

/// The script's return values as JSON: none → null, one → itself, several
/// → an array. What JSON cannot hold (functions, userdata) becomes text.
fn return_value(lua: &Lua, values: MultiValue) -> serde_json::Value {
    let mut list: Vec<serde_json::Value> = values.into_iter().map(|v| to_json(lua, v)).collect();
    match list.len() {
        0 => serde_json::Value::Null,
        1 => list.remove(0),
        _ => serde_json::Value::Array(list),
    }
}

fn to_json(lua: &Lua, value: Value) -> serde_json::Value {
    match lua.from_value::<serde_json::Value>(value.clone()) {
        Ok(json) => json,
        Err(_) => serde_json::Value::String(value.to_string().unwrap_or_default()),
    }
}

/// A Lua value as a step's JSON arguments.
fn args_json(lua: &Lua, value: Value) -> Result<serde_json::Value, String> {
    match value {
        Value::Nil => Ok(serde_json::json!({})),
        Value::Table(_) => lua
            .from_value::<serde_json::Value>(value)
            .map_err(|e| format!("bad args: {e}")),
        other => Err(format!(
            "bad args: expected a table, got {}",
            other.type_name()
        )),
    }
}

/// A table, or a string standing for `{ [key] = string }`.
fn args_or(lua: &Lua, value: Value, key: &str) -> Result<serde_json::Value, String> {
    match value {
        Value::String(s) => {
            Ok(serde_json::json!({ key: s.to_str().map_err(|e| e.to_string())?.to_string() }))
        }
        other => args_json(lua, other),
    }
}

/// Any Lua value as JSON (`nil` → null), for app action and query args.
fn any_json(lua: &Lua, value: Value) -> Result<serde_json::Value, String> {
    match value {
        Value::Nil => Ok(serde_json::Value::Null),
        other => lua
            .from_value::<serde_json::Value>(other)
            .map_err(|e| format!("bad args: {e}")),
    }
}

fn to_lua(lua: &Lua, value: &serde_json::Value) -> Result<Value, String> {
    let options = mlua::serde::SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false);
    lua.to_value_with(value, options).map_err(|e| e.to_string())
}

fn string_arg(value: &Value, what: &str) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.to_str().map_err(|e| e.to_string())?.to_string()),
        other => Err(format!(
            "bad args: {what} must be a string, got {}",
            other.type_name()
        )),
    }
}

/// A node as a Lua-ready JSON object, with `ref` beside `index`.
fn node_json(node: &iced_mcp_protocol::Node) -> serde_json::Value {
    let mut value = serde_json::to_value(node).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.insert("ref".into(), serde_json::json!(node.index));
    }
    value
}

/// One `app:method(a, b)` call: its result, or the message to raise,
/// `chunk:line: …` with the line of the script that called it.
fn call(
    lua: &Lua,
    app: Option<&str>,
    method: &'static str,
    first: Value,
    second: Value,
) -> Result<Value, String> {
    let Some(host) = lua.app_data_ref::<Rc<Host>>().map(|h| Rc::clone(&h)) else {
        return Err("the script host is gone".into());
    };
    // 0 = this function, 1 = the Lua wrapper, 2 = the script.
    let line = current_line(lua, 2);
    let started = Instant::now();

    // The step this call is, for the log: (action, args).
    let mut logged: Option<(String, serde_json::Value)> = None;
    let result = dispatch(lua, &host, app, method, first, second, &mut logged);

    if let Some((action, args)) = logged {
        let label = flow::label(&FlowStep { action, args });
        host.steps.borrow_mut().push(StepLog {
            line,
            app: host.last_app.borrow().clone(),
            action: method.to_owned(),
            label,
            ok: result.is_ok(),
            ms: elapsed_ms(started),
        });
    }
    result.map_err(|message| match line {
        Some(line) => format!("{}:{line}: {message}", host.chunk),
        None => format!("{}: {message}", host.chunk),
    })
}

#[allow(clippy::too_many_lines)]
fn dispatch(
    lua: &Lua,
    host: &Host,
    app: Option<&str>,
    method: &'static str,
    first: Value,
    second: Value,
    logged: &mut Option<(String, serde_json::Value)>,
) -> Result<Value, String> {
    // A step from the shared vocabulary, run through `ops::run_step`.
    let mut step = |action: &str, args: serde_json::Value| -> Result<Value, String> {
        *logged = Some((action.to_owned(), args.clone()));
        let step = Step {
            action: action.to_owned(),
            args,
        };
        let text = host.with_app(app, async |app, ctx| ops::run_step(app, ctx, &step).await)?;
        to_lua(lua, &serde_json::Value::String(text))
    };
    match method {
        "click" | "hover" => step(method, args_or(lua, first, "text")?),
        "type_text" | "paste" => step(method, args_or(lua, first, "text")?),
        "press_key" => step(method, args_or(lua, first, "keys")?),
        "scroll"
        | "drag"
        | "wait_idle"
        | "expect_pixel"
        | "expect_no_diagnostics"
        | "expect_query" => step(method, args_json(lua, first)?),
        "wait_for" => step(method, args_or(lua, first, "text")?),
        "expect_message" => step(method, args_or(lua, first, "contains")?),
        "screenshot_diff" => {
            // A baseline is a file name under .iced-mcp/baselines: scripts
            // may not write elsewhere.
            let args = args_or(lua, first, "baseline")?;
            if let Some(name) = args.get("baseline").and_then(serde_json::Value::as_str) {
                flow::check_name(name).map_err(|e| e.replacen("flow name", "baseline", 1))?;
            }
            step(method, args)
        }
        "run_app_action" | "load_fixture" | "fixture" => {
            let name = string_arg(&first, "the name")?;
            let args = any_json(lua, second)?;
            let action = if method == "fixture" {
                "load_fixture"
            } else {
                method
            };
            let mut json = serde_json::json!({ "name": name });
            if !args.is_null() {
                json["args"] = args;
            }
            step(action, json)
        }
        "expect" => {
            let mut json = args_json(lua, first)?;
            if !second.is_nil() {
                // `expect(node, checks)`: aim at the node by id, else by ref.
                let node = json;
                let mut checks = args_json(lua, second)?;
                let target = match (node.get("id"), node.get("ref")) {
                    (Some(id), _) if id.is_string() => serde_json::json!({ "id": id }),
                    (_, Some(reference)) => serde_json::json!({ "ref": reference }),
                    _ => {
                        return Err(
                            "bad args: expect(node, checks) needs a node from find or snapshot"
                                .into(),
                        )
                    }
                };
                if let (Some(checks), Some(target)) = (checks.as_object_mut(), target.as_object()) {
                    for (k, v) in target {
                        checks.insert(k.clone(), v.clone());
                    }
                    if let Some(window) = node.get("window") {
                        checks.entry("window").or_insert(window.clone());
                    }
                }
                json = checks;
            }
            step("expect", json)
        }
        "sleep" => {
            let ms = match first {
                Value::Integer(n) => u64::try_from(n).unwrap_or(0),
                Value::Number(n) if n >= 0.0 => n as u64,
                Value::Nil => 250,
                other => {
                    return Err(format!(
                        "bad args: sleep takes milliseconds, got {}",
                        other.type_name()
                    ))
                }
            }
            .min(MAX_SLEEP_MS);
            *logged = Some(("sleep".into(), serde_json::json!({ "ms": ms })));
            if let Some(reason) = host.stop_reason() {
                return Err(reason);
            }
            let left = host.deadline.saturating_duration_since(Instant::now());
            let wanted = Duration::from_millis(ms);
            host.handle
                .block_on(async { tokio::time::sleep(wanted.min(left)).await });
            if wanted > left {
                return Err(host.stop_reason().unwrap_or_else(|| "timed out".into()));
            }
            to_lua(lua, &serde_json::json!(format!("slept {ms} ms")))
        }
        "query" => {
            let name = string_arg(&first, "the query name")?;
            let args = any_json(lua, second)?;
            *logged = Some(("query".into(), serde_json::json!({ "name": name })));
            let value = host.with_app(app, async |app, _| ops::query(app, &name, &args).await)?;
            to_lua(lua, &value)
        }
        "snapshot" => {
            let window = match first {
                Value::Nil => None,
                other => Some(string_arg(&other, "the window")?),
            };
            *logged = Some(("snapshot".into(), serde_json::Value::Null));
            let snapshot = host.with_app(app, async |app, _| {
                ops::take_snapshot(app, window.as_deref()).await
            })?;
            let nodes: Vec<serde_json::Value> = snapshot.nodes.iter().map(node_json).collect();
            to_lua(
                lua,
                &serde_json::json!({
                    "window": snapshot.window,
                    "width": snapshot.size.width,
                    "height": snapshot.size.height,
                    "nodes": nodes,
                }),
            )
        }
        "find" | "find_all" => {
            let selector = args_or(lua, first, "text")?;
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
            let found = host.with_app(app, async |app, _| {
                let snapshot = ops::take_snapshot(app, window).await?;
                Ok(find_nodes(&snapshot, id, text, exact, role))
            })?;
            if method == "find" {
                match found.into_iter().next() {
                    Some(node) => to_lua(lua, &node),
                    None => Ok(Value::Nil),
                }
            } else {
                to_lua(lua, &serde_json::Value::Array(found))
            }
        }
        "diagnose" => {
            let json = args_json(lua, first)?;
            *logged = Some(("diagnose".into(), json.clone()));
            let p: DiagnoseParams =
                serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
            let (_, value) = host.with_app(app, async |app, _| ops::diagnose(app, &p).await)?;
            to_lua(lua, &value["findings"])
        }
        "screenshot" => {
            let mut json = args_or(lua, first, "name")?;
            *logged = Some(("screenshot".into(), json.clone()));
            let name = json
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or(
                    "bad args: screenshot needs a name (the file .iced-mcp/screenshots/<name>.png)",
                )?
                .to_owned();
            flow::check_name(&name)?;
            let root = host
                .handle
                .block_on(async { host.apps.lock().await.root().to_path_buf() });
            let path = screenshots_dir(&root).join(format!("{name}.png"));
            if let Some(map) = json.as_object_mut() {
                map.remove("name");
                map.insert("save_to".into(), serde_json::json!(path.to_string_lossy()));
            }
            let p: ScreenshotToolParams =
                serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
            let (_, text) = host.with_app(app, async |app, _| ops::screenshot(app, &p).await)?;
            to_lua(lua, &serde_json::json!(text))
        }
        "messages" => {
            let json = args_json(lua, first)?;
            *logged = Some(("messages".into(), serde_json::Value::Null));
            let p: MessagesToolParams =
                serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
            let log = host.with_app(app, async |app, _| {
                ops::read_messages(app, p.since, p.filter.clone(), Some(p.limit.unwrap_or(30)))
                    .await
            })?;
            to_lua(
                lua,
                &serde_json::to_value(&log.entries).map_err(|e| e.to_string())?,
            )
        }
        "state" => {
            let json = args_json(lua, first)?;
            *logged = Some(("state".into(), serde_json::Value::Null));
            let p: StateToolParams =
                serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
            let text = host.with_app(app, async |app, _| ops::state(app, &p).await)?;
            to_lua(lua, &serde_json::json!(text))
        }
        "logs" => {
            let json = args_json(lua, first)?;
            *logged = Some(("logs".into(), serde_json::Value::Null));
            let p: LogsParams =
                serde_json::from_value(json).map_err(|e| format!("bad args: {e}"))?;
            let lines = host.with_app(app, async |app, _| match &app.launched {
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
            })?;
            to_lua(lua, &serde_json::json!(lines))
        }
        other => Err(format!("unknown method `{other}`")),
    }
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
    let unique = format!(
        "iced-mcp-script-{}-{}.luau",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let path = std::env::temp_dir().join(unique);
    if let Err(error) = std::fs::write(&path, combined) {
        return TypeCheck::Unavailable {
            reason: format!("cannot write a temporary file: {error}"),
        };
    }
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

    fn sandbox_run(source: &str) -> Outcome {
        sandbox_run_with(Script {
            typecheck: false,
            ..Script::new("script", source)
        })
    }

    fn sandbox_run_with(script: Script) -> Outcome {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let root = std::env::temp_dir().join(format!("iced-mcp-script-{}", std::process::id()));
        let apps = Arc::new(Mutex::new(Apps::new(root)));
        runtime.block_on(run(apps, script))
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
            typecheck: false,
            ..Script::new("script", "local n = 0\nwhile true do\n  n += 1\nend")
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
            typecheck: false,
            ..Script::new(
                "script",
                "local t = {}\nfor i = 1, 1e8 do t[i] = string.rep('x', 64) .. i end",
            )
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
            typecheck: false,
            ..Script::new("script", "for i = 1, 1000 do print('line', i) end")
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
                .contains("baseline `../../outside` must be"),
            "baselines stay under .iced-mcp"
        );
        let out = sandbox_run("app:screenshot('../x')");
        assert!(out.error.unwrap().message.contains("must be 1–80 letters"));
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
            typecheck: false,
            ..Script::new("script", "app:sleep(5000)")
        });
        assert!(out.error.unwrap().message.contains("timed out"));
    }

    #[test]
    fn json_converts_both_ways() {
        let lua = Lua::new();
        let json = serde_json::json!({ "a": [1, 2.5, "x", true], "b": { "c": null }, "e": [] });
        let value = to_lua(&lua, &json).unwrap();
        let back = to_json(&lua, value);
        assert_eq!(back["a"], serde_json::json!([1, 2.5, "x", true]));
        assert!(back["b"].get("c").is_none(), "null becomes nil");
        assert_eq!(back["e"], serde_json::json!([]));
        // Whole numbers stay integers, which step parameters need.
        let args = args_json(
            &lua,
            lua.load("return { ms = 250, x = 10.5 }").eval().unwrap(),
        )
        .unwrap();
        assert_eq!(args, serde_json::json!({ "ms": 250, "x": 10.5 }));
        let p: WaitParams = serde_json::from_value(
            args_json(
                &lua,
                lua.load("return { text = 'a', timeout_ms = 100 }")
                    .eval()
                    .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(p.timeout_ms, Some(100));
        assert_eq!(
            args_or(
                &lua,
                Value::String(lua.create_string("Add").unwrap()),
                "text"
            )
            .unwrap(),
            serde_json::json!({ "text": "Add" })
        );
        assert_eq!(
            to_json(
                &lua,
                Value::Function(lua.create_function(|_, ()| Ok(())).unwrap())
            )
            .as_str()
            .map(|s| s.starts_with("function")),
            Some(true)
        );
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

    /// `run-script` exits 0 when the script finishes, 1 when it fails and
    /// 2 when the file cannot be read.
    #[test]
    fn the_cli_exit_status_follows_the_outcome() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join(format!("iced-mcp-run-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let server = crate::tools::Server::new();
        let cli = |file: PathBuf| CliOptions {
            file,
            root: Some(dir.clone()),
            ..CliOptions::default()
        };
        let (code, report) = runtime
            .block_on(server.run_script_cli(&cli(write("ok.luau", "print('hi')\nreturn 42"))));
        assert_eq!(code, 0, "{report}");
        assert!(report.starts_with("PASS ok.luau"), "{report}");
        assert!(report.contains("Return: 42"), "{report}");
        let (code, report) = runtime
            .block_on(server.run_script_cli(&cli(write("bad.luau", "local x = 1\nerror('nope')"))));
        assert_eq!(code, 1, "{report}");
        assert!(report.contains("Error: bad.luau:2: nope"), "{report}");
        assert!(report.contains("Failure bundle: "), "{report}");
        let (code, report) =
            runtime.block_on(server.run_script_cli(&cli(dir.join("missing.luau"))));
        assert_eq!(code, 2, "{report}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_names_are_file_safe() {
        assert_eq!(bundle_name("script"), "script");
        assert_eq!(bundle_name("todos-add.luau"), "script-todos-add");
        assert_eq!(bundle_name("a b/c"), "script-a_b_c");
    }
}
