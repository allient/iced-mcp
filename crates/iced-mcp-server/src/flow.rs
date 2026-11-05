//! Flows: a recorded path through an app, replayed as a regression test.
//!
//! A flow is a JSON file under `.iced-mcp/flows/<name>.json` in the project:
//!
//! ```json
//! {
//!   "prerequisite": "A chart widget is open",
//!   "launch": { "command": "cargo run -p my-app --features agent", "cwd": ".", "env": {} },
//!   "steps": [
//!     { "action": "click", "args": { "text": "Add" } },
//!     { "action": "wait_for", "args": { "text": "Saved" } },
//!     { "action": "expect_message", "args": { "contains": "Saved(Ok" } },
//!     { "action": "screenshot_diff", "args": { "baseline": "after-add" } }
//!   ]
//! }
//! ```
//!
//! Steps are the server's own actions with the arguments the tools take, so
//! a recorded session replays exactly. A flow with `launch` starts the app
//! itself (end to end); without, it is a fragment that runs on whatever app
//! is connected and states its `prerequisite`. As in Argent, the first
//! failing step stops the run and the rest are reported as skipped.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(
    title = "iced-mcp flow",
    description = "A recorded path through an iced app, replayed as a regression test by `flow_run` / `iced-mcp run-flow`."
)]
pub(crate) struct Flow {
    /// Editors validate against this; `schema/flow.v1.json` in iced-mcp-server.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub(crate) schema: Option<String>,
    /// What must be true before a fragment runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prerequisite: Option<String>,
    /// How to start the app, for an end-to-end flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) launch: Option<FlowLaunch>,
    /// The fixture (a named app state, see `fixtures`) loaded before the
    /// first step: an end-to-end flow starts the app in it, a fragment
    /// loads it into the connected app. It makes the flow independent of
    /// whatever ran before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fixture: Option<String>,
    /// The fixture's args.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fixture_args: Option<serde_json::Value>,
    pub(crate) steps: Vec<FlowStep>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct FlowLaunch {
    pub(crate) command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(crate) env: HashMap<String, String>,
    /// Keep the app from taking keyboard focus (default true; macOS — see
    /// the guide). `false` lets it come to the front like a normal launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) background: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct FlowStep {
    /// One of [`STEP_ACTIONS`]; `args` are that tool's parameters.
    pub(crate) action: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub(crate) args: serde_json::Value,
}

/// The actions a flow (and `run_sequence`) may contain.
pub(crate) const STEP_ACTIONS: &[&str] = &[
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
    "load_fixture",
    "expect_message",
    "query",
    "expect_query",
    "screenshot_diff",
    "expect_no_diagnostics",
    "echo",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepStatus {
    Pass,
    /// The app did not do what the step expected.
    Fail,
    /// The step could not run (bad arguments, lost connection).
    Error,
    Skip,
}

pub(crate) struct StepReport {
    pub(crate) index: usize,
    pub(crate) label: String,
    pub(crate) status: StepStatus,
    pub(crate) detail: String,
    pub(crate) duration: Duration,
}

/// Where flows live: `<root>/.iced-mcp/flows`.
pub(crate) fn dir(root: &Path) -> PathBuf {
    root.join(".iced-mcp").join("flows")
}

/// A flow name is a file name: letters, digits, `-` and `_`.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 80
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "flow name `{name}` must be 1–80 letters, digits, '-' or '_'"
        ))
    }
}

pub(crate) fn path(root: &Path, name: &str) -> Result<PathBuf, String> {
    check_name(name)?;
    Ok(dir(root).join(format!("{name}.json")))
}

pub(crate) fn load(root: &Path, name: &str) -> Result<Flow, String> {
    let path = path(root, name)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not a valid flow: {e}", path.display()))
}

