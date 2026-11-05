//! Tool parameters: what each tool takes, with the descriptions agents see.
//!
//! Every tool that acts on an app takes `app` (which connected app; the
//! default — the last one connected — when absent) and most take `window`.

use iced_mcp_protocol::{MouseButton, Rect, ScrollUnit};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

/// What to act on. Give one of `text`, `id`, `ref`, or `x` + `y`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct Target {
    /// Visible text to match: case-insensitive substring unless `exact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) text: Option<String>,
    /// Match `text` exactly (case-sensitive, whole string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exact: Option<bool>,
    /// A widget id the app set (`Id::new("…")`), shown as `#id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    /// A `[ref]` from the latest `snapshot` of this window.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub(crate) reference: Option<usize>,
    /// Logical x, for canvases and custom-drawn content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) x: Option<f32>,
    /// Logical y.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) y: Option<f32>,
    /// Which match when several do; 0 is the topmost, most specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) nth: Option<usize>,
}

impl Target {
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_none() && self.id.is_none() && self.reference.is_none() && self.x.is_none()
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(crate) struct LaunchParams {
    /// A name for this app, to address it when several are connected
    /// (default: the app's own name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// The command line, e.g. "cargo run -p my-app" or "./target/debug/my-app".
    pub(crate) command: String,
    /// Arguments, if you prefer them separate from `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) args: Option<Vec<String>>,
    /// Working directory; the server's own when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    /// Extra environment variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) env: Option<HashMap<String, String>>,
    /// Seconds to wait for the first window (default 300: a cold `cargo run` builds first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_secs: Option<u64>,
    /// Start the app in this fixture (see `fixtures`), via ICED_MCP_FIXTURE.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fixture: Option<String>,
    /// The fixture's args, as its description says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fixture_args: Option<serde_json::Value>,
    /// Keep the window from taking keyboard focus from you (default true;
    /// macOS). false: it comes to the front like a normal launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) background: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ConnectParams {
    /// A name for this app, to address it when several are connected
    /// (default: the app's own name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// Bridge port (default 9339, or the app's ICED_MCP_PORT).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) port: Option<u16>,
    /// The app's ICED_MCP_TOKEN, if it set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(crate) struct DisconnectParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Also stop the app if this server launched it (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kill: Option<bool>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct SnapshotToolParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Window id from `status`; the first window when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Only nodes whose text or id contains this (case-insensitive), flat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) query: Option<String>,
    /// Include nodes scrolled or clipped out of view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) include_hidden: Option<bool>,
    /// Line cap (default 300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_lines: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub(crate) struct AccessibilityActionParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Numeric AccessKit node id from `accessibility_tree`.
    pub(crate) node_id: u64,
    /// Snapshot token returned with the node tree; prevents acting on stale ids.
    pub(crate) snapshot_id: String,
    /// click, focus, expand, collapse, scroll_up/down, replace_selected_text, set_value, increment, or decrement.
    pub(crate) action: String,
    /// Replacement text; a number for sliders; `true`/`false` for toggles; or an exact option label for selects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ClickParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(flatten)]
    pub(crate) target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// left (default), right or middle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) button: Option<MouseButton>,
    /// 2 for a double click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) count: Option<u8>,
    /// Modifiers held during the click, e.g. "Shift" or "Cmd+Shift".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) modifiers: Option<String>,
    /// Append a fresh snapshot to the reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct TypeParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// The text to type. "\n" presses Enter, "\t" Tab.
    pub(crate) text: String,
    /// Click this first to focus it (an input, by its placeholder or value text, id or ref).
    #[serde(flatten)]
    pub(crate) target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Select all (Cmd/Ctrl+A) before typing, replacing what is there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) clear: Option<bool>,
    /// Press Enter afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) submit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct KeyParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// A key or chord: "Enter", "Escape", "ArrowDown", "Ctrl+Shift+P", "Mod+S" (Cmd on macOS, Ctrl elsewhere).
    pub(crate) keys: String,
    /// Press it this many times (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) repeat: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ScrollParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Where to scroll; the window centre when no target is given.
    #[serde(flatten)]
    pub(crate) target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Positive scrolls down (content moves up).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dy: Option<f32>,
    /// Positive scrolls right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dx: Option<f32>,
    /// "lines" (default) or "pixels".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) unit: Option<ScrollUnit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct DragParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Where the drag starts.
    #[serde(flatten)]
    pub(crate) target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// End point, absolute logical x (or use dx).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) to_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) to_y: Option<f32>,
    /// End point relative to the start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dx: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dy: Option<f32>,
    /// Intermediate moves, one per frame (default 12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) button: Option<MouseButton>,
    /// Modifiers held during the drag, e.g. "Shift".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) modifiers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct HoverParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(flatten)]
    pub(crate) target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct HighlightToolParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    #[serde(flatten)]
    pub(crate) target: Target,
    /// How long the live outline stays visible (100–5000 ms; default 1500).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ScreenshotToolParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Crop to this node (with a small margin) instead of the whole window.
    #[serde(flatten)]
    pub(crate) target: Target,
    /// Or crop to a logical rectangle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) region: Option<Rect>,
    /// Draw an outline around `text`, `id`, `ref`, or x/y in the returned PNG.
    /// When set, the target is highlighted in context instead of cropped.
    #[serde(default)]
    pub(crate) highlight: bool,
    /// Image width cap in pixels (default 1280).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_width: Option<u32>,
    /// Write the PNG to this path instead of returning it inline (keeps the
    /// image out of the conversation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) save_to: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct WaitParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Text to wait for (case-insensitive substring unless `exact`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exact: Option<bool>,
    /// Or a widget id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    /// Wait for it to disappear instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) gone: Option<bool>,
    /// Milliseconds (default 10000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct MessagesToolParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Only messages after this sequence number (from an earlier reply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) since: Option<u64>,
    /// Case-insensitive substring filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) filter: Option<String>,
    /// How many of the newest to return (default 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct LogsParams {
    /// Only lines after this cursor (from an earlier reply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) since: Option<u64>,
    /// Character budget for the reply (default 8000); older lines drop first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_chars: Option<usize>,
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Lines from the end (default 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tail: Option<usize>,
    /// Case-insensitive substring filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) filter: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WindowOp {
    Resize,
    Move,
    Focus,
    Maximize,
    Unmaximize,
    Minimize,
    Restore,
    Close,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(crate) struct WindowToolParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    pub(crate) action: WindowOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// For resize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) height: Option<f32>,
    /// For move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) y: Option<f32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(crate) struct SequenceParams {
    /// Which connected app (its name from `status`); the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Steps run in order: an `action` and its `args`, that tool's
    /// parameters (sleep takes `ms`; expect, expect_pixel and
    /// expect_no_diagnostics are assertions).
    pub(crate) steps: Vec<Step>,
    /// Keep going after a failed step (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) continue_on_error: Option<bool>,
    /// Append a snapshot after the last step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snapshot: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ScriptEvalParams {
    /// The Luau script. Globals: `app` (the app below), `apps["name"]`
    /// (any connected app) and `print`. Methods mirror the steps:
    /// `app:click{ id = "save" }`, `app:wait_for("Saved")`,
    /// `app:expect{ id = "total", text_contains = "3" }`,
    /// `app:query("items")`, `app:find_all{ role = "text" }`… The whole
    /// typed API: `script_api`. `return` a value to get it back as JSON.
    pub(crate) source: String,
    /// Which connected app `app` means (its name from `status`); the default
    /// app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Wall-clock limit in ms (default 60000, at most 1800000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(crate) struct Step {
    #[schemars(schema_with = "crate::flow::action_schema")]
    pub(crate) action: String,
    #[serde(default)]
    pub(crate) args: serde_json::Value,
}

// --------------------------------------------------------------------- tools

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct WaitIdleParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// How long the screen must stay unchanged, in ms (default 300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stable_ms: Option<u64>,
    /// Give up after this many ms (default 5000); not settling is reported, not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct PasteParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Put this on the clipboard, then press Cmd/Ctrl+V in the focused widget.
    pub(crate) text: String,
    /// Click this first to focus it.
    #[serde(flatten)]
    pub(crate) target: Target,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ClipboardToolParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Replace the clipboard with this; read it when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) write: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct StateToolParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Only lines containing this (case-insensitive), numbered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) filter: Option<String>,
    /// Cut at this many characters (default 20000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_chars: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct PerfToolParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Start a fresh measurement after reporting, to measure one interaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reset: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct PointParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Logical x.
    pub(crate) x: f32,
    /// Logical y.
    pub(crate) y: f32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct AuditParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct DiffParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Baseline name (stored as `.iced-mcp/baselines/<name>.png`). Created
    /// from the current screen the first time.
    pub(crate) baseline: String,
    /// Overwrite the baseline with the current screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) update: Option<bool>,
    /// Compare only this node (by text/id/ref) or point area.
    #[serde(flatten)]
    pub(crate) target: Target,
    /// Or only this logical rectangle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) region: Option<Rect>,
    /// Fail (in flows) when more than this % of pixels differ (default 0.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_mismatch: Option<f64>,
    /// Per-pixel colour tolerance, 0–1 of the largest difference (default 0.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tolerance: Option<f32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct RecordStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Frames per second (default 6, at most 30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fps: Option<u32>,
    /// Stop on its own after this many seconds (default 120, at most 600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_seconds: Option<u64>,
    /// Frame width cap in pixels (default 1280).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_width: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct FlowStartParams {
    /// File name for `.iced-mcp/flows/<name>.json`: letters, digits, - and _.
    pub(crate) name: String,
    /// For a fragment (no launch step): what must be true before it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prerequisite: Option<String>,
    /// Record which app; the default app when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct FlowStepParams {
    /// The step's action, as in run_sequence.
    #[schemars(schema_with = "crate::flow::action_schema")]
    pub(crate) action: String,
    /// That action's parameters, as its tool takes them.
    #[serde(default)]
    pub(crate) args: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct FlowRunParams {
    /// The flow to run; empty lists the saved flows.
    #[serde(default)]
    pub(crate) name: String,
    /// Run on this app (fragments); an end-to-end flow launches its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// Rewrite every screenshot_diff baseline from this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) update_baselines: Option<bool>,
    /// Confirm the fragment's prerequisite holds (required when it has one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prerequisite_ok: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ExpectMessageParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// A message's Debug text must contain this (case-insensitive).
    pub(crate) contains: String,
    /// How long to wait for it, in ms (default 3000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_ms: Option<u64>,
    /// Count only messages after this sequence number (default: the newest
    /// when the step starts, so earlier traffic cannot satisfy it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) since: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct AppActionParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// The action's name, from `app_actions`.
    pub(crate) name: String,
    /// Its arguments, as its description says.
    #[serde(default)]
    pub(crate) args: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct AppOnly {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct SleepParams {
    /// Milliseconds (at most 30000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct EchoParams {
    /// A note to print in the run report.
    pub(crate) message: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct QueryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// The query's name, from `app_actions` (kind: query).
    pub(crate) name: String,
    /// Its arguments, as its description says.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub(crate) args: serde_json::Value,
    /// Cut the text form of the answer at this many characters (default 8000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_chars: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ExpectQueryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub(crate) args: serde_json::Value,
    /// The answer's JSON text must contain this (case-insensitive).
    pub(crate) contains: String,
    /// Keep asking until it does, up to this many ms (default 3000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct DiagnoseParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Only these kinds: overlap, clipped, text_overflow, duplicate_id
    /// (default: all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kinds: Option<Vec<crate::diagnose::DiagnosticKind>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct FixtureParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    /// The fixture's name, from `fixtures`.
    pub(crate) name: String,
    /// Its arguments, as its description says.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub(crate) args: serde_json::Value,
}

/// A structural assertion on one target. Give the target (`id`, `text`,
/// `ref`, or `x`/`y`; `nth` for another match) and any of the checks; with
/// none, it checks that the target exists.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ExpectParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    #[serde(flatten)]
    pub(crate) target: Target,
    /// It is in the widget tree (visible or scrolled away), or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exists: Option<bool>,
    /// It is on screen (not scrolled or clipped out), or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) visible: Option<bool>,
    /// Enabled: its reported state, else whether it shows an interactive cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
    /// It has keyboard focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) focused: Option<bool>,
    /// Its text is exactly this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) text_equals: Option<String>,
    /// Its text contains this (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) text_contains: Option<String>,
    /// Its value is exactly this: a semantic node's value, a slider's number,
    /// an input's text (its placeholder when empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<String>,
    /// Its text (or all text inside it) fits its widget, as measured by the bridge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fits: Option<bool>,
    /// It has this many children, as `snapshot` lists them (hidden included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) children: Option<usize>,
    /// This many visible nodes match the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) count: Option<usize>,
    /// Its right edge is at or left of the other's left edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) left_of: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) right_of: Option<Target>,
    /// Its bottom edge is at or above the other's top edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) above: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) below: Option<Target>,
    /// Its box lies within the other's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) inside: Option<Target>,
    /// Vertical centres line up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) same_row: Option<Target>,
    /// Horizontal centres line up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) same_column: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) aligned_left: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) aligned_right: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) aligned_top: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) aligned_bottom: Option<Target>,
    /// Slack for the relations, in logical px (default 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tolerance: Option<f32>,
    /// Keep checking until every check holds, up to this many ms (default
    /// 2000; 0 checks once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_ms: Option<u64>,
}

impl ExpectParams {
    /// The geometric relations asked for, by name.
    pub(crate) fn relations(&self) -> impl Iterator<Item = (&'static str, &Target)> {
        [
            ("left_of", &self.left_of),
            ("right_of", &self.right_of),
            ("above", &self.above),
            ("below", &self.below),
            ("inside", &self.inside),
            ("same_row", &self.same_row),
            ("same_column", &self.same_column),
            ("aligned_left", &self.aligned_left),
            ("aligned_right", &self.aligned_right),
            ("aligned_top", &self.aligned_top),
            ("aligned_bottom", &self.aligned_bottom),
        ]
        .into_iter()
        .filter_map(|(name, target)| target.as_ref().map(|t| (name, t)))
    }
}

/// The colour of one pixel of a window screenshot.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub(crate) struct ExpectPixelParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<String>,
    /// Logical x.
    pub(crate) x: f32,
    /// Logical y.
    pub(crate) y: f32,
    /// Expected colour: "#rgb", "#rrggbb" or "#rrggbbaa".
    pub(crate) color: String,
    /// Largest difference allowed per channel, 0–255 (default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tolerance: Option<u8>,
    /// Keep checking until it matches, up to this many ms (default 2000; 0 checks once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_ms: Option<u64>,
}
