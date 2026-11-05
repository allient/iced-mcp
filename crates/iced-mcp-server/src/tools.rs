//! The MCP tools.
//!
//! Designed the way an agent works a UI: look (`snapshot`, `screenshot`),
//! act on what it saw by text, id or `[ref]` (`click`, `type_text`…), and see
//! the effect in the reply — every action waits for the screen to settle and
//! lists the messages the app published in response, which says more than a
//! screenshot and costs a few lines.
//!
//! Several apps can be connected at once (as in mcp-server-tauri): each has a
//! name, tools take `app`, and the most recently connected app is the
//! default. Flows (record/replay) and screen recording follow Argent.

use crate::bundle;
use crate::conn::{self, Conn, LaunchSpec};
use crate::flow::{self, Flow, FlowLaunch, FlowStep, StepReport, StepStatus};
use crate::ops::{self, App, Ctx};
use crate::params::*;
use crate::record::{self, Recording};

use iced_mcp_protocol::{
    Request, WindowAction, WindowParams, DEFAULT_PORT, ENV_FIXTURE, ENV_FIXTURE_ARGS,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};

use base64::Engine as _;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const INSTRUCTIONS: &str = "\
Drive running iced (Rust) desktop apps. If an app has no bridge yet, call `setup_help`.

1. `launch` the app (e.g. \"cargo run -p my-app\"; builds can take minutes) or `connect` to one running with ICED_MCP=1. Several apps can be connected; tools take `app`, default = the last connected.
2. `snapshot` lists what is on screen: `[ref] kind \"text\" #id (x,y w×h) cursor`, logical pixels from the window's top-left. `label` = an icon button's name, `overlay` = menus/popups, `region` = clickable area with no text.
3. Act by `text`, `id` or `ref` (prefer id > text > ref > x/y). Canvases are invisible to snapshot: `screenshot` (or `inspect_point`) and use x/y.
4. Every action waits for the screen to settle and lists the app messages it caused — verify with that, `wait_for`, `expect_message` (in sequences/flows), `snapshot`, `state` or `screenshot`.
5. Repeatable checks: `flow_start` → act → `flow_finish`, then `flow_run`. Visual checks: `screenshot_diff`. Speed: `perf`. Video: `record_start`/`record_stop`.";

const SETUP_HELP: &str = "\
# Add the iced-mcp bridge to an iced app

Inspect the project first and ask before editing its files.

1. Cargo.toml of the binary:
   ```toml
   [dependencies]
   iced-mcp = { path = \"<repo>/crates/iced-mcp\", optional = true }   # or the published crate
   [features]
   agent = [\"dep:iced-mcp\"]          # keep it out of release builds
   ```
2. main.rs — build the application without `.run()`, then:
   ```rust
   let app = iced::application(App::new, App::update, App::view) /* .title(..) .theme(..) … */;
   #[cfg(feature = \"agent\")]
   return iced_mcp::run(app);          // bridge only when ICED_MCP=1; else plain app.run()
   #[cfg(not(feature = \"agent\"))]
   app.run()
   ```
   The app's `Message` must derive `Debug`.
3. Optional, for richer tools:
   - `iced_mcp::attach(app).state(|s| format!(\"{s:#?}\")).run()` — enables `state` (leave secrets out).
   - `.action(\"open_settings\", \"Opens settings. No args.\", |_| Ok(Message::OpenSettings))` — enables `run_app_action`.
   - `.fixture(\"empty\", \"No data. No args.\", |_| Ok(State::default()))` — named starting states for tests (`fixtures`, `load_fixture`, `launch {fixture}`, a flow's `fixture`).
   - Give key widgets ids (`.id(\"search\")`); name icon-only buttons with a tooltip or an accessible label (operate → `operation.custom(None, bounds, &mut label_string)`).
4. Then `launch` with `cargo run -p <crate> --features agent`.";

/// Every connected app, the shared extras, and where files go.
pub(crate) struct Apps {
    list: Vec<App>,
    /// The app tools use when they name none.
    default: Option<String>,
    recording: Option<Recording>,
    draft: Option<Draft>,
    root: PathBuf,
}

/// A flow being recorded: actions on `app` are appended as they succeed.
struct Draft {
    name: String,
    app: String,
    flow: Flow,
}

impl Apps {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            list: Vec::new(),
            default: None,
            recording: None,
            draft: None,
            root,
        }
    }

    pub(crate) fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub(crate) fn set_root(&mut self, root: PathBuf) {
        self.root = root;
    }

    /// Whether `name` is connected and its process (if launched here) runs.
    pub(crate) fn is_connected(&mut self, name: &str) -> bool {
        self.list.iter_mut().any(|app| {
            app.name == name
                && app.conn.is_some()
                && !app
                    .launched
                    .as_mut()
                    .is_some_and(|l| matches!(l.child.try_wait(), Ok(Some(_))))
        })
    }

    /// Disconnects `name` and stops it if it was launched here.
    pub(crate) async fn drop_app(&mut self, name: &str) {
        self.remove(name, true).await;
    }

    /// Launches `launch` as an app called `name`.
    pub(crate) async fn launch_named(
        &mut self,
        name: &str,
        launch: FlowLaunch,
        timeout: Duration,
    ) -> Result<(), String> {
        let spec = spec_for(&launch, None, None)?;
        let (app, _) = start(Some(name.to_owned()), spec, launch, timeout).await?;
        self.add(app).await;
        Ok(())
    }

    fn names(&self) -> String {
        self.list
            .iter()
            .map(|a| {
                if self.default.as_deref() == Some(a.name.as_str()) {
                    format!("{} [default]", a.name)
                } else {
                    a.name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The app a tool means: the one named, else the only one, else the default.
    fn get(&mut self, name: Option<&str>) -> Result<&mut App, String> {
        let index = match name {
            Some(name) => self.list.iter().position(|a| a.name == name).ok_or_else(|| {
                format!("no app named `{name}`; connected: {}", self.names())
            })?,
            None if self.list.is_empty() => {
                return Err(
                    "no app connected: call `launch` or `connect` first (`setup_help` if the app has no bridge)"
                        .into(),
                )
            }
            None if self.list.len() == 1 => 0,
            None => {
                let default = self.default.clone().unwrap_or_default();
                self.list
                    .iter()
                    .position(|a| a.name == default)
                    .ok_or_else(|| format!("several apps connected, give `app`: {}", self.names()))?
            }
        };
        Ok(&mut self.list[index])
    }

    /// Adds `app` (replacing one of the same name) and makes it the default.
    async fn add(&mut self, app: App) {
        if let Some(index) = self.list.iter().position(|a| a.name == app.name) {
            let mut old = self.list.remove(index);
            if let Some(launched) = old.launched.as_mut() {
                let _ = launched.child.kill().await;
            }
        }
        self.default = Some(app.name.clone());
        self.list.push(app);
    }

    async fn remove(&mut self, name: &str, kill: bool) -> Option<App> {
        let index = self.list.iter().position(|a| a.name == name)?;
        let mut app = self.list.remove(index);
        if kill {
            if let Some(launched) = app.launched.as_mut() {
                let _ = launched.child.kill().await;
            }
        }
        if self.default.as_deref() == Some(name) {
            self.default = self.list.last().map(|a| a.name.clone());
        }
        Some(app)
    }
}

#[derive(Clone)]
pub(crate) struct Server {
    apps: Arc<Mutex<Apps>>,
    tool_router: ToolRouter<Self>,
}

type ToolResult = Result<CallToolResult, McpError>;

impl Server {
    /// Wraps an outcome as a tool result, with the recording reminder.
    fn reply(apps: &Apps, outcome: Result<String, String>) -> ToolResult {
        let note = apps.recording.as_ref().map(Recording::reminder);
        let with_note = |text: String| match &note {
            Some(note) => format!("{note}\n{text}"),
            None => text,
        };
        Ok(match outcome {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(with_note(text))]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(with_note(error))]),
        })
    }

    /// Runs an action on its app and, while a flow is being recorded on
    /// that app, appends it as a step once it succeeds.
    async fn action<P: serde::Serialize>(
        &self,
        action: &'static str,
        app_name: Option<&str>,
        params: &P,
    ) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let ctx = Ctx {
            root: apps.root.clone(),
            update_baselines: false,
        };
        let step = FlowStep {
            action: action.to_owned(),
            args: recorded_args(params),
        };
        let outcome = match apps.get(app_name) {
            Ok(app) => {
                let name = app.name.clone();
                let outcome = ops::run_step(
                    app,
                    &ctx,
                    &crate::params::Step {
                        action: step.action.clone(),
                        args: step.args.clone(),
                    },
                )
                .await;
                if outcome.is_ok() {
                    if let Some(draft) = apps.draft.as_mut().filter(|d| d.app == name) {
                        draft.flow.steps.push(step);
                    }
                }
                outcome
            }
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }
}

impl Server {
    /// Runs a flow outside MCP, for CI (`iced-mcp run-flow`): the report's
    /// text, and whether every step passed.
    pub(crate) async fn run_flow_cli(
        &self,
        params: FlowRunParams,
        root: Option<PathBuf>,
    ) -> (bool, String) {
        let mut apps = self.apps.lock().await;
        if let Some(root) = root {
            apps.root = root;
        }
        if params.name.is_empty() {
            let names = flow::list(&apps.root);
            if names.is_empty() {
                return (
                    true,
                    format!("No flows in {}.", flow::dir(&apps.root).display()),
                );
            }
            return (true, names.join("\n"));
        }
        let run = run_flow(&mut apps, &params).await;
        (run.passed, run.text)
    }

    /// `iced-mcp run-suite`: the exit status and the report.
    pub(crate) async fn run_suite_cli(&self, options: &crate::suite::Options) -> (i32, String) {
        let mut apps = self.apps.lock().await;
        crate::suite::run(&mut apps, options).await
    }
}

/// A tool's parameters as a flow step's `args`: no nulls, no `app` (a flow
/// runs on whichever app it is given).
fn recorded_args<P: serde::Serialize>(params: &P) -> serde_json::Value {
    fn strip(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.into_iter()
                    .filter(|(k, v)| !v.is_null() && k != "app")
                    .map(|(k, v)| (k, strip(v)))
                    .collect(),
            ),
            other => other,
        }
    }
    strip(serde_json::to_value(params).unwrap_or_default())
}

