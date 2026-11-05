//! The wire protocol between a running app's bridge and the MCP server.
//!
//! One TCP connection on loopback carries newline-delimited JSON: the server
//! writes a [`Request`] per line and reads one [`Response`] per line, matched
//! by `id`. Nothing here depends on iced, so a bridge for another toolkit
//! (egui, Slint, a custom renderer) can speak the same protocol and reuse the
//! server unchanged.
//!
//! Coordinates are logical pixels with the origin at the window's top-left,
//! the same space the toolkit lays widgets out in.

use serde::{Deserialize, Serialize};

/// Bumped on any breaking change to the messages below.
pub const PROTOCOL_VERSION: u32 = 6;

/// The port a bridge listens on when nothing else is configured.
pub const DEFAULT_PORT: u16 = 9339;

/// Enables the bridge in an app that wires it in conditionally.
pub const ENV_ENABLE: &str = "ICED_MCP";
/// Overrides [`DEFAULT_PORT`].
pub const ENV_PORT: &str = "ICED_MCP_PORT";
/// When set, the first request on a connection must be `hello` with this token.
pub const ENV_TOKEN: &str = "ICED_MCP_TOKEN";
/// Names a fixture the bridge loads as soon as the app has booted.
pub const ENV_FIXTURE: &str = "ICED_MCP_FIXTURE";
/// The JSON `args` for [`ENV_FIXTURE`] (default `null`).
pub const ENV_FIXTURE_ARGS: &str = "ICED_MCP_FIXTURE_ARGS";
/// `1`: keep the app from taking keyboard focus (where the platform lets an
/// app do that for itself).
pub const ENV_BACKGROUND: &str = "ICED_MCP_BACKGROUND";

