//! Lets AI agents see and drive a running [iced] application.
//!
//! The bridge wraps your program the way iced's own tester and devtools do:
//! it adds a loopback socket, a widget wrapped around every window's view,
//! and a log of your messages. The `iced-mcp-server` binary speaks MCP to the
//! agent and this protocol to the app, so an agent can read the widget tree,
//! click and type by text or id, scroll, drag, take screenshots and read what
//! the app did in response — without the OS accessibility APIs or pixel
//! guessing.
//!
//! ```no_run
//! # #[derive(Default)] struct App; #[derive(Debug, Clone)] enum Message {}
//! # impl App { fn update(&mut self, _: Message) {} fn view(&self) -> iced::Element<'_, Message> { iced::widget::text("").into() } }
//! fn main() -> iced::Result {
//!     let app = iced::application(App::default, App::update, App::view);
//!     // Attached only when `ICED_MCP=1`; otherwise exactly `app.run()`.
//!     iced_mcp::run(app)
//! }
//! ```
//!
//! Only build it into development binaries (behind a cargo feature): anyone
//! who can reach the port can drive the app. It binds `127.0.0.1` only, and
//! `ICED_MCP_TOKEN` makes connections present a token first.
//!
//! [iced]: https://iced.rs

mod bridge;
mod capture;
mod collect;
mod input;
mod inspector;
mod log;
mod perf;
mod registry;

pub use iced_mcp_protocol as protocol;

use bridge::Command;
use iced_mcp_protocol::{
    Acted, ActionInfo, ActionKind, ActionList, AppInfo, ClipboardContent, Outcome, QueryValue,
    Request, Response, ScreenshotParams, StateDump, WindowAction, WindowInfo, DEFAULT_PORT,
    ENV_ENABLE, ENV_PORT, ENV_TOKEN, PROTOCOL_VERSION,
};
use inspector::Inspector;
use registry::{Job, Registry, Replier};

use iced::application::Application;
use iced::message::{MaybeClone, MaybeDebug};
use iced::theme;
use iced::Program;
use iced::{window, Element, Point, Settings, Size, Subscription, Task};

use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::stream::{self, Stream, StreamExt};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Messages kept for the `messages` tool unless configured otherwise.
const DEFAULT_MESSAGE_LOG: usize = 2000;

/// Whether `ICED_MCP` asks for the bridge (set, and not `0`/`false`).
pub fn enabled() -> bool {
    std::env::var(ENV_ENABLE).is_ok_and(|v| !matches!(v.trim(), "" | "0" | "false"))
}

/// Runs `application`, with the bridge attached when [`enabled`] — the one
/// line most apps need.
pub fn run<P>(application: Application<P>) -> iced::Result
where
    P: Program + 'static,
    P::Message: fmt::Debug + MaybeDebug + MaybeClone,
{
    if enabled() {
        attach(application).run()
    } else {
        application.run()
    }
}

/// Wraps any iced [`Program`] — usually an [`Application`] — with the bridge.
pub fn attach<P: Program>(program: P) -> Attach<P> {
    Attach {
        program,
        config: Config::from_env(),
        registry: Registry::default(),
        describe_state: None,
        actions: Vec::new(),
        queries: Vec::new(),
    }
}

/// [`run`], also sharing the app's state with the `state` tool (see
/// [`Attach::state`]).
pub fn run_with_state<P>(
    application: Application<P>,
    describe: impl Fn(&P::State) -> String + Send + Sync + 'static,
) -> iced::Result
where
    P: Program + 'static,
    P::Message: fmt::Debug + MaybeDebug + MaybeClone,
{
    if enabled() {
        attach(application).state(describe).run()
    } else {
        application.run()
    }
}

/// Where and how the bridge listens.
#[derive(Debug, Clone)]
pub struct Config {
    /// Loopback port; `0` picks a free one (printed to stderr).
    pub port: u16,
    /// When set, connections must say `hello` with it first.
    pub token: Option<String>,
    /// Messages kept for the agent; `0` turns the log off.
    pub message_log: usize,
}