pub(crate) fn save(root: &Path, name: &str, flow: &Flow) -> Result<PathBuf, String> {
    let path = path(root, name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(flow).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n")
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

pub(crate) fn list(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == "json").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();
    names
}

/// A short label for a step: `click "Add"`, `press_key Mod+S`.
pub(crate) fn label(step: &FlowStep) -> String {
    let args = &step.args;
    let pick = |key: &str| args.get(key).and_then(serde_json::Value::as_str);
    let detail = pick("text")
        .map(|t| format!("{t:?}"))
        .or_else(|| pick("id").map(|i| format!("#{i}")))
        .or_else(|| pick("keys").map(str::to_owned))
        .or_else(|| pick("name").map(str::to_owned))
        .or_else(|| pick("baseline").map(str::to_owned))
        .or_else(|| pick("contains").map(|t| format!("{t:?}")))
        .or_else(|| pick("message").map(|t| format!("{t:?}")))
        .or_else(|| {
            let x = args.get("x")?.as_f64()?;
            let y = args.get("y")?.as_f64()?;
            Some(format!("({x:.0},{y:.0})"))
        });
    match detail {
        Some(detail) => format!("{} {detail}", step.action),
        None => step.action.clone(),
    }
}

/// `[3] ✗ click "Save" (0.4s) — nothing visible matches…` lines and a
/// `PASS|FAIL — …` total, as Argent renders a run.
pub(crate) fn render(name: &str, reports: &[StepReport], total: Duration) -> String {
    let count = |s: StepStatus| reports.iter().filter(|r| r.status == s).count();
    let (passed, failed, errored, skipped) = (
        count(StepStatus::Pass),
        count(StepStatus::Fail),
        count(StepStatus::Error),
        count(StepStatus::Skip),
    );
    let mut out = format!("Flow {name}\n");
    for report in reports {
        let glyph = match report.status {
            StepStatus::Pass => "✓",
            StepStatus::Fail => "✗",
            StepStatus::Error => "!",
            StepStatus::Skip => "-",
        };
        let _ = write!(out, "[{}] {glyph} {}", report.index + 1, report.label);
        if report.status != StepStatus::Skip {
            let _ = write!(out, " ({:.1}s)", report.duration.as_secs_f32());
        }
        if !report.detail.is_empty() {
            let detail: String = report
                .detail
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(240)
                .collect();
            let _ = write!(out, " — {detail}");
        }
        out.push('\n');
    }
    let verdict = if failed + errored == 0 {
        "PASS"
    } else {
        "FAIL"
    };
    let _ = write!(
        out,
        "{verdict} — {passed} passed, {failed} failed, {errored} errored, {skipped} skipped ({:.1}s)",
        total.as_secs_f32()
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `schema/flow.v1.json` documents the flow file format. Regenerate with
    /// `ICED_MCP_UPDATE_SCHEMA=1` after a deliberate change.
    #[test]
    fn flow_schema_is_committed_and_current() {
        let schema = schemars::schema_for!(Flow);
        let generated = serde_json::to_string_pretty(&schema).unwrap() + "\n";
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/schema");
        let path = format!("{dir}/flow.v1.json");
        if std::env::var_os("ICED_MCP_UPDATE_SCHEMA").is_some() {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(&path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(&path)
            .expect("schema/flow.v1.json missing: run with ICED_MCP_UPDATE_SCHEMA=1");
        assert_eq!(
            committed, generated,
            "flow schema changed: regenerate with ICED_MCP_UPDATE_SCHEMA=1"
        );
    }

    #[test]
    fn names_are_file_safe() {
        assert!(check_name("add-todo_2").is_ok());
        assert!(check_name("../etc").is_err());
        assert!(check_name("").is_err());
    }

    #[test]
    fn flows_round_trip_through_disk() {
        let root = std::env::temp_dir().join(format!("iced-mcp-flow-{}", std::process::id()));
        let flow = Flow {
            schema: None,
            prerequisite: Some("todos is open".into()),
            launch: None,
            fixture: None,
            fixture_args: None,
            steps: vec![FlowStep {
                action: "click".into(),
                args: serde_json::json!({ "text": "Add" }),
            }],
        };
        let path = save(&root, "add", &flow).unwrap();
        assert!(path.ends_with(".iced-mcp/flows/add.json"));
        assert_eq!(list(&root), ["add"]);
        let back = load(&root, "add").unwrap();
        assert_eq!(label(&back.steps[0]), "click \"Add\"");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_run_renders_one_line_per_step_and_a_verdict() {
        let step = |index, status, detail: &str| StepReport {
            index,
            label: format!("click {index}"),
            status,
            detail: detail.into(),
            duration: Duration::from_millis(200),
        };
        let text = render(
            "demo",
            &[
                step(0, StepStatus::Pass, ""),
                step(1, StepStatus::Fail, "nothing matches"),
                step(2, StepStatus::Skip, ""),
            ],
            Duration::from_secs(1),
        );
        assert!(text.contains("[2] ✗ click 1 (0.2s) — nothing matches"));
        assert!(text.contains("[3] - click 2\n"));
        assert!(text.ends_with("FAIL — 1 passed, 1 failed, 0 errored, 1 skipped (1.0s)"));
    }
}