/// A request line: `{"id":1,"method":"snapshot","params":{...}}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Handshake: app name, protocol version, windows.
    Hello(Hello),
    /// The widget tree of one window.
    Snapshot(SnapshotParams),
    /// Synthetic input, frame by frame.
    Act(ActParams),
    /// What the pointer would show at a point: the hit-test for canvases and
    /// custom widgets that report nothing to the widget tree.
    Probe(ProbeParams),
    /// A PNG of one window.
    Screenshot(ScreenshotParams),
    /// Temporarily outline a logical window rectangle in the live app.
    Highlight(HighlightParams),
    /// Resize, move, focus… a window.
    Window(WindowParams),
    /// The app's recent messages (their `Debug` form), oldest first.
    Messages(MessagesParams),
    /// Timings of the app's `update` and `view` and of the window's frames.
    Perf(PerfParams),
    /// The app's state as text, when the app opted in to sharing it.
    State(StateParams),
    /// Read or replace the system clipboard through the app.
    Clipboard(ClipboardParams),
    /// The actions the app registered for agents.
    Actions,
    /// Run one of them.
    Action(ActionParams),
    /// Run one of the app's read-only queries; the answer is JSON.
    Query(ActionParams),
    /// Put the app in one of its named states (fixtures).
    Fixture(ActionParams),
    /// Perform a direct operation on an iced widget in one window.
    WidgetOperation(WidgetOperationParams),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetOperationParams {
    pub window: Option<String>,
    pub id: String,
    pub operation: WidgetOperationKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetOperationKind {
    Focus,
    SelectAll,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionParams {
    pub name: String,
    /// The action's arguments, as its description documents them.
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerfParams {
    /// Start a fresh measurement after reporting.
    #[serde(default)]
    pub reset: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateParams {
    /// Only lines containing this (case-insensitive), each with its line
    /// number, instead of the whole dump.
    #[serde(default)]
    pub filter: Option<String>,
    /// Cut the text at this many characters (the bridge's default otherwise).
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClipboardParams {
    /// Replace the clipboard with this; read it when absent.
    #[serde(default)]
    pub write: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hello {
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotParams {
    /// Window id from [`AppInfo::windows`]; the first open window when absent.
    #[serde(default)]
    pub window: Option<String>,
    /// Hit-test a grid of this many logical pixels for interactive areas no
    /// widget reported ([`NodeKind::Region`]); `0` or absent skips it.
    #[serde(default)]
    pub grid: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActParams {
    #[serde(default)]
    pub window: Option<String>,
    /// Each inner list is delivered in one frame, so the app processes its
    /// messages and re-lays out before the next one — the way a click that
    /// opens a menu has to land before the click on the menu item.
    pub frames: Vec<Vec<Input>>,
}

/// One synthetic input event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Input {
    /// Moves the synthetic cursor. Later mouse events land here.
    Move {
        x: f32,
        y: f32,
    },
    Press {
        button: MouseButton,
    },
    Release {
        button: MouseButton,
    },
    /// Wheel scroll. Positive `y` scrolls content up (wheel away from you),
    /// matching the toolkit's sign.
    Scroll {
        x: f32,
        y: f32,
        unit: ScrollUnit,
    },
    /// A key going down. `text` is what it types, if anything.
    KeyDown {
        key: String,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        modifiers: Modifiers,
    },
    KeyUp {
        key: String,
        #[serde(default)]
        modifiers: Modifiers,
    },
    /// The held modifiers changed.
    Modifiers {
        modifiers: Modifiers,
    },
    /// The pointer left the window.
    Leave,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScrollUnit {
    #[default]
    Lines,
    Pixels,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Modifiers {
    #[serde(default, skip_serializing_if = "is_false")]
    pub shift: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ctrl: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub alt: bool,
    /// Command on macOS, the Windows key elsewhere.
    #[serde(default, skip_serializing_if = "is_false")]
    pub logo: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeParams {
    #[serde(default)]
    pub window: Option<String>,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScreenshotParams {
    #[serde(default)]
    pub window: Option<String>,
    /// Crop to this logical rectangle before scaling.
    #[serde(default)]
    pub region: Option<Rect>,
    /// Downscale so the image is at most this many pixels wide.
    #[serde(default)]
    pub max_width: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightParams {
    #[serde(default)]
    pub window: Option<String>,
    pub bounds: Rect,
    /// The outline disappears after this many milliseconds (capped by the bridge).
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowParams {
    #[serde(default)]
    pub window: Option<String>,
    pub action: WindowAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WindowAction {
    Resize {
        width: f32,
        height: f32,
    },
    Move {
        x: f32,
        y: f32,
    },
    Focus,
    Maximize {
        on: bool,
    },
    Minimize {
        on: bool,
    },
    /// Asks the window to close, as its close button would.
    Close,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessagesParams {
    /// Only messages with a larger sequence number.
    #[serde(default)]
    pub since: Option<u64>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Case-insensitive substring filter on the `Debug` text.
    #[serde(default)]
    pub filter: Option<String>,
}

/// A response line: `{"id":1,"ok":{...}}` or `{"id":1,"error":"..."}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Response),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Hello(AppInfo),
    Snapshot(Snapshot),
    Acted(Acted),
    Probe(Probe),
    Screenshot(Screenshot),
    Done,
    Messages(MessageLog),
    Perf(PerfReport),
    State(StateDump),
    Clipboard(ClipboardContent),
    Actions(ActionList),
    /// A query's answer.
    Value(QueryValue),
    /// Whether a direct operation found the target widget.
    WidgetOperation {
        found: bool,
    },
}

/// The actions an app offers. (A struct, not a bare list: the response enum
/// is internally tagged, which cannot carry a sequence directly.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionList {
    pub actions: Vec<ActionInfo>,
}

/// A named operation the app offers agents — the way to reach what has no
/// widget (open a scene, load a fixture, switch a feature flag).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionInfo {
    pub name: String,
    /// What it does and the `args` it takes.
    pub description: String,
    #[serde(default)]
    pub kind: ActionKind,
    /// For a query: the JSON Schema of its answer, when the app gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Changes the app: turns into a message that goes through `update`.
    #[default]
    Action,
    /// Reads the app's state and answers with JSON; changes nothing.
    Query,
    /// Puts the app in a named state: a known starting point for a test.
    Fixture,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryValue {
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfReport {
    /// Milliseconds the measurement covers.
    pub span_ms: u64,
    /// Rendered frames, from the windows' redraws.
    pub frames: Timing,
    /// The app's `view` calls (a rebuild of one window's widget tree).
    pub view: Timing,
    /// The app's `update`, all messages together.
    pub update: Timing,
    /// The costliest message kinds by total `update` time, worst first.
    pub slowest: Vec<KindTiming>,
}

/// Durations in microseconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timing {
    pub count: u64,
    pub mean_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindTiming {
    /// The message's variant path, e.g. `Chart(Book`.
    pub kind: String,
    pub count: u64,
    pub total_us: u64,
    pub max_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateDump {
    pub text: String,
    /// Characters in the full dump, before filtering or cutting.
    pub total_chars: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardContent {
    /// What the clipboard holds (or now holds, after a write); `None` when
    /// it is empty or not text.
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub toolkit: String,
    pub protocol: u32,
    pub pid: u32,
    pub windows: Vec<WindowInfo>,
    /// Whether `messages` records anything.
    pub messages: bool,
    /// The fixture loaded last (at start-up from [`ENV_FIXTURE`], or by a
    /// `fixture` request).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture: Option<String>,
    /// Why the start-up fixture could not be loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture_error: Option<String>,
    /// Whether the app hands keyboard focus back when it is activated
    /// without a click ([`ENV_BACKGROUND`], macOS only).
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub size: Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub fn area(&self) -> f32 {
        self.width.max(0.0) * self.height.max(0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub window: String,
    pub size: Size,
    /// Pre-order: a node's children follow it, and `parent` points back.
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Index in [`Snapshot::nodes`]; only meaningful within one snapshot.
    pub index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    pub kind: NodeKind,
    pub layer: Layer,
    /// The widget id the app gave it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Text content; for an input, its value or placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub bounds: Rect,
    /// On-screen part, after scrolling and clipping. `None` when hidden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    /// The cursor the app shows over the node's visible centre (`pointer`,
    /// `text`, `grab`…): a hint for what is clickable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll: Option<ScrollInfo>,
    /// For a [`NodeKind::Semantic`] node: what it is (`bar`, `drawing`,
    /// `handle`, `axis`, `cell`…). Its name is in `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// For a semantic node: its value (`O 71856 H … C …`), as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Accessible name, distinct from visible text and editable value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Additional accessible description supplied by the widget author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Placeholder shown when a text control has no value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Explicit control state. `None` means the widget did not report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indeterminate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    /// Currently enabled values for a select-like control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
    /// Current numeric value for controls such as sliders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_value: Option<f64>,
    /// Inclusive minimum for a numeric control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_numeric_value: Option<f64>,
    /// Inclusive maximum for a numeric control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_numeric_value: Option<f64>,
    /// Smallest increment accepted by a numeric control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_value_step: Option<f64>,
    /// Full track bounds for setting a slider value; `bounds` remains the thumb target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_bounds: Option<Rect>,
    /// Whether the slider track runs vertically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical: Option<bool>,
    /// Whether visual direction is reversed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverted: Option<bool>,
    /// For a text node: the size its text needs and the space it was given,
    /// when the bridge could measure it. `None` means unknown, not "fits".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_fit: Option<TextFit>,
}

/// How a text's laid-out size compares with the space its widget had.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextFit {
    /// The smallest box that holds the text as shaped and wrapped.
    pub needed: Size,
    /// The width it could take; `None` when unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_width: Option<f32>,
    /// The height it could take; `None` when unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_height: Option<f32>,
}

impl TextFit {
    /// Half a pixel of slack: shaping rounds.
    const SLACK: f32 = 0.5;

    /// Whether the text fits the space it was given.
    pub fn fits(&self) -> bool {
        let within = |needed: f32, available: Option<f32>| {
            available.is_none_or(|available| needed <= available + Self::SLACK)
        };
        within(self.needed.width, self.available_width)
            && within(self.needed.height, self.available_height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Container,
    Scrollable,
    Focusable,
    TextInput,
    Text,
    Custom,
    /// No widget reported it, but the app shows this cursor over it: a
    /// clickable area of a widget that does not take part in operations
    /// (a dropdown, slider, canvas button, custom widget). Found by
    /// hit-testing a grid, so its edges are approximate.
    Region,
    /// The accessible name of a widget that shows no text of its own, such
    /// as an icon-only button. A widget reports one by calling
    /// `operation.custom(id, bounds, &mut label)` with a `String` state; see
    /// [`LABEL_CONVENTION`].
    Label,
    /// Something a custom-drawn widget (a chart, a heatmap, a 3D view) drew
    /// and described: a bar, a drawing, a handle, an axis. See
    /// [`SEMANTICS_MARKER`].
    Semantic,
}

/// How a custom-drawn widget describes what it drew, with no dependency on
/// iced-mcp: in its `operate`, call
/// `operation.custom(id, bounds, &mut (SEMANTICS_MARKER, json))` where the
/// state is a `(&'static str, String)` and `json` is one [`SemanticNode`] or
/// an array of them. Other operations ignore it. Describe what is visible,
/// not a whole data set.
///
/// The `@1` is the payload format's version; its JSON Schema is
/// `schema/semantics.v1.json`. A bridge reports a version it does not know as
/// an error node rather than misreading it.
pub const SEMANTICS_MARKER: &str = "iced-mcp/semantics@1";

/// The marker without a version, read as version 1.
pub const SEMANTICS_MARKER_V1_UNVERSIONED: &str = "iced-mcp/semantics";

/// The payload format version a marker names: `Some(1)` for
/// `iced-mcp/semantics@1` (or the bare tag), `None` for any other state.
pub fn semantics_version(marker: &str) -> Option<u32> {
    if marker == SEMANTICS_MARKER_V1_UNVERSIONED {
        return Some(1);
    }
    marker
        .strip_prefix("iced-mcp/semantics@")
        .and_then(|v| v.parse().ok())
}

/// Roles agents understand. Use an ARIA/AccessKit role where one fits; add a
/// domain role (a trading chart's `bar`, `drawing`…) where none does. Any
/// other role passes through unchanged, so apps are never blocked by this
/// list — it only keeps names consistent across apps.
pub const ROLES: &[(&str, &str)] = &[
    // ARIA / AccessKit
    ("button", "something that acts when clicked"),
    ("checkbox", "an on/off choice"),
    ("combobox", "a field that opens a list of choices"),
    (
        "date",
        "a calendar date input; set_value accepts YYYY-MM-DD",
    ),
    ("calendar", "a calendar with visible month and date targets"),
    (
        "time",
        "a segmented time field; set_value accepts HH:MM[:SS] or hh:mm[:ss] AM/PM",
    ),
    ("switch", "an on/off switch"),
    ("radio", "one choice from a set"),
    ("tab", "a selectable page"),
    ("tablist", "a set of selectable pages"),
    ("tabpanel", "the content for a tab"),
    ("slider", "a value set by dragging"),
    ("cell", "one cell of a grid or table"),
    ("row", "a row of a grid or table"),
    ("grid", "a two-dimensional set of cells"),
    ("img", "a picture or icon with meaning"),
    ("list", "a list"),
    ("listitem", "one item of a list"),
    ("group", "a set of related nodes"),
    ("tooltip", "a transient description"),
    // Charts and plots
    ("chart", "a whole chart or plot area"),
    ("series", "one data series"),
    ("bar", "one bar or candle (value: OHLC or the bar's value)"),
    ("point", "one data point"),
    ("line", "a line drawn on the chart"),
    ("axis", "an axis (value: its visible range)"),
    ("tick", "an axis label"),
    ("legend", "a legend entry"),
    ("drawing", "a user drawing: trend line, rectangle, fib…"),
    ("handle", "a draggable point of a drawing or shape"),
    ("crosshair", "the crosshair or hovered position"),
    ("profile", "a volume or market profile"),
    ("heatmap", "a heatmap area"),
    ("plot3d", "a 3D plot"),
    ("plane", "a plane or surface in a 3D plot"),
];

/// One thing a custom-drawn widget shows. Serialized as
/// `{"role":"bar","name":"BTCUSDT M1 12:45","value":"O … C …","id":"bar-…","bounds":{…},"children":[…]}`;
/// `value`, `id` and `children` are optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SemanticNode {
    pub role: String,
    pub name: String,
    /// Text, a number or any JSON; shown as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// A longer explanation than the accessible name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Placeholder for empty text controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Control states are explicit; omitted values are unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// Tri-state checkbox status when checked cannot be represented as bool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indeterminate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    /// Currently enabled values for a select-like control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<String>>,
    /// Current numeric value for controls such as sliders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_value: Option<f64>,
    /// Inclusive minimum for a numeric control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_numeric_value: Option<f64>,
    /// Inclusive maximum for a numeric control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_numeric_value: Option<f64>,
    /// Smallest increment accepted by a numeric control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_value_step: Option<f64>,
    /// Full track bounds for setting a slider value; `bounds` remains the thumb target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_bounds: Option<Rect>,
    /// Whether the slider track runs vertically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical: Option<bool>,
    /// Whether visual direction is reversed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inverted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Logical pixels in window coordinates.
    pub bounds: Rect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SemanticNode>,
}

impl SemanticNode {
    /// The state to pass to `operation.custom` for these nodes (current
    /// version).
    pub fn payload(nodes: &[SemanticNode]) -> (&'static str, String) {
        (
            SEMANTICS_MARKER,
            serde_json::to_string(nodes).unwrap_or_else(|_| "[]".to_owned()),
        )
    }

    /// Parses a payload's JSON: one node or an array of them.
    pub fn parse(json: &str) -> Result<Vec<SemanticNode>, String> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
        // A root may carry its own version as `"v"` (it is not a node field).
        if let Some(v) = value.get("v").and_then(serde_json::Value::as_u64) {
            if v != 1 {
                return Err(format!(
                    "semantics version {v} is newer than this bridge (it reads 1): update iced-mcp"
                ));
            }
        }
        let nodes = match value {
            serde_json::Value::Array(_) => serde_json::from_value(value),
            single => serde_json::from_value(single).map(|node| vec![node]),
        };
        nodes.map_err(|e| format!("not semantic nodes: {e}"))
    }
}

/// How an app names an icon-only widget for agents, with no dependency on
/// iced-mcp: in the widget's `operate`, call
/// `operation.custom(None, layout.bounds(), &mut label)` where `label` is a
/// `String`. Other operations ignore it; the bridge reads it as a
/// [`NodeKind::Label`] that `text` targets match.
pub const LABEL_CONVENTION: &str = "operation.custom(None, bounds, &mut String)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// The window's main content.
    Base,
    /// Menus, dropdowns, tooltips, dialogs drawn above it.
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScrollInfo {
    pub offset_x: f32,
    pub offset_y: f32,
    pub content_width: f32,
    pub content_height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acted {
    /// Messages the app published while handling the input.
    pub messages: usize,
    /// Events a widget captured (consumed).
    pub captured: usize,
    /// Frames delivered.
    pub frames: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub x: f32,
    pub y: f32,
    pub interaction: String,
    pub layer: Layer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screenshot {
    pub png_base64: String,
    /// Pixel size of the PNG.
    pub width: u32,
    pub height: u32,
    /// Logical pixels per PNG pixel: multiply a PNG coordinate by this (and
    /// add the region's origin) to get a coordinate for `act`.
    pub logical_per_pixel: f32,
    /// The logical rectangle the image shows.
    pub region: Rect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageLog {
    pub entries: Vec<MessageEntry>,
    /// Sequence number of the newest message recorded so far.
    pub last: u64,
    /// Messages dropped from the ring buffer before `entries[0]`.
    pub dropped: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEntry {
    pub seq: u64,
    /// Milliseconds since the bridge started.
    pub at_ms: u64,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_as_one_json_line() {
        let line = r#"{"id":7,"method":"act","params":{"frames":[[{"type":"move","x":1.0,"y":2.0}],[{"type":"press","button":"left"}]]}}"#;
        let envelope: Envelope = serde_json::from_str(line).unwrap();
        assert_eq!(envelope.id, 7);
        let Request::Act(act) = &envelope.request else {
            panic!("expected act");
        };
        assert_eq!(act.frames.len(), 2);
        let back = serde_json::to_string(&envelope).unwrap();
        assert!(!back.contains('\n'));
    }

    #[test]
    fn unit_requests_need_no_params() {
        let envelope: Envelope =
            serde_json::from_str(r#"{"id":1,"method":"snapshot","params":{}}"#).unwrap();
        assert!(matches!(envelope.request, Request::Snapshot(_)));
    }

    #[test]
    fn every_response_serializes() {
        let actions = Response::Actions(ActionList {
            actions: vec![ActionInfo {
                name: "a".into(),
                description: "b".into(),
                kind: ActionKind::Query,
                output_schema: None,
            }],
        });
        let line = serde_json::to_string(&Reply {
            id: 1,
            outcome: Outcome::Ok(actions),
        })
        .unwrap();
        let back: Reply = serde_json::from_str(&line).unwrap();
        assert!(
            matches!(back.outcome, Outcome::Ok(Response::Actions(list)) if list.actions.len() == 1)
        );
    }

    #[test]
    fn semantics_parse_one_node_or_many() {
        let one = r#"{"role":"bar","name":"12:45","value":"O 1 C 2","bounds":{"x":1,"y":2,"width":3,"height":4}}"#;
        let nodes = SemanticNode::parse(one).unwrap();
        assert_eq!(nodes[0].role, "bar");
        let (marker, json) = SemanticNode::payload(&nodes);
        assert_eq!(marker, SEMANTICS_MARKER);
        assert_eq!(SemanticNode::parse(&json).unwrap(), nodes);
        assert!(SemanticNode::parse(r#"{"role":"bar"}"#).is_err());
        // A versioned root, as an app with its own schema sends it.
        let versioned = r#"{"v":1,"role":"chart","name":"BTCUSDT","bounds":{"x":0,"y":0,"width":9,"height":9}}"#;
        assert_eq!(SemanticNode::parse(versioned).unwrap()[0].role, "chart");
        assert!(SemanticNode::parse(&versioned.replace("\"v\":1", "\"v\":2")).is_err());
    }

    #[test]
    fn markers_carry_a_version() {
        assert_eq!(semantics_version(SEMANTICS_MARKER), Some(1));
        assert_eq!(semantics_version("iced-mcp/semantics"), Some(1));
        assert_eq!(semantics_version("iced-mcp/semantics@2"), Some(2));
        assert_eq!(semantics_version("something-else"), None);
    }

    /// `schema/semantics.v1.json` is the published contract for widget
    /// authors. Run with `ICED_MCP_UPDATE_SCHEMA=1` to regenerate it after a
    /// deliberate change (and bump the version if it is not compatible).
    #[cfg(feature = "schemars")]
    #[test]
    fn semantics_schema_is_committed_and_current() {
        let schema = schemars::schema_for!(Vec<SemanticNode>);
        let generated = serde_json::to_string_pretty(&schema).unwrap() + "\n";
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/schema/semantics.v1.json");
        if std::env::var_os("ICED_MCP_UPDATE_SCHEMA").is_some() {
            std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/schema")).unwrap();
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path)
            .expect("schema/semantics.v1.json missing: run with ICED_MCP_UPDATE_SCHEMA=1");
        assert_eq!(
            committed, generated,
            "schema changed: regenerate with ICED_MCP_UPDATE_SCHEMA=1"
        );
    }

    #[test]
    fn text_fit_compares_needed_with_available_space() {
        let fit = |w, h, aw, ah| TextFit {
            needed: Size {
                width: w,
                height: h,
            },
            available_width: aw,
            available_height: ah,
        };
        assert!(fit(100.0, 20.0, None, None).fits());
        assert!(fit(100.4, 20.0, Some(100.0), None).fits(), "rounding slack");
        assert!(!fit(140.0, 20.0, Some(100.0), None).fits());
        assert!(!fit(80.0, 42.0, Some(100.0), Some(21.0)).fits());
        let json = serde_json::to_string(&fit(1.0, 2.0, Some(3.0), None)).unwrap();
        assert_eq!(
            json,
            r#"{"needed":{"width":1.0,"height":2.0},"available_width":3.0}"#
        );
    }

    #[test]
    fn reply_carries_ok_or_error() {
        let ok = Reply {
            id: 3,
            outcome: Outcome::Ok(Response::Done),
        };
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            r#"{"id":3,"ok":{"kind":"done"}}"#
        );
        let err: Reply = serde_json::from_str(r#"{"id":4,"error":"no window"}"#).unwrap();
        assert!(matches!(err.outcome, Outcome::Error(ref e) if e == "no window"));
    }
}