impl Config {
    /// `ICED_MCP_PORT` and `ICED_MCP_TOKEN`, with defaults.
    pub fn from_env() -> Self {
        Self {
            port: std::env::var(ENV_PORT)
                .ok()
                .and_then(|p| p.trim().parse().ok())
                .unwrap_or(DEFAULT_PORT),
            token: std::env::var(ENV_TOKEN).ok().filter(|t| !t.is_empty()),
            message_log: DEFAULT_MESSAGE_LOG,
        }
    }
}

/// A [`Program`] with the bridge attached.
pub struct Attach<P: Program> {
    program: P,
    config: Config,
    registry: Registry,
    describe_state: Option<StateFn<P>>,
    actions: Vec<AppAction<P>>,
    queries: Vec<AppQuery<P>>,
}

/// Answers a query from the app's state.
type QueryFn<P> = Arc<
    dyn Fn(&<P as Program>::State, &serde_json::Value) -> Result<serde_json::Value, String>
        + Send
        + Sync,
>;

struct AppQuery<P: Program> {
    name: String,
    description: String,
    output_schema: Option<serde_json::Value>,
    run: QueryFn<P>,
}

/// Turns an action's JSON arguments into the message that performs it.
type ActionFn<P> =
    Arc<dyn Fn(&serde_json::Value) -> Result<<P as Program>::Message, String> + Send + Sync>;

struct AppAction<P: Program> {
    name: String,
    description: String,
    run: ActionFn<P>,
}

/// Renders the app's state for the `state` tool.
type StateFn<P> = Arc<dyn Fn(&<P as Program>::State) -> String + Send + Sync>;

/// State dumps are cut here unless the request asks for less.
const DEFAULT_STATE_CHARS: usize = 20_000;

impl<P: Program + 'static> Attach<P>
where
    P::Message: fmt::Debug,
{
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Lets agents read the app's state with the `state` tool, rendered by
    /// `describe` — usually `|state| format!("{state:#?}")`, or a summary
    /// that leaves out what must not leave the app (keys, credentials).
    /// Without it, `state` reports that the app does not share its state.
    pub fn state(mut self, describe: impl Fn(&P::State) -> String + Send + Sync + 'static) -> Self {
        self.describe_state = Some(Arc::new(describe));
        self
    }

    /// Offers agents a named action: `run` turns its JSON `args` into the
    /// message that does it (or an error saying what is wrong with them).
    /// The message goes through your `update` like any other, so actions
    /// cannot reach anything your own messages cannot. Say in
    /// `description` what it does and which `args` it takes.
    ///
    /// ```ignore
    /// iced_mcp::attach(app)
    ///     .action("open_settings", "Opens the settings dialog. No args.", |_| Ok(Message::OpenSettings))
    ///     .run()
    /// ```
    pub fn action(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        run: impl Fn(&serde_json::Value) -> Result<P::Message, String> + Send + Sync + 'static,
    ) -> Self {
        self.actions.push(AppAction {
            name: name.into(),
            description: description.into(),
            run: Arc::new(run),
        });
        self
    }

    /// Offers agents a read-only question about the app: `run` answers it
    /// from the current state as JSON (or says what is wrong with the
    /// `args`). It runs on the UI thread between frames and cannot change
    /// anything — use it for what the screen cannot say precisely: what is
    /// under a point of a chart, the bars in view, a model's internals.
    ///
    /// ```ignore
    /// iced_mcp::attach(app)
    ///     .query("selection", "The selected items. No args.", |state, _| {
    ///         Ok(serde_json::json!(state.selection))
    ///     })
    ///     .run()
    /// ```
    pub fn query(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        run: impl Fn(&P::State, &serde_json::Value) -> Result<serde_json::Value, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.queries.push(AppQuery {
            name: name.into(),
            description: description.into(),
            output_schema: None,
            run: Arc::new(run),
        });
        self
    }

    /// [`Attach::query`] with the JSON Schema of its answer (for instance
    /// `serde_json::to_value(schemars::schema_for!(Reply))`), which agents
    /// see in `app_actions` and can rely on.
    pub fn query_with_schema(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        output_schema: serde_json::Value,
        run: impl Fn(&P::State, &serde_json::Value) -> Result<serde_json::Value, String>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self = self.query(name, description, run);
        if let Some(query) = self.queries.last_mut() {
            query.output_schema = Some(output_schema);
        }
        self
    }

    /// Starts the bridge and runs the program. If the port is taken the app
    /// still runs, without the bridge, and says so on stderr.
    pub fn run(self) -> iced::Result {
        let (sender, receiver) = mpsc::unbounded();
        if let Ok(mut inbox) = INBOX.lock() {
            *inbox = Some(receiver);
        }
        match bridge::start(&self.config, sender) {
            Ok(port) => eprintln!("iced-mcp: listening on 127.0.0.1:{port}"),
            Err(error) => eprintln!(
                "iced-mcp: could not listen on 127.0.0.1:{} ({error}); running without the bridge",
                self.config.port
            ),
        }
        iced_winit::run(self).map_err(iced::Error::from)
    }
}