/// Starts an app from `spec` and waits for its bridge.
async fn start(
    name: Option<String>,
    spec: LaunchSpec,
    launch: FlowLaunch,
    timeout: Duration,
) -> Result<(App, String), String> {
    let mut launched = conn::spawn(&spec)?;
    let started = Instant::now();
    match conn::wait_for_bridge(&mut launched, spec.port, spec.token.clone(), timeout).await {
        Ok(conn) => {
            let name = name.unwrap_or_else(|| conn.info.name.clone());
            let mut app = App::new(name);
            app.conn = Some(conn);
            app.launched = Some(launched);
            app.token = spec.token.clone();
            app.launch = Some(launch);
            if let Some(name) = spec.env.get(ENV_FIXTURE).cloned() {
                let args = spec
                    .env
                    .get(ENV_FIXTURE_ARGS)
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();
                app.spec = Some(spec);
                if let Err(error) = ops::await_boot_fixture(&mut app, &name).await {
                    if let Some(launched) = app.launched.as_mut() {
                        let _ = launched.child.kill().await;
                    }
                    return Err(format!("fixture: {error}"));
                }
                app.fixture = Some((name, args));
            } else {
                app.spec = Some(spec);
            }
            let summary = match (&app.conn, &app.launched) {
                (Some(conn), launched) => ops::status_text(&app.name, conn, launched.as_ref()),
                _ => String::new(),
            };
            Ok((
                app,
                format!(
                    "Launched in {:.1}s.\n{summary}",
                    started.elapsed().as_secs_f32()
                ),
            ))
        }
        Err(error) => {
            let _ = launched.child.kill().await;
            Err(error)
        }
    }
}

