//! Failure bundles: everything needed to understand a failed flow step
//! without rerunning it, written to `.iced-mcp/failures/<flow>-<timestamp>/`.
//!
//! | File | |
//! |---|---|
//! | `failure.txt` | the failing step, its error, the app and when |
//! | `steps.log` | the run report, one line per step |
//! | `snapshot.txt` / `snapshot.json` | the widget tree at failure (outline with hidden nodes, and raw) |
//! | `diagnostics.txt` / `diagnostics.json` | layout findings at failure |
//! | `screenshot.png` | the window at failure |
//! | `messages.log` | the app's recent messages |
//! | `app-output.log` | the app's last stdout/stderr (apps launched by the server) |
//! | `flow.json` · `script.luau` | the flow or script that ran |
//!
//! Capturing is best effort: an app that crashed has no screenshot, so
//! `<file>.error.txt` says why instead, and the bundle is still written.

use crate::flow::Flow;
use crate::ops::{self, App};
use crate::view::{self, OutlineOptions};

use base64::Engine as _;
use iced_mcp_protocol::{Request, Response, ScreenshotParams};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Where bundles go: `<root>/.iced-mcp/failures`.
pub(crate) fn dir(root: &Path) -> PathBuf {
    root.join(".iced-mcp").join("failures")
}

/// What failed, for `failure.txt`.
pub(crate) struct Failure<'a> {
    pub(crate) flow: &'a str,
    /// `[3] click "Save"`, or `launch` when the app never came up.
    pub(crate) step: &'a str,
    pub(crate) error: &'a str,
}

/// What ran: a flow (saved as `flow.json`) or a script (`script.luau`).
pub(crate) enum Source<'a> {
    Flow(&'a Flow),
    Script(&'a str),
}

/// Captures the app's state at a failure into a new bundle directory and
/// returns its path. `app` is `None` when there is none to ask (the launch
/// failed).
pub(crate) async fn write(
    root: &Path,
    failure: &Failure<'_>,
    source: Source<'_>,
    app: Option<&mut App>,
) -> Result<PathBuf, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f");
    let path = dir(root).join(format!("{}-{stamp}", failure.flow));
    std::fs::create_dir_all(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let save = |name: &str, bytes: &[u8]| {
        let _ = std::fs::write(path.join(name), bytes);
    };
    let save_result = |name: &str, result: Result<Vec<u8>, String>| match result {
        Ok(bytes) => save(name, &bytes),
        Err(error) => save(&format!("{name}.error.txt"), error.as_bytes()),
    };

    let what = match source {
        Source::Flow(_) => "flow",
        Source::Script(_) => "script",
    };
    let mut summary = format!(
        "{what}: {}\nstep: {}\nwhen: {}\n",
        failure.flow,
        failure.step,
        chrono::Local::now().to_rfc3339()
    );
    match source {
        Source::Flow(flow) => {
            if let Ok(text) = serde_json::to_string_pretty(flow) {
                save("flow.json", text.as_bytes());
            }
        }
        Source::Script(text) => save("script.luau", text.as_bytes()),
    }

    match app {
        None => summary.push_str("app: none (it never came up)\n"),
        Some(app) => {
            let _ = writeln!(summary, "app: {}", app.name);
            capture(app, &save_result).await;
        }
    }
    let _ = write!(summary, "\nerror:\n{}\n", failure.error);
    save("failure.txt", summary.as_bytes());
    Ok(path)
}

async fn capture(app: &mut App, save: &impl Fn(&str, Result<Vec<u8>, String>)) {
    match ops::take_snapshot(app, None).await {
        Ok(snapshot) => {
            let outline = view::outline(
                &snapshot,
                &OutlineOptions {
                    query: None,
                    include_hidden: true,
                    max_lines: usize::MAX,
                },
            );
            save("snapshot.txt", Ok(outline.text.into_bytes()));
            save(
                "snapshot.json",
                serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string()),
            );
            let findings = crate::diagnose::diagnose(&snapshot, &[]);
            let visible = snapshot
                .nodes
                .iter()
                .filter(|n| n.visible.is_some())
                .count();
            save(
                "diagnostics.txt",
                Ok(crate::diagnose::render(&snapshot.window, &findings, visible).into_bytes()),
            );
            save(
                "diagnostics.json",
                serde_json::to_vec_pretty(&findings).map_err(|e| e.to_string()),
            );
        }
        Err(error) => {
            save("snapshot.txt", Err(error.clone()));
            save("diagnostics.txt", Err(error));
        }
    }

    let shot = match ops::request(
        app,
        Request::Screenshot(ScreenshotParams {
            window: None,
            region: None,
            max_width: Some(u32::MAX),
        }),
    )
    .await
    {
        Ok(Response::Screenshot(shot)) => base64::engine::general_purpose::STANDARD
            .decode(&shot.png_base64)
            .map_err(|e| format!("bad image from the app: {e}")),
        Ok(_) => Err("unexpected reply to screenshot".into()),
        Err(error) => Err(error),
    };
    save("screenshot.png", shot);

    let messages = ops::read_messages(app, None, None, Some(200))
        .await
        .map(|log| ops::format_messages(&log, usize::MAX).into_bytes());
    save("messages.log", messages);

    let output = match &app.launched {
        Some(launched) => Ok(launched.logs.tail(400, None).join("\n").into_bytes()),
        None => Err(format!(
            "{} was connected, not launched by iced-mcp: its output was not captured",
            app.name
        )),
    };
    save("app-output.log", output);
}

/// Adds the finished run report to a bundle.
pub(crate) fn add_report(path: &Path, report: &str) {
    let _ = std::fs::write(path.join("steps.log"), format!("{report}\n"));
}