/// The program's state, with the bridge's beside it.
pub struct State<P: Program> {
    app: P::State,
    registry: Registry,
    log: log::Log,
}

/// The program's messages, with the bridge's beside them.
pub struct Message<P: Program>(Inner<P>);

enum Inner<P: Program> {
    App(P::Message),
    Command(Command),
    Shot(Arc<window::Screenshot>, ScreenshotParams, Replier),
    Closed(window::Id),
    HighlightTick,
    Clip(Option<String>, Replier),
}

impl<P: Program> fmt::Debug for Message<P>
where
    P::Message: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Inner::App(message) => message.fmt(f),
            Inner::Command(command) => write!(f, "IcedMcp({:?})", command.request),
            Inner::Shot(..) => f.write_str("IcedMcp(Screenshot)"),
            Inner::Closed(id) => write!(f, "IcedMcp(Closed({id}))"),
            Inner::HighlightTick => f.write_str("IcedMcp(HighlightTick)"),
            Inner::Clip(..) => f.write_str("IcedMcp(Clipboard)"),
        }
    }
}

impl<P: Program> Clone for Message<P>
where
    P::Message: Clone,
{
    fn clone(&self) -> Self {
        Self(match &self.0 {
            Inner::App(message) => Inner::App(message.clone()),
            Inner::Command(command) => Inner::Command(command.clone()),
            Inner::Shot(shot, params, reply) => {
                Inner::Shot(shot.clone(), params.clone(), reply.clone())
            }
            Inner::Closed(id) => Inner::Closed(*id),
            Inner::HighlightTick => Inner::HighlightTick,
            Inner::Clip(text, reply) => Inner::Clip(text.clone(), reply.clone()),
        })
    }
}

fn app<P: Program>(message: P::Message) -> Message<P> {
    Message(Inner::App(message))
}