/// Asks a launch to start the app in `fixture`.
fn with_fixture(
    mut spec: LaunchSpec,
    fixture: Option<&str>,
    args: Option<&serde_json::Value>,
) -> LaunchSpec {
    if let Some(name) = fixture {
        spec.env.insert(ENV_FIXTURE.to_owned(), name.to_owned());
        match args.filter(|a| !a.is_null()) {
            Some(args) => {
                spec.env
                    .insert(ENV_FIXTURE_ARGS.to_owned(), args.to_string());
            }
            None => {
                spec.env.remove(ENV_FIXTURE_ARGS);
            }
        }
    }
    spec
}

fn spec_for(
    launch: &FlowLaunch,
    args: Option<Vec<String>>,
    token: Option<String>,
) -> Result<LaunchSpec, String> {
    let (program, args) = match args {
        Some(args) => (launch.command.clone(), args),
        None => {
            let mut words = ops::split_command(&launch.command);
            if words.is_empty() {
                return Err("empty command".into());
            }
            let program = words.remove(0);
            (program, words)
        }
    };
    Ok(LaunchSpec {
        program,
        args,
        cwd: launch.cwd.as_ref().map(PathBuf::from),
        env: launch.env.clone(),
        port: conn::free_port()?,
        token,
    })
}

#[tool_router]
impl Server {
    pub(crate) fn new() -> Self {
        let root = std::env::var_os("ICED_MCP_ROOT")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        Self {
            apps: Arc::new(Mutex::new(Apps::new(root))),
            tool_router: Self::tool_router(),
        }
    }

    // ---------------------------------------------------------- session