impl<P> Program for Attach<P>
where
    P: Program + 'static,
    P::Message: fmt::Debug,
{
    type State = State<P>;
    type Message = Message<P>;
    type Theme = P::Theme;
    type Renderer = P::Renderer;
    type Executor = P::Executor;

    fn name() -> &'static str {
        P::name()
    }

    fn settings(&self) -> Settings {
        self.program.settings()
    }

    fn window(&self) -> Option<window::Settings> {
        self.program.window()
    }

    fn boot(&self) -> (Self::State, Task<Self::Message>) {
        let (app_state, task) = self.program.boot();
        let state = State {
            app: app_state,
            registry: self.registry.clone(),
            log: log::Log::new(self.config.message_log),
        };
        (state, task.map(app::<P>))
    }

    fn update(&self, state: &mut Self::State, message: Self::Message) -> Task<Self::Message> {
        match message.0 {
            Inner::App(message) => {
                let text = log::describe(&message);
                let kind = perf::kind(&text).to_owned();
                state.log.push(text);
                let started = Instant::now();
                let task = self.program.update(&mut state.app, message);
                state.registry.lock().perf.update(&kind, started.elapsed());
                task.map(app::<P>)
            }
            Inner::Clip(text, reply) => {
                let _ = reply.try_send(Outcome::Ok(Response::Clipboard(ClipboardContent { text })));
                Task::none()
            }
            Inner::Command(command) => self.command(state, command),
            Inner::Shot(shot, params, reply) => {
                // Encoding a full frame is too slow for the UI thread.
                let _ = std::thread::Builder::new()
                    .name("iced-mcp-png".into())
                    .spawn(move || {
                        let outcome = match capture::encode(&shot, &params) {
                            Ok(png) => Outcome::Ok(Response::Screenshot(png)),
                            Err(error) => Outcome::Error(error),
                        };
                        let _ = reply.try_send(outcome);
                    });
                Task::none()
            }
            Inner::Closed(id) => {
                state.registry.lock().remove(id);
                Task::none()
            }
            Inner::HighlightTick => {
                let now = Instant::now();
                for slot in &mut state.registry.lock().slots {
                    if slot
                        .highlight
                        .is_some_and(|highlight| highlight.expires_at <= now)
                    {
                        slot.highlight = None;
                    }
                }
                Task::none()
            }
        }
    }

    fn view<'a>(
        &self,
        state: &'a Self::State,
        window: window::Id,
    ) -> Element<'a, Self::Message, Self::Theme, Self::Renderer> {
        let started = Instant::now();
        let content = self.program.view(&state.app, window).map(app::<P>);
        state.registry.lock().perf.view.push(started.elapsed());
        Inspector::new(content, state.registry.clone(), window).into()
    }

    fn title(&self, state: &Self::State, window: window::Id) -> String {
        self.program.title(&state.app, window)
    }

    fn subscription(&self, state: &Self::State) -> Subscription<Self::Message> {
        let mut subscriptions = vec![
            self.program.subscription(&state.app).map(app::<P>),
            Subscription::run(inbox).map(|command| Message(Inner::Command(command))),
            window::close_events().map(|id| Message(Inner::Closed(id))),
        ];
        if state
            .registry
            .lock()
            .slots
            .iter()
            .any(|slot| slot.highlight.is_some())
        {
            subscriptions
                .push(Subscription::run(highlight_ticks).map(|()| Message(Inner::HighlightTick)));
        }
        Subscription::batch(subscriptions)
    }

    fn theme(&self, state: &Self::State, window: window::Id) -> Option<Self::Theme> {
        self.program.theme(&state.app, window)
    }

    fn style(&self, state: &Self::State, theme: &Self::Theme) -> theme::Style {
        self.program.style(&state.app, theme)
    }

    fn scale_factor(&self, state: &Self::State, window: window::Id) -> f32 {
        self.program.scale_factor(&state.app, window)
    }
}