    #[tool(
        description = "Start an iced app with the bridge on and wait for its first window. Use when the app is not running yet. Returns its name and windows. Fails with the app's last output if it exits or never opens the bridge.",
        annotations(open_world_hint = false)
    )]
    async fn launch(&self, Parameters(p): Parameters<LaunchParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let launch = FlowLaunch {
            command: p.command.clone(),
            cwd: p.cwd.clone(),
            env: p.env.clone().unwrap_or_default(),
        };
        let outcome = async {
            let spec = with_fixture(
                spec_for(&launch, p.args.clone(), None)?,
                p.fixture.as_deref(),
                p.fixture_args.as_ref(),
            );
            let timeout = Duration::from_secs(p.timeout_secs.unwrap_or(300));
            start(p.name.clone(), spec, launch, timeout).await
        }
        .await;
        let outcome = match outcome {
            Ok((app, text)) => {
                apps.add(app).await;
                Ok(text)
            }
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Attach to an app already running with the bridge on (ICED_MCP=1). Use for apps you started yourself.",
        annotations(open_world_hint = false)
    )]
    async fn connect(&self, Parameters(p): Parameters<ConnectParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let port = p.port.unwrap_or(DEFAULT_PORT);
        let outcome = match Conn::open(port, p.token.clone()).await {
            Ok(conn) => {
                let name = p.name.clone().unwrap_or_else(|| conn.info.name.clone());
                let text = ops::status_text(&name, &conn, None);
                let mut app = App::new(name);
                app.conn = Some(conn);
                app.token = p.token.clone();
                apps.add(app).await;
                Ok(text)
            }
            Err(error) => Err(format!(
                "{error}. Start the app with ICED_MCP=1 (and ICED_MCP_PORT if not {DEFAULT_PORT}), or use `launch`; `setup_help` explains adding the bridge."
            )),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Drop an app's connection, and stop it if it was launched here (kill=false keeps it running).",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn disconnect(&self, Parameters(p): Parameters<DisconnectParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let name = match apps.get(p.app.as_deref()) {
            Ok(app) => app.name.clone(),
            Err(error) => return Self::reply(&apps, Err(error)),
        };
        let kill = p.kill.unwrap_or(true);
        let app = apps.remove(&name, kill).await;
        let mut text = format!("Disconnected {name}.");
        if kill && app.is_some_and(|a| a.launched.is_some()) {
            text.push_str(" Stopped the app.");
        }
        Self::reply(&apps, Ok(text))
    }

    #[tool(
        description = "Stop and relaunch an app started with `launch`, same command and environment — a clean state in one call.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn restart(&self, Parameters(p): Parameters<AppOnly>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let (name, spec, launch) = match apps.get(p.app.as_deref()) {
            Ok(App {
                name,
                spec: Some(spec),
                launch: Some(launch),
                ..
            }) => (name.clone(), spec.clone(), launch.clone()),
            Ok(app) => {
                let error = format!(
                    "{} was connected, not launched here: restart it yourself, then `connect`",
                    app.name
                );
                return Self::reply(&apps, Err(error));
            }
            Err(error) => return Self::reply(&apps, Err(error)),
        };
        apps.remove(&name, true).await;
        let outcome = async {
            let spec = LaunchSpec {
                port: conn::free_port()?,
                ..spec
            };
            start(Some(name), spec, launch, Duration::from_secs(300)).await
        }
        .await;
        let outcome = match outcome {
            Ok((app, text)) => {
                apps.add(app).await;
                Ok(format!("Restarted. {text}"))
            }
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Connected apps (default marked), their windows, and launched processes. Use to pick `app`/`window` values.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn status(&self) -> ToolResult {
        let mut apps = self.apps.lock().await;
        if apps.list.is_empty() {
            return Self::reply(
                &apps,
                Ok("No app connected. `launch` or `connect` one.".into()),
            );
        }
        let default = apps.default.clone();
        let mut text = String::new();
        for app in &mut apps.list {
            let alive = match app.launched.as_mut().map(|l| l.child.try_wait()) {
                Some(Ok(Some(status))) => format!(" — launched process exited: {status}"),
                _ => String::new(),
            };
            let token = app.token.clone();
            let line = match app.conn.as_mut() {
                Some(conn) => match conn.hello(token).await {
                    Ok(_) => ops::status_text(&app.name, conn, app.launched.as_ref()),
                    Err(error) => format!("[{}] connection lost: {error}", app.name),
                },
                None => format!("[{}] not connected", app.name),
            };
            let mark = if default.as_deref() == Some(app.name.as_str()) {
                " (default)"
            } else {
                ""
            };
            let _ = writeln!(text, "{line}{mark}{alive}");
        }
        Self::reply(&apps, Ok(text))
    }

    #[tool(
        description = "How to add the iced-mcp bridge to an iced app. Use when `launch`/`connect` finds no bridge.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn setup_help(&self) -> ToolResult {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            SETUP_HELP,
        )]))
    }

    // ---------------------------------------------------------- looking

    #[tool(
        description = "What is on screen as an outline: `[ref] kind \"text\" #id (x,y w×h) cursor`. `pointer` = clickable; `label` = an icon button's name; `overlay` = menus/popups; `region` = clickable area with no text. Canvases do not appear — use screenshot.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn snapshot(&self, Parameters(p): Parameters<SnapshotToolParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::snapshot_text(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Structured UI tree for agents using AccessKit's core node model: standard roles, names/values, bounds, hierarchy, focus, scroll state, supported actions, custom chart/plot roles, and MCP target hints. No native screen-reader adapter is involved.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn accessibility_tree(
        &self,
        Parameters(p): Parameters<SnapshotToolParams>,
    ) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::accessibility_tree(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Perform a supported action on a node_id returned by accessibility_tree. Pass its snapshotId to reject stale ids. Actions: click, focus, expand, collapse, scroll_up/down, replace_selected_text (requires value), set_value (number for sliders and numeric inputs; hexadecimal text for color fields; true/false for switches and checkboxes; exact enabled option label for selects; the reported 12/24-hour format for time fields; YYYY-MM-DD for single dates; YYYY-MM-DD/YYYY-MM-DD for date ranges), increment, or decrement. Value changes use normal widget input handling.",
        annotations(open_world_hint = false)
    )]
    async fn accessibility_action(
        &self,
        Parameters(p): Parameters<AccessibilityActionParams>,
    ) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::accessibility_action(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Temporarily draw an orange outline around a visible widget or canvas target in the running app. Target by text, id, snapshot ref, or x/y; the outline expires after duration_ms (100–5000).",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn highlight(&self, Parameters(p): Parameters<HighlightToolParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::highlight(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "PNG of a window, a node (text/id/ref) or a region, inline or saved with save_to. Set highlight=true with a target to draw a temporary outline in the running app before capturing. Says how image pixels map to logical coordinates.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn screenshot(&self, Parameters(p): Parameters<ScreenshotToolParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::screenshot(app, &p).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok((Some(png), text)) => {
                let note = apps.recording.as_ref().map(Recording::reminder);
                let mut blocks = vec![ContentBlock::image(png, "image/png")];
                blocks.push(ContentBlock::text(match note {
                    Some(note) => format!("{note}\n{text}"),
                    None => text,
                }));
                Ok(CallToolResult::success(blocks))
            }
            Ok((None, text)) => Self::reply(&apps, Ok(text)),
            Err(error) => Self::reply(&apps, Err(error)),
        }
    }

    #[tool(
        description = "Compare the screen (or a node/region) with a saved baseline: reports % changed, where, and shows a diff image. Creates the baseline on first use; update=true overwrites it.",
        annotations(open_world_hint = false)
    )]
    async fn screenshot_diff(&self, Parameters(p): Parameters<DiffParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let ctx = Ctx {
            root: apps.root.clone(),
            update_baselines: false,
        };
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::screenshot_diff(app, &ctx, &p).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(diff) => {
                let mut blocks = Vec::new();
                if let Some(png) = diff.context_png {
                    blocks.push(ContentBlock::image(
                        base64::engine::general_purpose::STANDARD.encode(png),
                        "image/png",
                    ));
                }
                blocks.push(ContentBlock::text(diff.text));
                if let Some(draft) = apps.draft.as_mut() {
                    draft.flow.steps.push(FlowStep {
                        action: "screenshot_diff".into(),
                        args: recorded_args(&p),
                    });
                }
                Ok(CallToolResult::success(blocks))
            }
            Err(error) => Self::reply(&apps, Err(error)),
        }
    }

    #[tool(
        description = "What is at a point: the widgets covering it (innermost first) and the cursor shown. Use on canvases and to check what a click at x/y would hit.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn inspect_point(&self, Parameters(p): Parameters<PointParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::inspect_point(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Audit visible UI semantics for unnamed controls, generic hit-test regions, missing checked/selected state, and missing numeric/select/calendar data. Use after UI changes; a clean report does not prove complete coverage.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn audit(&self, Parameters(p): Parameters<AuditParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::audit(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Layout diagnostics on the current screen: overlapping interactive widgets, content clipped by its container or the window, text that does not fit its widget, and duplicate widget ids. Each finding names the nodes ([ref], #id), their boxes and why it matters. Use after UI changes; `expect_no_diagnostics` makes it a flow assertion.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn diagnose(&self, Parameters(p): Parameters<DiagnoseParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::diagnose(app, &p).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok((text, value)) => {
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(match apps.recording.as_ref() {
                    Some(r) => format!("{}\n{text}", r.reminder()),
                    None => text,
                })];
                Ok(result)
            }
            Err(error) => Self::reply(&apps, Err(error)),
        }
    }

    #[tool(
        description = "Wait until text or an id appears (or disappears with gone=true). Fails with the visible text when it does not.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn wait_for(&self, Parameters(p): Parameters<WaitParams>) -> ToolResult {
        self.action("wait_for", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Wait until the screen stops changing (widget tree stable for stable_ms; ticking numbers ignored). Returns whether it settled — not settling is not an error.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn wait_idle(&self, Parameters(p): Parameters<WaitIdleParams>) -> ToolResult {
        self.action("wait_idle", p.app.as_deref(), &p).await
    }

    // ---------------------------------------------------------- acting

    #[tool(
        description = "Click a widget by text, id or ref, or a point (double click: count=2; right: button=right). Replies with the messages the app published.",
        annotations(open_world_hint = false)
    )]
    async fn click(&self, Parameters(p): Parameters<ClickParams>) -> ToolResult {
        self.action("click", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Type text, optionally clicking a target first to focus it; clear=true replaces what is there, submit=true presses Enter.",
        annotations(open_world_hint = false)
    )]
    async fn type_text(&self, Parameters(p): Parameters<TypeParams>) -> ToolResult {
        self.action("type_text", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Put text on the clipboard and paste it (Cmd/Ctrl+V), optionally into a target — instant for long text, and tests paste handling.",
        annotations(open_world_hint = false)
    )]
    async fn paste(&self, Parameters(p): Parameters<PasteParams>) -> ToolResult {
        self.action("paste", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Press a key or chord: \"Enter\", \"Escape\", \"Tab\", \"ArrowDown\", \"Mod+S\" (Cmd on macOS, Ctrl elsewhere).",
        annotations(open_world_hint = false)
    )]
    async fn press_key(&self, Parameters(p): Parameters<KeyParams>) -> ToolResult {
        self.action("press_key", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Mouse-wheel scroll over a target (or the window centre); dy > 0 scrolls down.",
        annotations(open_world_hint = false)
    )]
    async fn scroll(&self, Parameters(p): Parameters<ScrollParams>) -> ToolResult {
        self.action("scroll", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Press, move and release: sliders, splitters, panning a chart, drawing. Starts at the target; ends at to_x/to_y or start + dx/dy.",
        annotations(open_world_hint = false)
    )]
    async fn drag(&self, Parameters(p): Parameters<DragParams>) -> ToolResult {
        self.action("drag", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Move the pointer over a target and report the cursor there (pointer, text, grab, crosshair…). Opens hover menus; effects last until real mouse input.",
        annotations(open_world_hint = false)
    )]
    async fn hover(&self, Parameters(p): Parameters<HoverParams>) -> ToolResult {
        self.action("hover", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Resize, move, focus, maximize, minimize or close a window.",
        annotations(open_world_hint = false)
    )]
    async fn window(&self, Parameters(p): Parameters<WindowToolParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let action = match p.action {
            WindowOp::Resize => match (p.width, p.height) {
                (Some(width), Some(height)) => WindowAction::Resize { width, height },
                _ => return Self::reply(&apps, Err("resize needs width and height".into())),
            },
            WindowOp::Move => match (p.x, p.y) {
                (Some(x), Some(y)) => WindowAction::Move { x, y },
                _ => return Self::reply(&apps, Err("move needs x and y".into())),
            },
            WindowOp::Focus => WindowAction::Focus,
            WindowOp::Maximize => WindowAction::Maximize { on: true },
            WindowOp::Unmaximize => WindowAction::Maximize { on: false },
            WindowOp::Minimize => WindowAction::Minimize { on: true },
            WindowOp::Restore => WindowAction::Minimize { on: false },
            WindowOp::Close => WindowAction::Close,
        };
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::request(
                app,
                Request::Window(WindowParams {
                    window: p.window.clone(),
                    action,
                }),
            )
            .await
            .map(|_| format!("Window {:?}: done.", p.action)),
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Run several steps in one call when you need not look in between: click, type_text, press_key, scroll, drag, hover, paste, wait_for, wait_idle, sleep, run_app_action, load_fixture, query, expect_query, expect_message, screenshot_diff, expect_no_diagnostics, echo. Stops at the first failure.",
        annotations(open_world_hint = false)
    )]
    async fn run_sequence(&self, Parameters(p): Parameters<SequenceParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let ctx = Ctx {
            root: apps.root.clone(),
            update_baselines: false,
        };
        let keep_going = p.continue_on_error.unwrap_or(false);
        let mut out = String::new();
        let mut failed = false;
        let mut recorded = Vec::new();
        let name = match apps.get(p.app.as_deref()) {
            Ok(app) => app.name.clone(),
            Err(error) => return Self::reply(&apps, Err(error)),
        };
        for (i, step) in p.steps.iter().enumerate() {
            let Ok(app) = apps.get(Some(&name)) else {
                break;
            };
            match ops::run_step(app, &ctx, step).await {
                Ok(text) => {
                    let _ = writeln!(out, "{}. {}: {}", i + 1, step.action, text.trim_end());
                    recorded.push(FlowStep {
                        action: step.action.clone(),
                        args: step.args.clone(),
                    });
                }
                Err(error) => {
                    failed = true;
                    let _ = writeln!(out, "{}. {} FAILED: {error}", i + 1, step.action);
                    if !keep_going {
                        let _ =
                            writeln!(out, "Stopped; {} step(s) not run.", p.steps.len() - i - 1);
                        break;
                    }
                }
            }
        }
        if p.snapshot.unwrap_or(false) {
            if let Ok(app) = apps.get(Some(&name)) {
                match ops::snapshot_text(app, &SnapshotToolParams::default()).await {
                    Ok(snap) => {
                        let _ = write!(out, "\n{snap}");
                    }
                    Err(error) => {
                        let _ = write!(out, "\nsnapshot failed: {error}");
                    }
                }
            }
        }
        if let Some(draft) = apps.draft.as_mut().filter(|d| d.app == name) {
            draft.flow.steps.extend(recorded);
        }
        let outcome = if failed { Err(out) } else { Ok(out) };
        Self::reply(&apps, outcome)
    }

    // ---------------------------------------------------------- the app's side

    #[tool(
        description = "The app's recent messages (Debug form) — what its update() received. Use `since` from an earlier reply to see only new ones.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn messages(&self, Parameters(p): Parameters<MessagesToolParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => {
                match ops::read_messages(
                    app,
                    p.since,
                    p.filter.clone(),
                    Some(p.limit.unwrap_or(30)),
                )
                .await
                {
                    Ok(log) => {
                        let mut text = ops::format_messages(&log, usize::MAX);
                        if log.entries.is_empty() {
                            let _ = write!(text, "No messages yet (newest seq {}).", log.last);
                        } else {
                            let _ = write!(
                                text,
                                "Newest seq {} — pass it as `since` to read on.",
                                log.last
                            );
                        }
                        Ok(text)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "stdout/stderr of an app launched here (build output, logs, panics). Pass the returned cursor as `since` to read only new lines.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn logs(&self, Parameters(p): Parameters<LogsParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(App {
                launched: Some(launched),
                ..
            }) => {
                let read = launched.logs.read(
                    p.since,
                    p.tail.unwrap_or(60),
                    p.max_chars.unwrap_or(8000),
                    p.filter.as_deref(),
                );
                let mut text = if read.lines.is_empty() {
                    "(no new output)".to_owned()
                } else {
                    read.lines.join("\n")
                };
                if read.dropped > 0 {
                    let _ = write!(
                        text,
                        "\n[{} older matching line(s) not shown: narrow with filter or since, or raise tail/max_chars]",
                        read.dropped
                    );
                }
                let _ = write!(text, "\ncursor: {}", read.cursor);
                Ok(text)
            }
            Ok(app) => Err(format!(
                "{} was connected, not launched here: its output is not captured",
                app.name
            )),
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "The app's state as text (its Debug, as the app chose to share it), optionally only lines matching `filter`. Use to verify an action changed the model.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn state(&self, Parameters(p): Parameters<StateToolParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::state(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Frame rate and time spent in the app's view() and update(), with the costliest message kinds. reset=true starts a fresh measurement — reset, interact, then read.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn perf(&self, Parameters(p): Parameters<PerfToolParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::perf(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Read the system clipboard through the app, or set it with `write`.",
        annotations(open_world_hint = false)
    )]
    async fn clipboard(&self, Parameters(p): Parameters<ClipboardToolParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::clipboard(app, &p).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Named actions the app registered for agents (open a scene, load a fixture…), with what each takes.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn app_actions(&self, Parameters(p): Parameters<AppOnly>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let value = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::app_actions_value(app).await,
            Err(error) => Err(error),
        };
        match value {
            Ok(value) => {
                let text = serde_json::to_string_pretty(&value)
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(text)];
                Ok(result)
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(error)])),
        }
    }

    #[tool(
        description = "Ask one of the app's read-only queries (see app_actions, kind: query) — e.g. what is under a point of a chart, the bars in view. Returns JSON (also as structuredContent). Changes nothing.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn query(&self, Parameters(p): Parameters<QueryParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::query(app, &p.name, &p.args).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(value) => {
                if let Some(draft) = apps.draft.as_mut() {
                    draft.flow.steps.push(FlowStep {
                        action: "query".into(),
                        args: recorded_args(&p),
                    });
                }
                let text = ops::clip_json(&value, p.max_chars.unwrap_or(8000));
                let mut result = CallToolResult::structured(value);
                result.content = vec![ContentBlock::text(match apps.recording.as_ref() {
                    Some(r) => format!("{}\n{text}", r.reminder()),
                    None => text,
                })];
                Ok(result)
            }
            Err(error) => Self::reply(&apps, Err(error)),
        }
    }

    #[tool(
        description = "The fixtures the app registered: named states to start a test from (an empty cart, a signed-in user…), with what args each takes.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn fixtures(&self, Parameters(p): Parameters<AppOnly>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.get(p.app.as_deref()) {
            Ok(app) => ops::fixtures(app).await,
            Err(error) => Err(error),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Put the app in one of its fixtures (see `fixtures`): a known state, whatever ran before. Replies with the messages it caused. Flows can name a `fixture` instead, and `launch {fixture}` starts an app in one.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn load_fixture(&self, Parameters(p): Parameters<FixtureParams>) -> ToolResult {
        self.action("load_fixture", p.app.as_deref(), &p).await
    }

    #[tool(
        description = "Run one of the app's registered actions with JSON args; replies with the messages it caused.",
        annotations(open_world_hint = false)
    )]
    async fn run_app_action(&self, Parameters(p): Parameters<AppActionParams>) -> ToolResult {
        self.action("run_app_action", p.app.as_deref(), &p).await
    }

    // ---------------------------------------------------------- recording

    #[tool(
        description = "Start recording a window as video (distinct frames at `fps`; mp4 via ffmpeg on stop). Every reply reminds you while it runs.",
        annotations(open_world_hint = false)
    )]
    async fn record_start(&self, Parameters(p): Parameters<RecordStartParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        if let Some(recording) = &apps.recording {
            let error = format!(
                "already recording {}: call record_stop first",
                recording.app
            );
            return Self::reply(&apps, Err(error));
        }
        let root = apps.root.clone();
        let target = match apps.get(p.app.as_deref()) {
            Ok(App {
                name,
                conn: Some(conn),
                token,
                ..
            }) => (name.clone(), conn.port, token.clone()),
            Ok(app) => {
                let error = format!("{} is not connected", app.name);
                return Self::reply(&apps, Err(error));
            }
            Err(error) => return Self::reply(&apps, Err(error)),
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let dir = root
            .join(".iced-mcp")
            .join("recordings")
            .join(format!("{}-{stamp}", target.0));
        let options = record::Options {
            fps: p.fps.unwrap_or(6),
            limit: Duration::from_secs(p.max_seconds.unwrap_or(120).clamp(1, 600)),
            max_width: p.max_width.unwrap_or(1280),
            window: p.window.clone(),
        };
        let outcome =
            match record::start(target.1, target.2, target.0.clone(), dir.clone(), options).await {
                Ok(recording) => {
                    let text = format!(
                        "Recording {} at {} fps into {} (auto-stops after {}s).",
                        target.0,
                        recording.fps,
                        dir.display(),
                        recording.limit.as_secs()
                    );
                    apps.recording = Some(recording);
                    Ok(text)
                }
                Err(error) => Err(error),
            };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Stop the screen recording and return the video (or frames) path.",
        annotations(open_world_hint = false)
    )]
    async fn record_stop(&self) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let outcome = match apps.recording.take() {
            Some(recording) => Ok(recording.stop().await),
            None => Err("no recording is running".into()),
        };
        Self::reply(&apps, outcome)
    }

    // ---------------------------------------------------------- flows

    #[tool(
        description = "Start recording a flow: from now on every successful action on that app (and screenshot_diff) is saved as a step. Add assertions with flow_add_step; save with flow_finish.",
        annotations(open_world_hint = false)
    )]
    async fn flow_start(&self, Parameters(p): Parameters<FlowStartParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        if let Err(error) = flow::check_name(&p.name) {
            return Self::reply(&apps, Err(error));
        }
        let (app_name, launch, fixture) = match apps.get(p.app.as_deref()) {
            Ok(app) => (app.name.clone(), app.launch.clone(), app.fixture.clone()),
            Err(error) => return Self::reply(&apps, Err(error)),
        };
        let discarded = apps.draft.as_ref().map_or(0, |d| d.flow.steps.len());
        let end_to_end = launch.is_some() && p.prerequisite.is_none();
        apps.draft = Some(Draft {
            name: p.name.clone(),
            app: app_name.clone(),
            flow: Flow {
                schema: None,
                prerequisite: p.prerequisite.clone(),
                launch: if end_to_end { launch } else { None },
                fixture: fixture.as_ref().map(|(name, _)| name.clone()),
                fixture_args: fixture.and_then(|(_, args)| (!args.is_null()).then_some(args)),
                steps: Vec::new(),
            },
        });
        let mut text = format!(
            "Recording flow {} on {app_name} ({}).",
            p.name,
            if end_to_end {
                "end to end: replays launch the app with the same command"
            } else {
                "a fragment: replays run on whatever app is connected"
            }
        );
        if discarded > 0 {
            let _ = write!(
                text,
                " Discarded an unfinished draft of {discarded} step(s)."
            );
        }
        Self::reply(&apps, Ok(text))
    }

    #[tool(
        description = "Run one step and add it to the flow being recorded — for assertions like expect_message, or echo notes. Same actions as run_sequence.",
        annotations(open_world_hint = false)
    )]
    async fn flow_add_step(&self, Parameters(p): Parameters<FlowStepParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let Some(app_name) = apps.draft.as_ref().map(|d| d.app.clone()) else {
            return Self::reply(
                &apps,
                Err("no flow is being recorded: call flow_start first".into()),
            );
        };
        let ctx = Ctx {
            root: apps.root.clone(),
            update_baselines: false,
        };
        let step = crate::params::Step {
            action: p.action.clone(),
            args: p.args.clone(),
        };
        let outcome = match apps.get(Some(&app_name)) {
            Ok(app) => ops::run_step(app, &ctx, &step).await,
            Err(error) => Err(error),
        };
        let outcome = match outcome {
            Ok(text) => {
                if let Some(draft) = apps.draft.as_mut() {
                    draft.flow.steps.push(FlowStep {
                        action: p.action.clone(),
                        args: recorded_args(&p.args),
                    });
                    Ok(format!("Step {} recorded: {text}", draft.flow.steps.len()))
                } else {
                    Ok(text)
                }
            }
            Err(error) => Err(format!("{error}\n(not recorded)")),
        };
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Save the flow being recorded to .iced-mcp/flows/<name>.json and show its steps.",
        annotations(open_world_hint = false)
    )]
    async fn flow_finish(&self) -> ToolResult {
        let mut apps = self.apps.lock().await;
        let Some(draft) = apps.draft.take() else {
            return Self::reply(&apps, Err("no flow is being recorded".into()));
        };
        let outcome = flow::save(&apps.root, &draft.name, &draft.flow).map(|path| {
            let mut text = format!(
                "Saved flow {} ({} steps) to {}:\n",
                draft.name,
                draft.flow.steps.len(),
                path.display()
            );
            for (i, step) in draft.flow.steps.iter().enumerate() {
                let _ = writeln!(text, "  {}. {}", i + 1, flow::label(step));
            }
            text.push_str("Replay with flow_run.");
            text
        });
        Self::reply(&apps, outcome)
    }

    #[tool(
        description = "Replay a saved flow as a regression test: one line per step and PASS/FAIL. Stops at the first failure. With no name, lists the saved flows.",
        annotations(open_world_hint = false)
    )]
    async fn flow_run(&self, Parameters(p): Parameters<FlowRunParams>) -> ToolResult {
        let mut apps = self.apps.lock().await;
        if p.name.is_empty() {
            let names = flow::list(&apps.root);
            let text = if names.is_empty() {
                format!("No flows in {}.", flow::dir(&apps.root).display())
            } else {
                names.join("\n")
            };
            return Self::reply(&apps, Ok(text));
        }
        let run = run_flow(&mut apps, &p).await;
        Self::reply(
            &apps,
            if run.passed {
                Ok(run.text)
            } else {
                Err(run.text)
            },
        )
    }
}