impl<P> Attach<P>
where
    P: Program + 'static,
    P::Message: fmt::Debug,
{
    fn command(
        &self,
        state: &mut State<P>,
        Command { request, reply }: Command,
    ) -> Task<Message<P>> {
        let answer = |outcome: Outcome| {
            let _ = reply.try_send(outcome);
        };
        let window_id = |name: Option<&str>| -> Result<window::Id, String> {
            let mut windows = state.registry.lock();
            if let Some(slot) = windows.find(name) {
                return Ok(slot.id);
            }
            let known: Vec<String> = windows.slots.iter().map(|s| s.id.to_string()).collect();
            Err(match name {
                Some(name) => format!("no window '{name}'; open windows: {known:?}"),
                None => "no window has rendered yet".into(),
            })
        };

        match request {
            Request::Hello(_) => {
                let windows = state
                    .registry
                    .lock()
                    .slots
                    .iter()
                    .map(|slot| WindowInfo {
                        id: slot.id.to_string(),
                        title: self.program.title(&state.app, slot.id),
                        size: iced_mcp_protocol::Size {
                            width: slot.size.width,
                            height: slot.size.height,
                        },
                    })
                    .collect();
                answer(Outcome::Ok(Response::Hello(AppInfo {
                    name: P::name().to_owned(),
                    toolkit: "iced 0.14".into(),
                    protocol: PROTOCOL_VERSION,
                    pid: std::process::id(),
                    windows,
                    messages: state.log.is_enabled(),
                })));
                Task::none()
            }
            Request::Messages(params) => {
                answer(Outcome::Ok(Response::Messages(state.log.read(&params))));
                Task::none()
            }
            Request::Perf(params) => {
                let mut windows = state.registry.lock();
                answer(Outcome::Ok(Response::Perf(windows.perf.report())));
                if params.reset {
                    windows.perf = perf::Perf::default();
                }
                Task::none()
            }
            Request::State(params) => {
                answer(match &self.describe_state {
                    Some(describe) => Outcome::Ok(Response::State(state_dump(
                        &describe(&state.app),
                        params.filter.as_deref(),
                        params.max_chars.unwrap_or(DEFAULT_STATE_CHARS),
                    ))),
                    None => Outcome::Error(
                        "this app does not share its state: build it with \
                         `iced_mcp::run_with_state(app, |s| format!(\"{s:#?}\"))` or \
                         `iced_mcp::attach(app).state(..)`"
                            .into(),
                    ),
                });
                Task::none()
            }
            Request::Actions => {
                answer(Outcome::Ok(Response::Actions(ActionList {
                    actions: self
                        .actions
                        .iter()
                        .map(|a| ActionInfo {
                            name: a.name.clone(),
                            description: a.description.clone(),
                            kind: ActionKind::Action,
                            output_schema: None,
                        })
                        .chain(self.queries.iter().map(|q| ActionInfo {
                            name: q.name.clone(),
                            description: q.description.clone(),
                            kind: ActionKind::Query,
                            output_schema: q.output_schema.clone(),
                        }))
                        .collect(),
                })));
                Task::none()
            }
            Request::Action(params) => {
                let Some(action) = self.actions.iter().find(|a| a.name == params.name) else {
                    let known: Vec<&str> = self.actions.iter().map(|a| a.name.as_str()).collect();
                    answer(Outcome::Error(format!(
                        "no action '{}'; the app offers {known:?}",
                        params.name
                    )));
                    return Task::none();
                };
                match (action.run)(&params.args) {
                    Ok(message) => {
                        answer(Outcome::Ok(Response::Done));
                        Task::done(app::<P>(message))
                    }
                    Err(error) => {
                        answer(Outcome::Error(format!("{}: {error}", params.name)));
                        Task::none()
                    }
                }
            }
            Request::Query(params) => {
                let Some(query) = self.queries.iter().find(|q| q.name == params.name) else {
                    let known: Vec<&str> = self.queries.iter().map(|q| q.name.as_str()).collect();
                    let hint = if self.actions.iter().any(|a| a.name == params.name) {
                        " (that is an action: use run_app_action)"
                    } else {
                        ""
                    };
                    answer(Outcome::Error(format!(
                        "no query '{}'{hint}; the app offers {known:?}",
                        params.name
                    )));
                    return Task::none();
                };
                answer(match (query.run)(&state.app, &params.args) {
                    Ok(value) => Outcome::Ok(Response::Value(QueryValue { value })),
                    Err(error) => Outcome::Error(format!("{}: {error}", params.name)),
                });
                Task::none()
            }
            Request::Clipboard(params) => {
                let read = move |text| Message(Inner::Clip(text, reply.clone()));
                match params.write {
                    Some(text) => iced::clipboard::write::<Message<P>>(text)
                        .chain(iced::clipboard::read().map(read)),
                    None => iced::clipboard::read().map(read),
                }
            }
            Request::Snapshot(params) => {
                let job = Job::Snapshot {
                    grid: params.grid.unwrap_or(0.0).max(0.0),
                    reply: reply.clone(),
                };
                self.enqueue(state, params.window.as_deref(), job, &reply);
                Task::none()
            }
            Request::Probe(params) => {
                let job = Job::Probe {
                    point: Point::new(params.x, params.y),
                    reply: reply.clone(),
                };
                self.enqueue(state, params.window.as_deref(), job, &reply);
                Task::none()
            }
            Request::Act(params) => {
                if params.frames.is_empty() {
                    answer(Outcome::Error("act needs at least one frame".into()));
                    return Task::none();
                }
                let job = Job::Act {
                    frames: params.frames.into(),
                    acted: Acted {
                        messages: 0,
                        captured: 0,
                        frames: 0,
                    },
                    reply: reply.clone(),
                };
                self.enqueue(state, params.window.as_deref(), job, &reply);
                Task::none()
            }
            Request::WidgetOperation(params) => {
                let job = Job::WidgetOperation {
                    id: params.id,
                    operation: params.operation,
                    reply: reply.clone(),
                };
                self.enqueue(state, params.window.as_deref(), job, &reply);
                Task::none()
            }
            Request::Screenshot(params) => match window_id(params.window.as_deref()) {
                Ok(id) => window::screenshot(id).map(move |shot| {
                    Message(Inner::Shot(Arc::new(shot), params.clone(), reply.clone()))
                }),
                Err(error) => {
                    answer(Outcome::Error(error));
                    Task::none()
                }
            },
            Request::Highlight(params) => {
                match window_id(params.window.as_deref()) {
                    Ok(id) => {
                        let duration = std::time::Duration::from_millis(
                            params.duration_ms.unwrap_or(1_500).clamp(100, 5_000),
                        );
                        self.registry.lock().slot(id).highlight = Some(registry::Highlight {
                            bounds: params.bounds,
                            expires_at: Instant::now() + duration,
                        });
                        answer(Outcome::Ok(Response::Done));
                    }
                    Err(error) => answer(Outcome::Error(error)),
                }
                Task::none()
            }
            Request::Window(params) => match window_id(params.window.as_deref()) {
                Ok(id) => {
                    answer(Outcome::Ok(Response::Done));
                    match params.action {
                        WindowAction::Resize { width, height } => {
                            window::resize(id, Size::new(width, height))
                        }
                        WindowAction::Move { x, y } => window::move_to(id, Point::new(x, y)),
                        WindowAction::Focus => window::gain_focus(id),
                        WindowAction::Maximize { on } => window::maximize(id, on),
                        WindowAction::Minimize { on } => window::minimize(id, on),
                        WindowAction::Close => window::close(id),
                    }
                }
                Err(error) => {
                    answer(Outcome::Error(error));
                    Task::none()
                }
            },
        }
    }

    /// Queues a job on its window; the window's inspector serves it on the
    /// redraw that follows this update.
    fn enqueue(&self, state: &mut State<P>, window: Option<&str>, job: Job, reply: &Replier) {
        let mut windows = state.registry.lock();
        match windows.find(window) {
            Some(slot) => slot.jobs.push_back(job),
            None => {
                let _ = reply.try_send(Outcome::Error(match window {
                    Some(name) => format!("no window '{name}'"),
                    None => "no window has rendered yet".into(),
                }));
            }
        }
    }
}