/// The result of one flow run: its report, and the failure bundle if a
/// step failed.
pub(crate) struct FlowRun {
    pub(crate) passed: bool,
    pub(crate) text: String,
    pub(crate) bundle: Option<PathBuf>,
}

impl FlowRun {
    fn failed(text: String, bundle: Option<PathBuf>) -> Self {
        let text = match &bundle {
            Some(path) => format!("{text}\nFailure bundle: {}", path.display()),
            None => text,
        };
        Self {
            passed: false,
            text,
            bundle,
        }
    }
}

/// Runs one saved flow: end to end on a fresh app it launches, or as a
/// fragment on `p.app`. On a failure, writes a bundle while the app is
/// still up.
pub(crate) async fn run_flow(apps: &mut Apps, p: &FlowRunParams) -> FlowRun {
    let root = apps.root.clone();
    let loaded = match flow::load(&root, &p.name) {
        Ok(loaded) => loaded,
        Err(error) => return FlowRun::failed(error, None),
    };
    let ctx = Ctx {
        root: root.clone(),
        update_baselines: p.update_baselines.unwrap_or(false),
    };
    let started = Instant::now();

    // End to end: launch a fresh app for the run; a fragment uses one.
    let (app_name, launched_here) = match &loaded.launch {
        Some(launch) => {
            let name = format!("flow-{}", p.name);
            let outcome = async {
                let spec = with_fixture(
                    spec_for(launch, None, None)?,
                    loaded.fixture.as_deref(),
                    loaded.fixture_args.as_ref(),
                );
                start(
                    Some(name.clone()),
                    spec,
                    launch.clone(),
                    Duration::from_secs(300),
                )
                .await
            }
            .await;
            match outcome {
                Ok((app, _)) => {
                    apps.add(app).await;
                    (name, true)
                }
                Err(error) => {
                    let error = format!("launch failed: {error}");
                    let failure = bundle::Failure {
                        flow: &p.name,
                        step: "launch",
                        error: &error,
                    };
                    let path = bundle::write(&root, &failure, &loaded, None).await.ok();
                    return FlowRun::failed(error, path);
                }
            }
        }
        None => {
            // A fixture sets up the state a prerequisite would describe.
            let prerequisite = loaded
                .prerequisite
                .as_ref()
                .filter(|_| loaded.fixture.is_none());
            if let Some(prerequisite) = prerequisite {
                if !p.prerequisite_ok.unwrap_or(false) {
                    let error = format!(
                        "flow {} is a fragment that needs: \"{prerequisite}\". Make that true, then call again with prerequisite_ok=true.",
                        p.name
                    );
                    return FlowRun::failed(error, None);
                }
            }
            match apps.get(p.app.as_deref()) {
                Ok(app) => (app.name.clone(), false),
                Err(error) => return FlowRun::failed(error, None),
            }
        }
    };

    // A fragment loads its fixture into the app it runs on (an end-to-end
    // flow started the app in it).
    if let (Some(fixture), false) = (&loaded.fixture, launched_here) {
        let params = FixtureParams {
            app: None,
            name: fixture.clone(),
            args: loaded.fixture_args.clone().unwrap_or_default(),
        };
        let outcome = match apps.get(Some(&app_name)) {
            Ok(app) => ops::load_fixture(app, &params).await,
            Err(error) => Err(error),
        };
        if let Err(error) = outcome {
            let error = format!("fixture {fixture}: {error}");
            let failure = bundle::Failure {
                flow: &p.name,
                step: "fixture",
                error: &error,
            };
            let path = bundle::write(&root, &failure, &loaded, apps.get(Some(&app_name)).ok())
                .await
                .ok();
            return FlowRun::failed(error, path);
        }
    }

    let mut reports: Vec<StepReport> = Vec::new();
    let mut bundle_path = None;
    let mut stopped = false;
    for (index, step) in loaded.steps.iter().enumerate() {
        let label = flow::label(step);
        if stopped {
            reports.push(StepReport {
                index,
                label,
                status: StepStatus::Skip,
                detail: String::new(),
                duration: Duration::ZERO,
            });
            continue;
        }
        let step_started = Instant::now();
        let outcome = match apps.get(Some(&app_name)) {
            Ok(app) => {
                ops::run_step(
                    app,
                    &ctx,
                    &crate::params::Step {
                        action: step.action.clone(),
                        args: step.args.clone(),
                    },
                )
                .await
            }
            Err(error) => Err(error),
        };
        let duration = step_started.elapsed();
        let (status, detail) = match outcome {
            Ok(text) if step.action == "echo" => (StepStatus::Pass, text),
            Ok(_) => (StepStatus::Pass, String::new()),
            Err(error) => {
                stopped = true;
                let infrastructure = [
                    "not connected",
                    "connection broke",
                    "closed the connection",
                    "bad args",
                    "unknown action",
                    "did not answer",
                ]
                .iter()
                .any(|m| error.contains(m));
                let failure = bundle::Failure {
                    flow: &p.name,
                    step: &format!("[{}] {label}", index + 1),
                    error: &error,
                };
                bundle_path =
                    bundle::write(&root, &failure, &loaded, apps.get(Some(&app_name)).ok())
                        .await
                        .ok();
                (
                    if infrastructure {
                        StepStatus::Error
                    } else {
                        StepStatus::Fail
                    },
                    error,
                )
            }
        };
        reports.push(StepReport {
            index,
            label,
            status,
            detail,
            duration,
        });
    }
    if launched_here {
        apps.remove(&app_name, true).await;
    }
    let text = flow::render(&p.name, &reports, started.elapsed());
    if let Some(path) = &bundle_path {
        bundle::add_report(path, &text);
    }
    if reports.iter().all(|r| r.status == StepStatus::Pass) {
        FlowRun {
            passed: true,
            text,
            bundle: None,
        }
    } else {
        FlowRun::failed(text, bundle_path)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schemas_build() {
        let tools = Server::new().tool_router.list_all();
        let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
        for expected in [
            "launch",
            "snapshot",
            "click",
            "type_text",
            "screenshot",
            "run_sequence",
            "wait_idle",
            "screenshot_diff",
            "flow_start",
            "flow_run",
            "perf",
            "state",
            "record_start",
            "audit",
            "diagnose",
            "fixtures",
            "load_fixture",
        ] {
            assert!(
                names.contains(&expected.to_owned()),
                "{expected} missing from {names:?}"
            );
        }
    }

    #[test]
    fn typing_records_its_text_beside_a_target() {
        let typing = TypeParams {
            text: "flow item".into(),
            target: Target {
                id: Some("new-todo".into()),
                ..Target::default()
            },
            submit: Some(true),
            ..TypeParams::default()
        };
        let args = recorded_args(&typing);
        assert_eq!(
            args,
            serde_json::json!({ "text": "flow item", "id": "new-todo", "submit": true })
        );
        let back: TypeParams = serde_json::from_value(args).unwrap();
        assert_eq!(back.text, "flow item");
    }

    #[test]
    fn a_launch_in_a_fixture_sets_its_environment() {
        let launch = FlowLaunch {
            command: "my-app --flag".into(),
            cwd: None,
            env: std::collections::HashMap::new(),
        };
        let spec = spec_for(&launch, None, None).unwrap();
        assert!(with_fixture(spec.clone(), None, None).env.is_empty());
        let args = serde_json::json!({ "items": 2 });
        let spec = with_fixture(spec, Some("full-cart"), Some(&args));
        assert_eq!(spec.env[ENV_FIXTURE], "full-cart");
        assert_eq!(spec.env[ENV_FIXTURE_ARGS], r#"{"items":2}"#);
        let plain = with_fixture(spec, Some("empty"), Some(&serde_json::Value::Null));
        assert_eq!(plain.env[ENV_FIXTURE], "empty");
        assert!(!plain.env.contains_key(ENV_FIXTURE_ARGS), "no stale args");
    }

    #[test]
    fn recorded_steps_drop_nulls_and_the_app() {
        let click = ClickParams {
            app: Some("todos".into()),
            target: Target {
                text: Some("Add".into()),
                ..Target::default()
            },
            ..ClickParams::default()
        };
        assert_eq!(recorded_args(&click), serde_json::json!({ "text": "Add" }));
    }
}