/// Requests from the socket, taken once by the subscription.
static INBOX: Mutex<Option<UnboundedReceiver<Command>>> = Mutex::new(None);

fn inbox() -> impl Stream<Item = Command> {
    let receiver = INBOX.lock().ok().and_then(|mut inbox| inbox.take());
    stream::iter(receiver).flatten()
}

/// The state text the request asked for: the lines matching `filter` (with
/// their numbers) or all of it, cut at `max_chars`.
fn state_dump(full: &str, filter: Option<&str>, max_chars: usize) -> StateDump {
    let total_chars = full.chars().count();
    let selected = match filter.map(str::to_lowercase) {
        None => full.to_owned(),
        Some(needle) => full
            .lines()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&needle))
            .map(|(n, line)| format!("{:>5}: {line}\n", n + 1))
            .collect(),
    };
    let truncated = selected.chars().count() > max_chars;
    let text = if truncated {
        selected.chars().take(max_chars).collect()
    } else {
        selected
    };
    StateDump {
        text,
        total_chars,
        truncated,
    }
}

/// Ticks every 100 ms while a highlight is shown. A plain thread rather than
/// `iced::time::every`, which exists only when the app enables iced's `tokio`
/// or `smol` executor: the bridge must work with any executor. The thread
/// ends at the first tick after the subscription (and so the receiver) drops.
fn highlight_ticks() -> futures::channel::mpsc::Receiver<()> {
    let (mut sender, receiver) = futures::channel::mpsc::channel(1);
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        match sender.try_send(()) {
            Err(error) if error.is_disconnected() => break,
            _ => {}
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dumps_filter_by_line_and_cut() {
        let full = "App {\n    theme: Dark,\n    symbol: \"BTCUSDT\",\n}";
        let dump = state_dump(full, Some("SYMBOL"), 1000);
        assert_eq!(dump.text, "    3:     symbol: \"BTCUSDT\",\n");
        assert!(!dump.truncated);
        let cut = state_dump(full, None, 5);
        assert_eq!(cut.text, "App {");
        assert!(cut.truncated);
        assert_eq!(cut.total_chars, full.chars().count());
    }
}
