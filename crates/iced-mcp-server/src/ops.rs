//! What the tools do, against one connected [`App`]. The tool layer picks
//! the app; everything here works on it.

use crate::conn::{Conn, LaunchSpec, Launched};
use crate::diff;
use crate::flow::{self, FlowLaunch};
use crate::keys;
use crate::params::*;
use crate::view::{self, OutlineOptions, Selector};

use base64::Engine as _;
use chrono::Datelike;
use iced_mcp_protocol::{
    ActParams, ActionKind, ActionParams, ClipboardParams, HighlightParams, Input, MessageLog,
    MessagesParams, Modifiers, MouseButton, NodeKind, PerfParams, ProbeParams, Rect, Request,
    Response, ScreenshotParams, ScrollUnit, Snapshot, SnapshotParams, StateParams,
    WidgetOperationKind, WidgetOperationParams,
};
use serde::Deserialize;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Hit-test grid for regions no widget reports, in logical pixels.
pub(crate) const GRID: f32 = 8.0;
/// After an action: how long the screen must hold still, and the most to wait.
const ACTION_STABLE: Duration = Duration::from_millis(150);
const ACTION_SETTLE_CAP: Duration = Duration::from_millis(1200);
/// Settling polls this often.
const SETTLE_POLL: Duration = Duration::from_millis(100);
/// screenshot_diff fails a flow step above this % by default.
pub(crate) const DEFAULT_MAX_MISMATCH: f64 = 0.5;

/// One connected (or launched) app.
pub(crate) struct App {
    pub(crate) name: String,
    pub(crate) conn: Option<Conn>,
    pub(crate) launched: Option<Launched>,
    /// How it was started, for `restart` and for flows that launch it.
    pub(crate) spec: Option<LaunchSpec>,
    pub(crate) launch: Option<FlowLaunch>,
    pub(crate) token: Option<String>,
    /// The latest snapshot per window, for `ref` targets.
    pub(crate) snapshots: HashMap<String, Snapshot>,
}

impl App {
    pub(crate) fn new(name: String) -> Self {
        Self {
            name,
            conn: None,
            launched: None,
            spec: None,
            launch: None,
            token: None,
            snapshots: HashMap::new(),
        }
    }
}

/// What steps need beyond the app: where files go.
pub(crate) struct Ctx {
    /// The project root: flows and baselines live under `<root>/.iced-mcp/`.
    pub(crate) root: PathBuf,
    pub(crate) update_baselines: bool,
}

/// Runs one step of a sequence or flow.
pub(crate) async fn run_step(app: &mut App, ctx: &Ctx, step: &Step) -> Result<String, String> {
    fn args<T: for<'de> Deserialize<'de>>(value: &serde_json::Value) -> Result<T, String> {
        let value = if value.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            value.clone()
        };
        serde_json::from_value(value).map_err(|e| format!("bad args: {e}"))
    }
    match step.action.as_str() {
        "click" => click(app, &args(&step.args)?).await,
        "type_text" | "type" => type_text(app, &args(&step.args)?).await,
        "press_key" | "key" => press_key(app, &args(&step.args)?).await,
        "scroll" => scroll(app, &args(&step.args)?).await,
        "drag" => drag(app, &args(&step.args)?).await,
        "hover" => hover(app, &args(&step.args)?).await,
        "paste" => paste(app, &args(&step.args)?).await,
        "wait_for" => wait_for(app, &args(&step.args)?).await,
        "wait_idle" => wait_idle(app, &args(&step.args)?).await,
        "run_app_action" => run_app_action(app, &args(&step.args)?).await,
        "expect_message" => expect_message(app, &args(&step.args)?).await,
        "query" => query_text(app, &args(&step.args)?).await,
        "expect_query" => expect_query(app, &args(&step.args)?).await,
        "screenshot_diff" => {
            let p: DiffParams = args(&step.args)?;
            let outcome = screenshot_diff(app, ctx, &p).await?;
            let limit = p.max_mismatch.unwrap_or(DEFAULT_MAX_MISMATCH);
            if outcome.mismatch > limit {
                return Err(format!(
                    "{:.2}% of pixels differ (limit {limit}%): {}",
                    outcome.mismatch, outcome.text
                ));
            }
            Ok(outcome.text)
        }
        "sleep" => {
            let p: SleepParams = args(&step.args)?;
            let ms = p.ms.unwrap_or(250).min(30_000);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(format!("slept {ms} ms"))
        }
        "echo" => Ok(args::<EchoParams>(&step.args)?.message),
        other => Err(format!(
            "unknown action `{other}`; one of {}",
            flow::STEP_ACTIONS.join(", ")
        )),
    }
}

pub(crate) fn conn(app: &mut App) -> Result<&mut Conn, String> {
    app.conn
        .as_mut()
        .ok_or_else(|| "not connected: call `launch` or `connect` first".to_owned())
}

pub(crate) async fn request(app: &mut App, request: Request) -> Result<Response, String> {
    let result = conn(app)?.request(request).await;
    if let Err(error) = &result {
        if error.contains("connection broke") || error.contains("closed the connection") {
            app.conn = None;
            let exited = match app.launched.as_mut().map(|l| l.child.try_wait()) {
                Some(Ok(Some(status))) => format!(" The app exited ({status}); see `logs`."),
                _ => String::new(),
            };
            return Err(format!("{error}.{exited}"));
        }
    }
    result
}

/// The window a request means, as the key the snapshot cache uses.
pub(crate) fn window_key(app: &App, window: Option<&str>) -> String {
    window.map(str::to_owned).unwrap_or_else(|| {
        app.conn
            .as_ref()
            .and_then(|c| c.info.windows.first())
            .map(|w| w.id.clone())
            .unwrap_or_default()
    })
}

pub(crate) async fn take_snapshot(app: &mut App, window: Option<&str>) -> Result<Snapshot, String> {
    raw_snapshot(app, window, Some(GRID)).await
}

/// A snapshot, cached for `ref` targets. `grid` hit-tests for regions, which
/// costs a frame's worth of work; settling polls without it.
pub(crate) async fn raw_snapshot(
    app: &mut App,
    window: Option<&str>,
    grid: Option<f32>,
) -> Result<Snapshot, String> {
    let response = request(
        app,
        Request::Snapshot(SnapshotParams {
            window: window.map(str::to_owned),
            grid,
        }),
    )
    .await?;
    let Response::Snapshot(snapshot) = response else {
        return Err("unexpected reply to snapshot".into());
    };
    app.snapshots
        .insert(snapshot.window.clone(), snapshot.clone());
    Ok(snapshot)
}

pub(crate) async fn snapshot_text(app: &mut App, p: &SnapshotToolParams) -> Result<String, String> {
    let snapshot = take_snapshot(app, p.window.as_deref()).await?;
    let outline = view::outline(
        &snapshot,
        &OutlineOptions {
            query: p.query.as_deref(),
            include_hidden: p.include_hidden.unwrap_or(false),
            max_lines: p.max_lines.unwrap_or(300),
        },
    );
    let title = app
        .conn
        .as_ref()
        .and_then(|c| c.info.windows.iter().find(|w| w.id == snapshot.window))
        .map(|w| w.title.clone())
        .unwrap_or_default();
    let mut text = format!(
        "window {} {:?} {:.0}×{:.0} — {} of {} nodes\n",
        snapshot.window,
        title,
        snapshot.size.width,
        snapshot.size.height,
        outline.shown,
        snapshot.nodes.len()
    );
    text.push_str(&outline.text);
    if outline.shown == 0 {
        text.push_str(if p.query.is_some() {
            "(nothing matches)\n"
        } else {
            "(no widgets reported — custom-drawn UI? use screenshot)\n"
        });
    }
    Ok(text)
}

/// The visible widget and canvas nodes as a role/name/value hierarchy.
/// This is an MCP accessibility representation; it is not an OS accessibility API.
pub(crate) async fn accessibility_tree(
    app: &mut App,
    p: &SnapshotToolParams,
) -> Result<String, String> {
    let snapshot = take_snapshot(app, p.window.as_deref()).await?;
    let node_indexes: HashSet<_> = snapshot.nodes.iter().map(|node| node.index).collect();
    let invalid_parent_references = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.parent
                .is_some_and(|parent| !node_indexes.contains(&parent))
        })
        .count();
    let clipped_nodes = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.visible.is_some_and(|visible| {
                (visible.x - node.bounds.x).abs() > 0.1
                    || (visible.y - node.bounds.y).abs() > 0.1
                    || (visible.width - node.bounds.width).abs() > 0.1
                    || (visible.height - node.bounds.height).abs() > 0.1
            })
        })
        .count();
    let mut names = HashMap::<String, usize>::new();
    for node in &snapshot.nodes {
        if node.visible.is_some() {
            if let Some(name) = node.label.as_deref().or(node.text.as_deref()) {
                if !name.trim().is_empty() {
                    *names.entry(name.to_owned()).or_default() += 1;
                }
            }
        }
    }
    let mut duplicate_names: Vec<_> = names.into_iter().filter(|(_, count)| *count > 1).collect();
    duplicate_names.sort_by(|left, right| left.0.cmp(&right.0));
    let tree = serde_json::json!({
        "schema": "iced-mcp/accessibility-tree@2",
        "window": snapshot.window,
        "size": snapshot.size,
        "accessibilityModel": "AccessKit",
        "platformAdapter": false,
        "snapshotId": crate::accessibility::snapshot_id(&snapshot),
        "coverage": {
            "totalNodes": snapshot.nodes.len(),
            "visibleNodes": snapshot.nodes.iter().filter(|node| node.visible.is_some()).count(),
            "offscreenNodes": snapshot.nodes.iter().filter(|node| node.visible.is_none()).count(),
            "clippedNodes": clipped_nodes,
            "baseNodes": snapshot.nodes.iter().filter(|node| node.layer == iced_mcp_protocol::Layer::Base && node.visible.is_some()).count(),
            "overlayNodes": snapshot.nodes.iter().filter(|node| node.layer == iced_mcp_protocol::Layer::Overlay && node.visible.is_some()).count(),
            "fallbackRegions": snapshot.nodes.iter().filter(|node| node.kind == NodeKind::Region && node.visible.is_some()).count(),
            "invalidParentReferences": invalid_parent_references,
            "duplicateNames": duplicate_names,
        },
        "accessKitTree": crate::accessibility::projection(&snapshot),
    });
    serde_json::to_string_pretty(&tree).map_err(|error| error.to_string())
}

async fn widget_operation(
    app: &mut App,
    window: Option<&str>,
    id: &str,
    operation: WidgetOperationKind,
) -> Result<bool, String> {
    match request(
        app,
        Request::WidgetOperation(WidgetOperationParams {
            window: window.map(str::to_owned),
            id: id.to_owned(),
            operation,
        }),
    )
    .await?
    {
        Response::WidgetOperation { found } => Ok(found),
        _ => Err("unexpected reply to widget operation".to_owned()),
    }
}

async fn select_calendar_date(
    app: &mut App,
    window: Option<&str>,
    target: chrono::NaiveDate,
    snapshot: bool,
) -> Result<String, String> {
    let requested = target.format("%Y-%m-%d").to_string();
    let opened = take_snapshot(app, window).await?;
    let calendar = opened
        .nodes
        .iter()
        .find(|candidate| {
            candidate.kind == NodeKind::Semantic
                && candidate.role.as_deref() == Some("calendar")
                && candidate.id.as_deref() == Some("calendar")
                && candidate.visible.is_some()
        })
        .ok_or_else(|| "the open date picker did not expose its calendar semantics".to_owned())?;
    let current_month = calendar
        .value
        .as_deref()
        .and_then(|value| chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .ok_or_else(|| "calendar did not report its visible month".to_owned())?;
    let month_index = |date: chrono::NaiveDate| date.year() * 12 + date.month0() as i32;
    let month_delta = month_index(target) - month_index(current_month);
    if month_delta.unsigned_abs() > 1200 {
        return Err("date is more than 100 years from the currently shown month".to_owned());
    }
    if month_delta != 0 {
        let nav_id = if month_delta < 0 {
            "calendar-prev"
        } else {
            "calendar-next"
        };
        let nav = opened
            .nodes
            .iter()
            .find(|candidate| {
                candidate.kind == NodeKind::Semantic
                    && candidate.id.as_deref() == Some(nav_id)
                    && candidate.visible.is_some()
            })
            .ok_or_else(|| format!("calendar is missing its {nav_id} control"))?;
        let (nav_x, nav_y) = nav
            .visible
            .map(|bounds| bounds.center())
            .ok_or_else(|| format!("calendar {nav_id} is not visible"))?;
        let mut navigation = Vec::with_capacity(month_delta.unsigned_abs() as usize * 2);
        for _ in 0..month_delta.unsigned_abs() {
            navigation.push(vec![Input::Move { x: nav_x, y: nav_y }]);
            navigation.push(vec![
                Input::Press {
                    button: MouseButton::Left,
                },
                Input::Release {
                    button: MouseButton::Left,
                },
            ]);
        }
        act(app, window, navigation, "Navigated calendar", false).await?;
    }
    let calendar_view = take_snapshot(app, window).await?;
    let target_id = format!("day-{requested}");
    let day = calendar_view
        .nodes
        .iter()
        .find(|candidate| {
            candidate.kind == NodeKind::Semantic
                && candidate.id.as_deref() == Some(target_id.as_str())
        })
        .ok_or_else(|| format!("calendar did not expose the date {requested}"))?;
    if day.disabled == Some(true) {
        return Err(format!("date {requested} is disabled"));
    }
    let (day_x, day_y) = day
        .visible
        .map(|bounds| bounds.center())
        .ok_or_else(|| format!("date {requested} is not visible"))?;
    act(
        app,
        window,
        vec![
            vec![Input::Move { x: day_x, y: day_y }],
            vec![
                Input::Press {
                    button: MouseButton::Left,
                },
                Input::Release {
                    button: MouseButton::Left,
                },
            ],
        ],
        &format!("Selected {requested}"),
        snapshot,
    )
    .await
}

/// Dispatches a supported AccessKit action through iced's normal synthetic
/// input path, using the node's latest visible bounds.
pub(crate) async fn accessibility_action(
    app: &mut App,
    p: &AccessibilityActionParams,
) -> Result<String, String> {
    const FIRST_WIDGET_NODE: u64 = 2;
    let index = p
        .node_id
        .checked_sub(FIRST_WIDGET_NODE)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(|| "node_id is not a widget node from accessibility_tree".to_owned())?;
    let snapshot = take_snapshot(app, p.window.as_deref()).await?;
    if crate::accessibility::snapshot_id(&snapshot) != p.snapshot_id {
        return Err("accessibility snapshot is stale; call accessibility_tree again".to_owned());
    }
    let node = snapshot
        .nodes
        .iter()
        .find(|node| node.index == index)
        .ok_or_else(|| "node_id is stale; call accessibility_tree again".to_owned())?;
    let bounds = node.visible.ok_or_else(|| {
        "node is not visible; call accessibility_tree after scrolling it into view".to_owned()
    })?;
    if node.disabled == Some(true) {
        return Err("node is disabled".to_owned());
    }
    let (x, y) = bounds.center();
    let click_frames = || {
        vec![
            vec![Input::Move { x, y }],
            vec![
                Input::Press {
                    button: MouseButton::Left,
                },
                Input::Release {
                    button: MouseButton::Left,
                },
            ],
        ]
    };
    if p.action == "focus" && matches!(node.kind, NodeKind::Focusable | NodeKind::TextInput) {
        let id = node.id.as_deref().ok_or_else(|| {
            "cannot focus this widget directly: it has no stable iced id".to_owned()
        })?;
        if !widget_operation(app, p.window.as_deref(), id, WidgetOperationKind::Focus).await? {
            return Err(format!("iced could not find focusable widget #{id}"));
        }
        let mut result = format!(
            "Focused {} directly through iced's widget operation.",
            node.label.as_deref().or(node.text.as_deref()).unwrap_or(id)
        );
        if p.snapshot.unwrap_or(false) {
            result.push('\n');
            result.push_str(
                &snapshot_text(
                    app,
                    &SnapshotToolParams {
                        window: p.window.clone(),
                        ..SnapshotToolParams::default()
                    },
                )
                .await?,
            );
        }
        return Ok(result);
    }

    let semantic_slider = node.kind == NodeKind::Semantic && node.role.as_deref() == Some("slider");
    let semantic_spinbutton =
        node.kind == NodeKind::Semantic && node.role.as_deref() == Some("spinbutton");
    let semantic_textbox =
        node.kind == NodeKind::Semantic && node.role.as_deref() == Some("textbox");
    let semantic_boolean = node.kind == NodeKind::Semantic
        && matches!(node.role.as_deref(), Some("switch" | "checkbox"))
        && node.checked.is_some();
    let semantic_select = node.kind == NodeKind::Semantic
        && node.role.as_deref() == Some("combobox")
        && node
            .choices
            .as_ref()
            .is_some_and(|choices| !choices.is_empty());
    let semantic_time = node.kind == NodeKind::Semantic && node.role.as_deref() == Some("time");
    let semantic_date = node.kind == NodeKind::Semantic
        && node.role.as_deref() == Some("date")
        && node.description.as_deref().is_some_and(|description| {
            description.starts_with("Single date picker")
                || description.starts_with("Date range picker")
        });

    if p.action == "set_value" && semantic_time {
        let requested = p
            .value
            .as_deref()
            .ok_or_else(|| "value is required for set_value".to_owned())?;
        let format = node.description.as_deref().unwrap_or("");
        let (digits, period) = parse_time_value(requested, format)?;
        if node.value.as_deref() == Some(requested) {
            return Ok(format!("Time is already {requested}."));
        }
        let mut frames = click_frames();
        frames.extend(keys::typing(&digits));
        if let Some(period) = period {
            frames.extend(keys::typing(&period.to_string()));
        }
        let result = act(
            app,
            p.window.as_deref(),
            frames,
            "Set time",
            p.snapshot.unwrap_or(false),
        )
        .await?;
        let updated = take_snapshot(app, p.window.as_deref()).await?;
        let actual = updated
            .nodes
            .iter()
            .find(|candidate| {
                candidate.kind == NodeKind::Semantic
                    && candidate.role.as_deref() == Some("time")
                    && candidate.bounds == node.bounds
            })
            .and_then(|candidate| candidate.value.as_deref())
            .unwrap_or("unknown");
        return Ok(format!("{result}\nTime value now {actual}"));
    }

    if p.action == "set_value" && semantic_date {
        let requested = p
            .value
            .as_deref()
            .ok_or_else(|| "value is required for set_value".to_owned())?;
        let is_range = node
            .description
            .as_deref()
            .is_some_and(|description| description.starts_with("Date range picker"));
        let mut targets = Vec::with_capacity(if is_range { 2 } else { 1 });
        if is_range {
            let (start, end) = requested
                .split_once('/')
                .ok_or_else(|| "date range value must be YYYY-MM-DD/YYYY-MM-DD".to_owned())?;
            let start = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
                .map_err(|_| "date range start must be a valid YYYY-MM-DD date".to_owned())?;
            let end = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
                .map_err(|_| "date range end must be a valid YYYY-MM-DD date".to_owned())?;
            if start > end {
                return Err("date range start must be on or before its end".to_owned());
            }
            targets.extend([start, end]);
        } else {
            targets.push(
                chrono::NaiveDate::parse_from_str(requested, "%Y-%m-%d")
                    .map_err(|_| "date value must be a valid YYYY-MM-DD date".to_owned())?,
            );
        }
        if node.value.as_deref() == Some(requested) {
            return Ok(format!("Date value is already {requested}."));
        }
        act(
            app,
            p.window.as_deref(),
            click_frames(),
            "Opened date picker",
            false,
        )
        .await?;
        let mut result = String::new();
        for (index, target) in targets.iter().enumerate() {
            result = select_calendar_date(
                app,
                p.window.as_deref(),
                *target,
                p.snapshot.unwrap_or(false) && index + 1 == targets.len(),
            )
            .await?;
            if index + 1 < targets.len() {
                let refreshed = take_snapshot(app, p.window.as_deref()).await?;
                let value = refreshed
                    .nodes
                    .iter()
                    .find(|candidate| {
                        candidate.kind == NodeKind::Semantic
                            && candidate.role.as_deref() == Some("date")
                            && candidate.label == node.label
                            && candidate.bounds == node.bounds
                    })
                    .and_then(|candidate| candidate.value.as_deref());
                if value
                    .is_none_or(|value| !value.starts_with(&target.format("%Y-%m-%d").to_string()))
                {
                    return Err(
                        "date range start was not applied; end date was not selected".to_owned(),
                    );
                }
            }
        }
        let updated = take_snapshot(app, p.window.as_deref()).await?;
        let actual = updated
            .nodes
            .iter()
            .find(|candidate| {
                candidate.kind == NodeKind::Semantic
                    && candidate.role.as_deref() == Some("date")
                    && candidate.label == node.label
                    && candidate.bounds == node.bounds
            })
            .and_then(|candidate| candidate.value.as_deref())
            .unwrap_or(requested);
        return Ok(format!("{result}\nDate value now {actual}"));
    }

    if p.action == "set_value" && semantic_select {
        let requested = p
            .value
            .as_deref()
            .ok_or_else(|| "value is required for set_value".to_owned())?;
        let choices = node.choices.as_deref().unwrap_or_default();
        if !choices.iter().any(|choice| choice == requested) {
            return Err(format!(
                "{requested:?} is not an enabled option; choose one of: {}",
                choices.join(", ")
            ));
        }
        if node.value.as_deref() == Some(requested) {
            return Ok(format!("{} is already selected.", requested));
        }

        act(
            app,
            p.window.as_deref(),
            click_frames(),
            "Opened select",
            false,
        )
        .await?;
        let expanded = take_snapshot(app, p.window.as_deref()).await?;
        let option = expanded
            .nodes
            .iter()
            .find(|candidate| {
                candidate.layer == iced_mcp_protocol::Layer::Overlay
                    && candidate.visible.is_some()
                    && candidate.text.as_deref() == Some(requested)
                    && candidate.disabled != Some(true)
            })
            .ok_or_else(|| format!("enabled select option {requested:?} is not visible"))?;
        let (option_x, option_y) = option
            .visible
            .map(|bounds| bounds.center())
            .ok_or_else(|| format!("enabled select option {requested:?} is not visible"))?;
        let selected = act(
            app,
            p.window.as_deref(),
            vec![
                vec![Input::Move {
                    x: option_x,
                    y: option_y,
                }],
                vec![
                    Input::Press {
                        button: MouseButton::Left,
                    },
                    Input::Release {
                        button: MouseButton::Left,
                    },
                ],
            ],
            &format!("Selected {requested}"),
            p.snapshot.unwrap_or(false),
        )
        .await?;
        let updated = take_snapshot(app, p.window.as_deref()).await?;
        let actual = updated
            .nodes
            .iter()
            .find(|candidate| {
                candidate.kind == NodeKind::Semantic
                    && candidate.role.as_deref() == Some("combobox")
                    && candidate.label == node.label
                    && candidate.bounds == node.bounds
            })
            .and_then(|candidate| candidate.value.as_deref())
            .unwrap_or(requested);
        return Ok(format!("{selected}\nSelect value now {actual}"));
    }

    let frames = match p.action.as_str() {
        "set_value" if semantic_textbox => {
            let requested = p
                .value
                .as_deref()
                .ok_or_else(|| "value is required for set_value".to_owned())?;
            if node
                .description
                .as_deref()
                .is_some_and(|description| description.starts_with("Hexadecimal color input"))
            {
                let digits = requested.strip_prefix('#').unwrap_or(requested);
                if !matches!(digits.len(), 3 | 4 | 6 | 8)
                    || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(
                        "color value must be #rgb, #rgba, #rrggbb, or #rrggbbaa hexadecimal"
                            .to_owned(),
                    );
                }
            }
            if node
                .value
                .as_deref()
                .is_some_and(|current| current.eq_ignore_ascii_case(requested))
            {
                return Ok(format!("Value is already {requested}."));
            }
            let field_x = bounds.x + bounds.width * 0.55;
            let mut frames = vec![
                vec![Input::Move { x: field_x, y }],
                vec![
                    Input::Press {
                        button: MouseButton::Left,
                    },
                    Input::Release {
                        button: MouseButton::Left,
                    },
                ],
            ];
            frames.extend(keys::press(
                Modifiers {
                    ctrl: !cfg!(target_os = "macos"),
                    logo: cfg!(target_os = "macos"),
                    ..Modifiers::default()
                },
                "a",
            ));
            frames.extend(keys::typing(requested));
            frames.extend(keys::press(Modifiers::default(), "Enter"));
            frames
        }
        "set_value" if semantic_spinbutton => {
            let requested = p
                .value
                .as_deref()
                .ok_or_else(|| "value is required for set_value".to_owned())?
                .parse::<f64>()
                .map_err(|_| "value for set_value must be a number".to_owned())?;
            if !requested.is_finite()
                || node
                    .min_numeric_value
                    .is_some_and(|minimum| requested < minimum)
                || node
                    .max_numeric_value
                    .is_some_and(|maximum| requested > maximum)
            {
                return Err(
                    "value must be finite and within the reported numeric limits".to_owned(),
                );
            }
            let field_x = bounds.x + bounds.width * 0.35;
            let mut frames = vec![
                vec![Input::Move { x: field_x, y }],
                vec![
                    Input::Press {
                        button: MouseButton::Left,
                    },
                    Input::Release {
                        button: MouseButton::Left,
                    },
                ],
            ];
            frames.extend(keys::press(
                Modifiers {
                    ctrl: !cfg!(target_os = "macos"),
                    logo: cfg!(target_os = "macos"),
                    ..Modifiers::default()
                },
                "a",
            ));
            frames.extend(keys::typing(&requested.to_string()));
            frames.extend(keys::press(Modifiers::default(), "Enter"));
            frames
        }
        "increment" | "decrement" if semantic_spinbutton => {
            if node.numeric_value_step.is_none() {
                return Err("numeric input does not report its step size".to_owned());
            }
            let field_x = bounds.x + bounds.width * 0.35;
            let mut frames = vec![
                vec![Input::Move { x: field_x, y }],
                vec![
                    Input::Press {
                        button: MouseButton::Left,
                    },
                    Input::Release {
                        button: MouseButton::Left,
                    },
                ],
            ];
            frames.extend(keys::press(
                Modifiers::default(),
                if p.action == "increment" {
                    "ArrowUp"
                } else {
                    "ArrowDown"
                },
            ));
            frames
        }
        "set_value" if semantic_slider => {
            let value = p
                .value
                .as_deref()
                .ok_or_else(|| "value is required for set_value".to_owned())?
                .parse::<f64>()
                .map_err(|_| "value for set_value must be a number".to_owned())?;
            let minimum = node
                .min_numeric_value
                .ok_or_else(|| "slider does not report its minimum".to_owned())?;
            let maximum = node
                .max_numeric_value
                .ok_or_else(|| "slider does not report its maximum".to_owned())?;
            let track = node
                .track_bounds
                .ok_or_else(|| "slider does not report its track bounds".to_owned())?;
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(format!("value must be within {minimum}..={maximum}"));
            }
            let ratio = if maximum > minimum {
                (value - minimum) / (maximum - minimum)
            } else {
                0.0
            };
            let ratio = if node.inverted.unwrap_or(false) {
                1.0 - ratio
            } else {
                ratio
            };
            let (target_x, target_y) = if node.vertical.unwrap_or(false) {
                (
                    track.x + track.width / 2.0,
                    track.y + track.height * (1.0 - ratio) as f32,
                )
            } else {
                (
                    track.x + track.width * ratio as f32,
                    track.y + track.height / 2.0,
                )
            };
            vec![
                vec![Input::Move { x, y }],
                vec![Input::Press {
                    button: MouseButton::Left,
                }],
                vec![Input::Move {
                    x: target_x,
                    y: target_y,
                }],
                vec![Input::Release {
                    button: MouseButton::Left,
                }],
            ]
        }
        "increment" | "decrement" if semantic_slider => {
            if node.numeric_value_step.is_none() {
                return Err("slider does not report its step size".to_owned());
            }
            let increasing = p.action == "increment";
            let right_increases = !node.inverted.unwrap_or(false);
            let key = match (increasing, right_increases) {
                (true, true) | (false, false) => "ArrowRight",
                (true, false) | (false, true) => "ArrowLeft",
            };
            let mut frames = click_frames();
            frames.extend(keys::press(iced_mcp_protocol::Modifiers::default(), key));
            frames
        }
        "set_value" if semantic_boolean => {
            let requested = p
                .value
                .as_deref()
                .ok_or_else(|| "value is required for set_value (true or false)".to_owned())?
                .parse::<bool>()
                .map_err(|_| "value for a switch or checkbox must be true or false".to_owned())?;
            if node.checked == Some(requested) {
                return Ok(format!(
                    "{} is already {}.",
                    node.label
                        .as_deref()
                        .or(node.text.as_deref())
                        .unwrap_or("Control"),
                    requested
                ));
            }
            click_frames()
        }
        "click" | "focus" | "expand" | "collapse" => click_frames(),
        "scroll_up" if node.kind == NodeKind::Scrollable => vec![
            vec![Input::Move { x, y }],
            vec![Input::Scroll {
                x: 0.0,
                y: 3.0,
                unit: ScrollUnit::Lines,
            }],
        ],
        "scroll_down" if node.kind == NodeKind::Scrollable => vec![
            vec![Input::Move { x, y }],
            vec![Input::Scroll {
                x: 0.0,
                y: -3.0,
                unit: ScrollUnit::Lines,
            }],
        ],
        "replace_selected_text" if node.kind == NodeKind::TextInput => {
            let value = p
                .value
                .as_deref()
                .ok_or_else(|| "value is required for replace_selected_text".to_owned())?;
            let id = node.id.as_deref().ok_or_else(|| {
                "cannot replace text directly: the text input has no stable iced id".to_owned()
            })?;
            if !widget_operation(app, p.window.as_deref(), id, WidgetOperationKind::Focus).await? {
                return Err(format!("iced could not find text input #{id}"));
            }
            if !widget_operation(app, p.window.as_deref(), id, WidgetOperationKind::SelectAll)
                .await?
            {
                return Err(format!("iced could not select text in input #{id}"));
            }
            let mut frames = keys::press(iced_mcp_protocol::Modifiers::default(), "Backspace");
            frames.extend(keys::typing(value));
            frames
        }
        "scroll_up" | "scroll_down" => {
            return Err("scroll actions require a scrollable node".to_owned());
        }
        "replace_selected_text" => {
            return Err("replace_selected_text requires a text input node".to_owned());
        }
        action => return Err(format!("unsupported accessibility action: {action}")),
    };
    let mut result = act(
        app,
        p.window.as_deref(),
        frames,
        &format!("Accessibility action {} on node {}", p.action, p.node_id),
        p.snapshot.unwrap_or(false),
    )
    .await?;
    if semantic_slider && matches!(p.action.as_str(), "set_value" | "increment" | "decrement") {
        let updated = take_snapshot(app, p.window.as_deref()).await?;
        let current = updated.nodes.iter().find(|candidate| {
            candidate.kind == NodeKind::Semantic
                && candidate.role.as_deref() == Some("slider")
                && candidate.label == node.label
                && candidate.min_numeric_value == node.min_numeric_value
                && candidate.max_numeric_value == node.max_numeric_value
                && candidate.track_bounds == node.track_bounds
        });
        if let Some(value) = current.and_then(|candidate| candidate.numeric_value) {
            let _ = write!(result, "\nSlider value now {value}");
        } else {
            result.push_str(
                "\nCould not read the slider's updated value; refresh accessibility_tree.",
            );
        }
    }
    if semantic_boolean && p.action == "set_value" {
        let updated = take_snapshot(app, p.window.as_deref()).await?;
        let current = updated.nodes.iter().find(|candidate| {
            candidate.kind == NodeKind::Semantic
                && candidate.role == node.role
                && candidate.label == node.label
                && candidate.bounds == node.bounds
        });
        if let Some(checked) = current.and_then(|candidate| candidate.checked) {
            let _ = write!(result, "\nControl value now {checked}");
        }
    }
    Ok(result)
}

fn parse_time_value(value: &str, format: &str) -> Result<(String, Option<char>), String> {
    let is_12_hour = format.starts_with("12-hour");
    let has_seconds = format.contains("SS");
    let (clock, period) = if is_12_hour {
        let (clock, period) = value
            .rsplit_once(' ')
            .ok_or_else(|| "12-hour value must end in AM or PM".to_owned())?;
        let period = match period.to_ascii_uppercase().as_str() {
            "AM" => 'a',
            "PM" => 'p',
            _ => return Err("12-hour value must end in AM or PM".to_owned()),
        };
        (clock, Some(period))
    } else {
        (value, None)
    };
    let parts = clock.split(':').collect::<Vec<_>>();
    let expected = if has_seconds { 3 } else { 2 };
    if parts.len() != expected
        || parts
            .iter()
            .any(|part| part.len() != 2 || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        let expected = if is_12_hour {
            if has_seconds {
                "hh:mm:ss AM/PM"
            } else {
                "hh:mm AM/PM"
            }
        } else if has_seconds {
            "HH:MM:SS"
        } else {
            "HH:MM"
        };
        return Err(format!("time value must use {expected}"));
    }
    let values = parts
        .iter()
        .map(|part| part.parse::<u32>().unwrap_or(u32::MAX))
        .collect::<Vec<_>>();
    let max_hour = if is_12_hour { 12 } else { 23 };
    if (is_12_hour && values[0] == 0)
        || values[0] > max_hour
        || values[1] > 59
        || values.get(2).is_some_and(|second| *second > 59)
    {
        return Err("time value contains an out-of-range hour, minute, or second".to_owned());
    }
    Ok((parts.concat(), period))
}

/// A resolved target: the point to aim at and how to describe it.
pub(crate) struct Aim {
    x: f32,
    y: f32,
    label: String,
    bounds: Option<Rect>,
}

pub(crate) async fn resolve(
    app: &mut App,
    window: Option<&str>,
    target: &Target,
) -> Result<Aim, String> {
    if let (Some(x), Some(y)) = (target.x, target.y) {
        return Ok(Aim {
            x,
            y,
            label: format!("({x:.0},{y:.0})"),
            bounds: None,
        });
    }
    if target.x.is_some() || target.y.is_some() {
        return Err("give both x and y".into());
    }
    if let Some(reference) = target.reference {
        let key = window_key(app, window);
        let node = app
            .snapshots
            .get(&key)
            .and_then(|s| s.nodes.get(reference))
            .ok_or_else(|| format!("no [{reference}] in the latest snapshot of window {key}: take a snapshot first"))?;
        let (x, y) = view::aim(node);
        return Ok(Aim {
            x,
            y,
            label: view::line(node),
            bounds: Some(node.visible.unwrap_or(node.bounds)),
        });
    }
    let selector = if let Some(id) = &target.id {
        Selector::Id(id)
    } else if let Some(text) = &target.text {
        Selector::Text {
            text,
            exact: target.exact.unwrap_or(false),
        }
    } else {
        return Err("no target: give text, id, ref, or x and y".into());
    };
    let snapshot = take_snapshot(app, window).await?;
    let found = view::find(&snapshot, &selector);
    let nth = target.nth.unwrap_or(0);
    match found.get(nth) {
        Some(node) => {
            let (x, y) = view::aim(node);
            let mut label = view::line(node);
            if found.len() > 1 {
                let _ = write!(label, " (match {} of {})", nth + 1, found.len());
            }
            Ok(Aim {
                x,
                y,
                label,
                bounds: Some(node.visible.unwrap_or(node.bounds)),
            })
        }
        None => {
            let what = match selector {
                Selector::Id(id) => format!("#{id}"),
                Selector::Text { text, .. } => format!("{text:?}"),
            };
            let texts = view::visible_texts(&snapshot, 25);
            Err(if found.is_empty() {
                format!(
                    "nothing visible matches {what}. Visible text: {texts:?}. It may be custom-drawn (try screenshot + x/y), scrolled away (scroll), or not open yet (wait_for)."
                )
            } else {
                format!(
                    "only {} match(es) for {what}; nth={nth} is out of range",
                    found.len()
                )
            })
        }
    }
}

pub(crate) async fn read_messages(
    app: &mut App,
    since: Option<u64>,
    filter: Option<String>,
    limit: Option<usize>,
) -> Result<MessageLog, String> {
    match request(
        app,
        Request::Messages(MessagesParams {
            since,
            limit,
            filter,
        }),
    )
    .await?
    {
        Response::Messages(log) => Ok(log),
        _ => Err("unexpected reply to messages".into()),
    }
}

pub(crate) fn format_messages(log: &MessageLog, limit: usize) -> String {
    let mut text = String::new();
    let skip = log.entries.len().saturating_sub(limit);
    if skip > 0 {
        let _ = writeln!(text, "  … {skip} earlier");
    }
    for entry in log.entries.iter().skip(skip) {
        let _ = writeln!(text, "  #{} +{}ms {}", entry.seq, entry.at_ms, entry.text);
    }
    text
}

/// An action's messages, compact: each kind once (its first occurrence,
/// shortened) with a count, so periodic traffic — autosave ticks, live data
/// — does not bury what the input caused.
pub(crate) fn summarize_messages(log: &MessageLog, max_kinds: usize) -> String {
    /// Long payloads (a whole order book) say little in a reply.
    const MAX_LINE: usize = 160;
    let mut kinds: Vec<(String, &iced_mcp_protocol::MessageEntry, usize)> = Vec::new();
    for entry in &log.entries {
        let kind = message_kind(&entry.text);
        match kinds.iter_mut().find(|(k, _, _)| *k == kind) {
            Some((_, _, count)) => *count += 1,
            None => kinds.push((kind, entry, 1)),
        }
    }
    let mut text = String::new();
    for (_, entry, count) in kinds.iter().take(max_kinds) {
        let mut line: String = entry.text.chars().take(MAX_LINE).collect();
        if entry.text.chars().count() > MAX_LINE {
            line.push('…');
        }
        let times = if *count > 1 {
            format!(" ×{count}")
        } else {
            String::new()
        };
        let _ = writeln!(text, "  #{} {line}{times}", entry.seq);
    }
    if kinds.len() > max_kinds {
        let _ = writeln!(
            text,
            "  … {} more kinds; see `messages`",
            kinds.len() - max_kinds
        );
    }
    text
}

/// What makes two messages "the same kind" in a reply: their first 80
/// characters with digits dropped, so `Book([71889.6, ..])` updates group
/// together while `Hover(..)` and `Opened(..)` on the same widget do not.
pub(crate) fn message_kind(text: &str) -> String {
    /// Long enough to reach the variant that differs, short of the payload.
    const KEY: usize = 80;
    text.chars()
        .take(KEY)
        .filter(|c| !c.is_ascii_digit())
        .collect()
}

/// Runs the frames, then reports what the app did with them.
pub(crate) async fn act(
    app: &mut App,
    window: Option<&str>,
    frames: Vec<Vec<Input>>,
    what: &str,
    then_snapshot: bool,
) -> Result<String, String> {
    let before = read_messages(app, Some(u64::MAX), None, Some(1))
        .await
        .ok()
        .map(|log| log.last);
    let response = request(
        app,
        Request::Act(ActParams {
            window: window.map(str::to_owned),
            frames,
        }),
    )
    .await?;
    let Response::Acted(acted) = response else {
        return Err("unexpected reply to act".into());
    };
    // Wait for the screen to stop changing (bounded), so the reply and the
    // agent's next look see the result, not a frame in between.
    let settled = settle(app, window, ACTION_STABLE, ACTION_SETTLE_CAP).await;

    let mut text = what.to_owned();
    match settled {
        Ok(s) if s.settled => {
            let _ = write!(text, " (settled in {} ms)", s.waited.as_millis());
        }
        Ok(s) => {
            let _ = write!(
                text,
                " (still changing after {} ms — use wait_idle or wait_for if the next step depends on it)",
                s.waited.as_millis()
            );
        }
        Err(_) => {}
    }
    match before {
        Some(since) => {
            let log = read_messages(app, Some(since), None, Some(200)).await?;
            if log.entries.is_empty() {
                text.push_str("\nNo messages published");
                text.push_str(if acted.captured == 0 {
                    " — nothing reacted; check the target with snapshot or screenshot."
                } else {
                    "."
                });
            } else {
                let _ = write!(
                    text,
                    "\n{} message(s):\n{}",
                    log.entries.len(),
                    summarize_messages(&log, 16)
                );
            }
        }
        None => {
            let _ = write!(text, "\n{} message(s) published.", acted.messages);
        }
    }
    if then_snapshot {
        let snap = snapshot_text(
            app,
            &SnapshotToolParams {
                window: window.map(str::to_owned),
                ..SnapshotToolParams::default()
            },
        )
        .await?;
        let _ = write!(text, "\n{snap}");
    }
    Ok(text)
}

pub(crate) fn modifiers(spec: Option<&str>) -> Result<Modifiers, String> {
    match spec {
        None | Some("") => Ok(Modifiers::default()),
        // "Shift" alone parses as a key named Shift: add a dummy key.
        Some(spec) => keys::chord(&format!("{spec}+x")).map(|(m, _)| m),
    }
}

/// Wraps frames with modifiers held down and released around them.
pub(crate) fn hold(modifiers: Modifiers, mut frames: Vec<Vec<Input>>) -> Vec<Vec<Input>> {
    if modifiers == Modifiers::default() {
        return frames;
    }
    frames.insert(0, vec![Input::Modifiers { modifiers }]);
    frames.push(vec![Input::Modifiers {
        modifiers: Modifiers::default(),
    }]);
    frames
}

pub(crate) async fn click(app: &mut App, p: &ClickParams) -> Result<String, String> {
    let aim = resolve(app, p.window.as_deref(), &p.target).await?;
    let button = p.button.unwrap_or_default();
    let count = p.count.unwrap_or(1).clamp(1, 3);
    let mut frames = vec![vec![Input::Move { x: aim.x, y: aim.y }]];
    for _ in 0..count {
        frames.push(vec![Input::Press { button }, Input::Release { button }]);
    }
    let frames = hold(modifiers(p.modifiers.as_deref())?, frames);
    let verb = match count {
        2 => "Double-clicked",
        3 => "Triple-clicked",
        _ => "Clicked",
    };
    act(
        app,
        p.window.as_deref(),
        frames,
        &format!("{verb} {}", aim.label),
        p.snapshot.unwrap_or(false),
    )
    .await
}

pub(crate) async fn type_text(app: &mut App, p: &TypeParams) -> Result<String, String> {
    let mut frames = Vec::new();
    let mut what = String::from("Typed");
    if !p.target.is_empty() {
        let aim = resolve(app, p.window.as_deref(), &p.target).await?;
        frames.push(vec![Input::Move { x: aim.x, y: aim.y }]);
        frames.push(vec![
            Input::Press {
                button: MouseButton::Left,
            },
            Input::Release {
                button: MouseButton::Left,
            },
        ]);
        what = format!("Focused {} and typed", aim.label);
    }
    if p.clear.unwrap_or(false) {
        let (m, key) = keys::chord("Mod+a")?;
        frames.extend(keys::press(m, &key));
        frames.extend(keys::press(Modifiers::default(), "Backspace"));
    }
    frames.extend(keys::typing(&p.text));
    if p.submit.unwrap_or(false) {
        frames.extend(keys::press(Modifiers::default(), "Enter"));
        what.push_str(" (then Enter)");
    }
    let shown: String = p.text.chars().take(80).collect();
    act(
        app,
        p.window.as_deref(),
        frames,
        &format!("{what} {shown:?}"),
        p.snapshot.unwrap_or(false),
    )
    .await
}

pub(crate) async fn press_key(app: &mut App, p: &KeyParams) -> Result<String, String> {
    let (m, key) = keys::chord(&p.keys)?;
    let repeat = p.repeat.unwrap_or(1).clamp(1, 200);
    let mut frames = Vec::new();
    for _ in 0..repeat {
        frames.extend(keys::press(m, &key));
    }
    let times = if repeat > 1 {
        format!(" ×{repeat}")
    } else {
        String::new()
    };
    act(
        app,
        p.window.as_deref(),
        frames,
        &format!("Pressed {}{times}", p.keys),
        p.snapshot.unwrap_or(false),
    )
    .await
}

pub(crate) async fn scroll(app: &mut App, p: &ScrollParams) -> Result<String, String> {
    let (x, y, label) = if p.target.is_empty() {
        let size = window_size(app, p.window.as_deref()).await?;
        (size.0 / 2.0, size.1 / 2.0, "the window centre".to_owned())
    } else {
        let aim = resolve(app, p.window.as_deref(), &p.target).await?;
        (aim.x, aim.y, aim.label)
    };
    let dy = p.dy.unwrap_or(0.0);
    let dx = p.dx.unwrap_or(0.0);
    if dx == 0.0 && dy == 0.0 {
        return Err("give dy (positive scrolls down) and/or dx".into());
    }
    let frames = vec![
        vec![Input::Move { x, y }],
        vec![Input::Scroll {
            // iced's wheel delta is positive when content should move down
            // (scrolling up); our dy is the reading direction.
            x: -dx,
            y: -dy,
            unit: p.unit.unwrap_or_default(),
        }],
    ];
    act(
        app,
        p.window.as_deref(),
        frames,
        &format!("Scrolled ({dx}, {dy}) over {label}"),
        p.snapshot.unwrap_or(false),
    )
    .await
}

pub(crate) async fn drag(app: &mut App, p: &DragParams) -> Result<String, String> {
    let from = resolve(app, p.window.as_deref(), &p.target).await?;
    let (to_x, to_y) = match (p.to_x, p.to_y, p.dx, p.dy) {
        (Some(x), Some(y), _, _) => (x, y),
        (_, _, dx, dy) if dx.is_some() || dy.is_some() => {
            (from.x + dx.unwrap_or(0.0), from.y + dy.unwrap_or(0.0))
        }
        _ => return Err("give to_x and to_y, or dx/dy".into()),
    };
    let button = p.button.unwrap_or_default();
    let steps = p.steps.unwrap_or(12).clamp(1, 200);
    let mut frames = vec![
        vec![Input::Move {
            x: from.x,
            y: from.y,
        }],
        vec![Input::Press { button }],
    ];
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        frames.push(vec![Input::Move {
            x: from.x + (to_x - from.x) * t,
            y: from.y + (to_y - from.y) * t,
        }]);
    }
    frames.push(vec![Input::Release { button }]);
    let frames = hold(modifiers(p.modifiers.as_deref())?, frames);
    act(
        app,
        p.window.as_deref(),
        frames,
        &format!("Dragged from {} to ({to_x:.0},{to_y:.0})", from.label),
        p.snapshot.unwrap_or(false),
    )
    .await
}

pub(crate) async fn hover(app: &mut App, p: &HoverParams) -> Result<String, String> {
    let aim = resolve(app, p.window.as_deref(), &p.target).await?;
    request(
        app,
        Request::Act(ActParams {
            window: p.window.clone(),
            frames: vec![vec![Input::Move { x: aim.x, y: aim.y }]],
        }),
    )
    .await?;
    let response = request(
        app,
        Request::Probe(ProbeParams {
            window: p.window.clone(),
            x: aim.x,
            y: aim.y,
        }),
    )
    .await?;
    let Response::Probe(probe) = response else {
        return Err("unexpected reply to probe".into());
    };
    Ok(format!(
        "Hovering {} — cursor: {} ({:?} layer)",
        aim.label, probe.interaction, probe.layer
    ))
}

pub(crate) async fn window_size(app: &mut App, window: Option<&str>) -> Result<(f32, f32), String> {
    let key = window_key(app, window);
    if let Some(snapshot) = app.snapshots.get(&key) {
        return Ok((snapshot.size.width, snapshot.size.height));
    }
    let snapshot = take_snapshot(app, window).await?;
    Ok((snapshot.size.width, snapshot.size.height))
}

/// The logical area a screenshot covers: a target's box with a margin, a
/// given region, or the whole window (`None`).
pub(crate) async fn shot_region(
    app: &mut App,
    window: Option<&str>,
    target: &Target,
    region: Option<Rect>,
) -> Result<Option<Rect>, String> {
    Ok(if target.is_empty() {
        region
    } else {
        let aim = resolve(app, window, target).await?;
        const MARGIN: f32 = 8.0;
        Some(match aim.bounds {
            Some(b) => Rect {
                x: b.x - MARGIN,
                y: b.y - MARGIN,
                width: b.width + 2.0 * MARGIN,
                height: b.height + 2.0 * MARGIN,
            },
            None => Rect {
                x: aim.x - 100.0,
                y: aim.y - 100.0,
                width: 200.0,
                height: 200.0,
            },
        })
    })
}

pub(crate) async fn screenshot(
    app: &mut App,
    p: &ScreenshotToolParams,
) -> Result<(Option<String>, String), String> {
    let target_bounds = if p.highlight {
        if p.target.is_empty() {
            return Err("highlight needs a target: give text, id, ref, or x and y".into());
        }
        let aim = resolve(app, p.window.as_deref(), &p.target).await?;
        Some(aim.bounds.unwrap_or(Rect {
            x: aim.x - 10.0,
            y: aim.y - 10.0,
            width: 20.0,
            height: 20.0,
        }))
    } else {
        None
    };
    if let Some(bounds) = target_bounds {
        request(
            app,
            Request::Highlight(HighlightParams {
                window: p.window.clone(),
                bounds,
                duration_ms: Some(1_500),
            }),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    let region = if p.highlight {
        p.region
    } else {
        shot_region(app, p.window.as_deref(), &p.target, p.region).await?
    };
    let response = request(
        app,
        Request::Screenshot(ScreenshotParams {
            window: p.window.clone(),
            region,
            max_width: Some(p.max_width.unwrap_or(1280)),
        }),
    )
    .await?;
    let Response::Screenshot(shot) = response else {
        return Err("unexpected reply to screenshot".into());
    };
    let mut text = format!(
        "{}×{} PNG of logical region {}. Logical point = region origin + pixel × {:.3}.",
        shot.width,
        shot.height,
        view::rect_text(&shot.region),
        shot.logical_per_pixel
    );
    match &p.save_to {
        Some(path) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&shot.png_base64)
                .map_err(|e| format!("bad image from the app: {e}"))?;
            let path = PathBuf::from(path);
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(&path, bytes)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            let _ = write!(text, " Saved to {}.", path.display());
            Ok((None, text))
        }
        None => Ok((Some(shot.png_base64), text)),
    }
}

pub(crate) async fn highlight(app: &mut App, p: &HighlightToolParams) -> Result<String, String> {
    let aim = resolve(app, p.window.as_deref(), &p.target).await?;
    let bounds = aim.bounds.unwrap_or(Rect {
        x: aim.x - 10.0,
        y: aim.y - 10.0,
        width: 20.0,
        height: 20.0,
    });
    request(
        app,
        Request::Highlight(HighlightParams {
            window: p.window.clone(),
            bounds,
            duration_ms: p.duration_ms,
        }),
    )
    .await?;
    Ok(format!(
        "Highlighted {} at ({:.0},{:.0} {:.0}×{:.0}) for up to {} ms.",
        aim.label,
        bounds.x,
        bounds.y,
        bounds.width,
        bounds.height,
        p.duration_ms.unwrap_or(1_500).clamp(100, 5_000)
    ))
}

pub(crate) async fn wait_for(app: &mut App, p: &WaitParams) -> Result<String, String> {
    let selector = match (&p.id, &p.text) {
        (Some(id), _) => Selector::Id(id),
        (None, Some(text)) => Selector::Text {
            text,
            exact: p.exact.unwrap_or(false),
        },
        (None, None) => return Err("give text or id".into()),
    };
    let gone = p.gone.unwrap_or(false);
    let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(10_000).min(120_000));
    let started = Instant::now();
    loop {
        let snapshot = take_snapshot(app, p.window.as_deref()).await?;
        let found = view::find(&snapshot, &selector);
        let done = found.is_empty() == gone;
        if done {
            let elapsed = started.elapsed().as_millis();
            return Ok(match found.first() {
                Some(node) if !gone => format!("Appeared after {elapsed} ms: {}", view::line(node)),
                _ => format!("Gone after {elapsed} ms."),
            });
        }
        if started.elapsed() >= timeout {
            let state = if gone {
                "still there"
            } else {
                "never appeared"
            };
            let texts = view::visible_texts(&snapshot, 20);
            return Err(format!(
                "{state} after {} ms. Visible text: {texts:?}",
                timeout.as_millis()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub(crate) fn status_text(name: &str, conn: &Conn, launched: Option<&Launched>) -> String {
    let info = &conn.info;
    let mut text = format!(
        "[{name}] connected to {} ({}, pid {}) on port {}.",
        info.name, info.toolkit, info.pid, conn.port
    );
    if !info.messages {
        text.push_str(" Message log off.");
    }
    for window in &info.windows {
        let _ = write!(
            text,
            "\n  window {} {:?} {:.0}×{:.0}",
            window.id, window.title, window.size.width, window.size.height
        );
    }
    if let Some(launched) = launched {
        let _ = write!(text, "\nLaunched: `{}`", launched.command);
    }
    text
}

/// Splits a command line on whitespace, honouring "double" and 'single' quotes.
pub(crate) fn split_command(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut in_word = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

// ------------------------------------------------------------------ settling

pub(crate) struct Settled {
    pub(crate) settled: bool,
    pub(crate) waited: Duration,
    pub(crate) polls: u32,
}

/// Polls the widget tree until its [`view::signature`] holds for `stable`,
/// or `timeout` passes. Not settling is an answer, not an error (Argent's
/// `await-screen-idle`).
pub(crate) async fn settle(
    app: &mut App,
    window: Option<&str>,
    stable: Duration,
    timeout: Duration,
) -> Result<Settled, String> {
    let started = Instant::now();
    let mut last: Option<String> = None;
    let mut since = Instant::now();
    let mut polls = 0;
    loop {
        let snapshot = raw_snapshot(app, window, None).await?;
        polls += 1;
        let signature = view::signature(&snapshot);
        if last.as_ref() != Some(&signature) {
            last = Some(signature);
            since = Instant::now();
        } else if since.elapsed() >= stable {
            return Ok(Settled {
                settled: true,
                waited: started.elapsed(),
                polls,
            });
        }
        if started.elapsed() >= timeout {
            return Ok(Settled {
                settled: false,
                waited: started.elapsed(),
                polls,
            });
        }
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

pub(crate) async fn wait_idle(app: &mut App, p: &WaitIdleParams) -> Result<String, String> {
    let stable = Duration::from_millis(p.stable_ms.unwrap_or(300).min(10_000));
    let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(5000).min(120_000));
    let s = settle(app, p.window.as_deref(), stable, timeout).await?;
    Ok(if s.settled {
        format!(
            "Settled after {} ms ({} polls).",
            s.waited.as_millis(),
            s.polls
        )
    } else {
        format!(
            "Still changing after {} ms ({} polls): something animates or moves. Use wait_for on the element you need instead.",
            s.waited.as_millis(),
            s.polls
        )
    })
}

// ------------------------------------------------------------ app internals

pub(crate) async fn paste(app: &mut App, p: &PasteParams) -> Result<String, String> {
    request(
        app,
        Request::Clipboard(ClipboardParams {
            write: Some(p.text.clone()),
        }),
    )
    .await?;
    let mut frames = Vec::new();
    let mut what = String::from("Pasted");
    if !p.target.is_empty() {
        let aim = resolve(app, p.window.as_deref(), &p.target).await?;
        frames.push(vec![Input::Move { x: aim.x, y: aim.y }]);
        frames.push(vec![
            Input::Press {
                button: MouseButton::Left,
            },
            Input::Release {
                button: MouseButton::Left,
            },
        ]);
        what = format!("Focused {} and pasted", aim.label);
    }
    let (m, key) = keys::chord("Mod+v")?;
    frames.extend(keys::press(m, &key));
    let shown: String = p.text.chars().take(80).collect();
    act(
        app,
        p.window.as_deref(),
        frames,
        &format!("{what} {shown:?}"),
        false,
    )
    .await
}

pub(crate) async fn clipboard(app: &mut App, p: &ClipboardToolParams) -> Result<String, String> {
    match request(
        app,
        Request::Clipboard(ClipboardParams {
            write: p.write.clone(),
        }),
    )
    .await?
    {
        Response::Clipboard(content) => Ok(match (content.text, &p.write) {
            (Some(text), Some(_)) => format!("Clipboard set ({} chars).", text.chars().count()),
            (Some(text), None) => text,
            (None, _) => "(the clipboard is empty or holds no text)".into(),
        }),
        _ => Err("unexpected reply to clipboard".into()),
    }
}

pub(crate) async fn state(app: &mut App, p: &StateToolParams) -> Result<String, String> {
    match request(
        app,
        Request::State(StateParams {
            filter: p.filter.clone(),
            max_chars: p.max_chars,
        }),
    )
    .await?
    {
        Response::State(dump) => {
            let mut text = dump.text;
            if text.is_empty() {
                text = "(nothing matches)".into();
            }
            if dump.truncated {
                let _ = write!(
                    text,
                    "\n… cut ({} chars in all): narrow with `filter`, or raise `max_chars`.",
                    dump.total_chars
                );
            }
            Ok(text)
        }
        _ => Err("unexpected reply to state".into()),
    }
}

pub(crate) async fn perf(app: &mut App, p: &PerfToolParams) -> Result<String, String> {
    let Response::Perf(r) = request(
        app,
        Request::Perf(PerfParams {
            reset: p.reset.unwrap_or(false),
        }),
    )
    .await?
    else {
        return Err("unexpected reply to perf".into());
    };
    let ms = |us: u64| us as f64 / 1000.0;
    let fps = if r.frames.mean_us > 0 {
        1_000_000.0 / r.frames.mean_us as f64
    } else {
        0.0
    };
    let mut text = format!(
        "Over {:.1}s:\n\
         frames  {} — {fps:.0} fps while animating; interval mean {:.1} ms, p95 {:.1} ms, worst {:.1} ms\n\
         view    {} calls — mean {:.2} ms, p95 {:.2} ms, worst {:.2} ms\n\
         update  {} messages — mean {:.2} ms, p95 {:.2} ms, worst {:.2} ms\n",
        r.span_ms as f64 / 1000.0,
        r.frames.count,
        ms(r.frames.mean_us),
        ms(r.frames.p95_us),
        ms(r.frames.max_us),
        r.view.count,
        ms(r.view.mean_us),
        ms(r.view.p95_us),
        ms(r.view.max_us),
        r.update.count,
        ms(r.update.mean_us),
        ms(r.update.p95_us),
        ms(r.update.max_us),
    );
    if !r.slowest.is_empty() {
        text.push_str("costliest messages (total update time):\n");
        for k in &r.slowest {
            let _ = writeln!(
                text,
                "  {:<40} ×{:<6} total {:>8.2} ms  worst {:>7.2} ms",
                k.kind,
                k.count,
                ms(k.total_us),
                ms(k.max_us)
            );
        }
    }
    text.push_str("A frame interval over 16.7 ms is a dropped frame at 60 Hz; view and update both run on the UI thread.");
    if p.reset.unwrap_or(false) {
        text.push_str(" Counters reset.");
    }
    Ok(text)
}

pub(crate) async fn inspect_point(app: &mut App, p: &PointParams) -> Result<String, String> {
    let snapshot = take_snapshot(app, p.window.as_deref()).await?;
    let hits = view::at_point(&snapshot, p.x, p.y);
    let probe = match request(
        app,
        Request::Probe(ProbeParams {
            window: p.window.clone(),
            x: p.x,
            y: p.y,
        }),
    )
    .await?
    {
        Response::Probe(probe) => probe,
        _ => return Err("unexpected reply to probe".into()),
    };
    let mut text = format!(
        "At ({:.0},{:.0}): cursor {} ({:?} layer).\n",
        p.x, p.y, probe.interaction, probe.layer
    );
    if hits.is_empty() {
        text.push_str(
            "No reported widget covers this point: it is custom-drawn (a canvas) or empty.",
        );
    } else {
        text.push_str("Covering it, innermost first:\n");
        // Nested layout boxes often share one rectangle: list each area once,
        // unless the node has something of its own to show.
        let mut shown: Vec<Rect> = Vec::new();
        for node in hits {
            let plain = node.id.is_none() && node.text.is_none() && node.interaction.is_none();
            let area = node.visible.unwrap_or(node.bounds);
            if plain && shown.contains(&area) {
                continue;
            }
            shown.push(area);
            let _ = writeln!(text, "  {}", view::line(node));
            if shown.len() == 8 {
                break;
            }
        }
    }
    Ok(text)
}

pub(crate) async fn audit(app: &mut App, p: &AuditParams) -> Result<String, String> {
    let snapshot = take_snapshot(app, p.window.as_deref()).await?;
    let findings = view::audit_findings(&snapshot);
    let visible_count = snapshot
        .nodes
        .iter()
        .filter(|node| node.visible.is_some())
        .count();
    if findings.is_empty() {
        return Ok(format!(
            "No semantic gaps detected among {visible_count} visible nodes. This checks names and reported control state; it does not prove every widget has complete semantics."
        ));
    }
    let mut text = format!(
        "{} semantic gap(s) across {visible_count} visible nodes:\n",
        findings.len()
    );
    for (node, issue) in findings {
        let _ = writeln!(text, "  [{:?}] {issue} — {}", node.layer, view::line(node));
    }
    text.push_str(
        "Fix: add a human-readable label, report the correct semantic role and current state, \
         or replace approximate hit-test regions with semantic nodes (see the iced-mcp README).",
    );
    Ok(text)
}

// --------------------------------------------------------------- app actions

/// Complete machine-readable metadata for registered app actions.
pub(crate) async fn app_actions_value(app: &mut App) -> Result<serde_json::Value, String> {
    match request(app, Request::Actions).await? {
        Response::Actions(list) => Ok(serde_json::json!({
            "actions": list.actions.into_iter().map(|a| {
                let kind = match a.kind {
                    ActionKind::Action => "action",
                    ActionKind::Query => "query",
                };
                let mut value = serde_json::json!({
                    "name": a.name,
                    "kind": kind,
                    "description": a.description,
                });
                if let Some(schema) = a.output_schema {
                    value["outputSchema"] = schema;
                }
                value
            }).collect::<Vec<_>>()
        })),
        _ => Err("unexpected reply to actions".into()),
    }
}

pub(crate) async fn run_app_action(app: &mut App, p: &AppActionParams) -> Result<String, String> {
    let before = read_messages(app, Some(u64::MAX), None, Some(1))
        .await
        .ok()
        .map(|log| log.last);
    request(
        app,
        Request::Action(ActionParams {
            name: p.name.clone(),
            args: p.args.clone(),
        }),
    )
    .await?;
    let _ = settle(app, None, ACTION_STABLE, ACTION_SETTLE_CAP).await;
    let mut text = format!("Ran action {}.", p.name);
    if let Some(since) = before {
        let log = read_messages(app, Some(since), None, Some(200)).await?;
        if !log.entries.is_empty() {
            let _ = write!(
                text,
                "\n{} message(s):\n{}",
                log.entries.len(),
                summarize_messages(&log, 16)
            );
        }
    }
    Ok(text)
}

/// Waits for a message containing `contains` — the assertion that the app
/// did something, not just showed it.
pub(crate) async fn expect_message(
    app: &mut App,
    p: &ExpectMessageParams,
) -> Result<String, String> {
    let since = match p.since {
        Some(since) => since,
        None => {
            read_messages(app, Some(u64::MAX), None, Some(1))
                .await?
                .last
        }
    };
    let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(3000).min(120_000));
    let started = Instant::now();
    loop {
        let log = read_messages(app, Some(since), Some(p.contains.clone()), Some(1)).await?;
        if let Some(entry) = log.entries.first() {
            return Ok(format!(
                "Got #{} after {} ms: {}",
                entry.seq,
                started.elapsed().as_millis(),
                entry.text.chars().take(200).collect::<String>()
            ));
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "no message containing {:?} within {} ms",
                p.contains,
                timeout.as_millis()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ------------------------------------------------------------ visual diffing

pub(crate) struct DiffOutcome {
    pub(crate) text: String,
    pub(crate) mismatch: f64,
    /// A small diff image to show inline, when something changed.
    pub(crate) context_png: Option<Vec<u8>>,
}

fn baselines(root: &Path) -> PathBuf {
    root.join(".iced-mcp").join("baselines")
}

pub(crate) async fn screenshot_diff(
    app: &mut App,
    ctx: &Ctx,
    p: &DiffParams,
) -> Result<DiffOutcome, String> {
    flow::check_name(&p.baseline).map_err(|e| e.replace("flow name", "baseline name"))?;
    // Full resolution: a diff of a downscaled frame hides small changes.
    let region = shot_region(app, p.window.as_deref(), &p.target, p.region).await?;
    let Response::Screenshot(shot) = request(
        app,
        Request::Screenshot(ScreenshotParams {
            window: p.window.clone(),
            region,
            max_width: Some(u32::MAX),
        }),
    )
    .await?
    else {
        return Err("unexpected reply to screenshot".into());
    };
    let png = base64::engine::general_purpose::STANDARD
        .decode(&shot.png_base64)
        .map_err(|e| format!("bad image from the app: {e}"))?;
    let dir = baselines(&ctx.root);
    let base_path = dir.join(format!("{}.png", p.baseline));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    if p.update.unwrap_or(false) || ctx.update_baselines || !base_path.exists() {
        let existed = base_path.exists();
        std::fs::write(&base_path, &png)
            .map_err(|e| format!("cannot write {}: {e}", base_path.display()))?;
        let verb = if existed { "Updated" } else { "Created" };
        return Ok(DiffOutcome {
            text: format!("{verb} baseline {}.", base_path.display()),
            mismatch: 0.0,
            context_png: None,
        });
    }

    let baseline_bytes = std::fs::read(&base_path)
        .map_err(|e| format!("cannot read {}: {e}", base_path.display()))?;
    let baseline = diff::decode(&baseline_bytes)?;
    let current = diff::decode(&png)?;
    let result = diff::compare(&baseline, &current, p.tolerance.unwrap_or(0.1));
    let mismatch = result.mismatch_percent();
    let mut text = String::from("Screenshot diff\n");
    let mut context_png = None;
    match result.status {
        diff::Status::DimensionMismatch => {
            let _ = write!(
                text,
                "status: dimension_mismatch — baseline {}×{}, now {}×{}. The window was resized or the scale changed: compare at the same size, or update the baseline.",
                baseline.width, baseline.height, current.width, current.height
            );
            return Ok(DiffOutcome {
                text,
                mismatch: 100.0,
                context_png: None,
            });
        }
        diff::Status::Unchanged => {
            text.push_str("status: unchanged (0 pixels differ)");
        }
        diff::Status::Changed => {
            let _ = writeln!(
                text,
                "status: changed\npixel_mismatch: {mismatch:.3}% — {} visual change\nchanged_areas: shown={} total={}",
                result.severity(),
                result.regions.len().min(diff::MAX_REGIONS),
                result.regions.len()
            );
            for (i, r) in result.regions.iter().take(diff::MAX_REGIONS).enumerate() {
                // In logical coordinates, so the agent can inspect or click it.
                let k = shot.logical_per_pixel;
                let logical = Rect {
                    x: shot.region.x + r.x as f32 * k,
                    y: shot.region.y + r.y as f32 * k,
                    width: r.width as f32 * k,
                    height: r.height as f32 * k,
                };
                let _ = writeln!(
                    text,
                    "  region {}: {} changed_pixels={}",
                    i + 1,
                    view::rect_text(&logical),
                    r.changed
                );
            }
            if let Some(diff_image) = &result.diff {
                let diffs = ctx.root.join(".iced-mcp").join("diffs");
                let _ = std::fs::create_dir_all(&diffs);
                let full = diffs.join(format!("{}-diff.png", p.baseline));
                if let Ok(bytes) = diff::encode(diff_image) {
                    let _ = std::fs::write(&full, bytes);
                    let _ = write!(
                        text,
                        "Diff image (green brighter, red darker, yellow boxes): {}",
                        full.display()
                    );
                }
                context_png = diff::encode(&diff::shrink(diff_image, 640)).ok();
            }
        }
    }
    Ok(DiffOutcome {
        text,
        mismatch,
        context_png,
    })
}

// -------------------------------------------------------------------- queries

/// Asks one of the app's queries; the answer is JSON.
pub(crate) async fn query(
    app: &mut App,
    name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match request(
        app,
        Request::Query(ActionParams {
            name: name.to_owned(),
            args: args.clone(),
        }),
    )
    .await?
    {
        Response::Value(answer) => Ok(answer.value),
        _ => Err("unexpected reply to query".into()),
    }
}

/// A query's answer as pretty JSON, cut at `max_chars`.
pub(crate) async fn query_text(app: &mut App, p: &QueryParams) -> Result<String, String> {
    let value = query(app, &p.name, &p.args).await?;
    Ok(clip_json(&value, p.max_chars.unwrap_or(8000)))
}

pub(crate) fn clip_json(value: &serde_json::Value, max_chars: usize) -> String {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    let total = text.chars().count();
    if total <= max_chars {
        return text;
    }
    let mut cut: String = text.chars().take(max_chars).collect();
    let _ = write!(
        cut,
        "\n… cut ({total} chars): pass args that narrow it, or raise max_chars"
    );
    cut
}

/// Asks a query until its answer contains `contains` — an assertion on the
/// app's model rather than its pixels.
pub(crate) async fn expect_query(app: &mut App, p: &ExpectQueryParams) -> Result<String, String> {
    let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(3000).min(120_000));
    let needle = p.contains.to_lowercase();
    let started = Instant::now();
    loop {
        let value = query(app, &p.name, &p.args).await?;
        let text = value.to_string();
        if text.to_lowercase().contains(&needle) {
            return Ok(format!(
                "{} contains {:?} after {} ms",
                p.name,
                p.contains,
                started.elapsed().as_millis()
            ));
        }
        if started.elapsed() >= timeout {
            let shown: String = text.chars().take(300).collect();
            return Err(format!(
                "{} never contained {:?} within {} ms; last answer: {shown}",
                p.name,
                p.contains,
                timeout.as_millis()
            ));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_like_a_shell() {
        assert_eq!(
            split_command(r#"cargo run -p "my app" -- --name 'a b'"#),
            ["cargo", "run", "-p", "my app", "--", "--name", "a b"]
        );
        assert_eq!(split_command(r#"x """#), ["x", ""]);
    }

    #[test]
    fn targets_parse_with_ref_and_flattened_into_tools() {
        let click: ClickParams = serde_json::from_value(serde_json::json!({
            "ref": 12, "count": 2, "modifiers": "Shift"
        }))
        .unwrap();
        assert_eq!(click.target.reference, Some(12));
        assert_eq!(click.count, Some(2));
        assert!(modifiers(click.modifiers.as_deref()).unwrap().shift);
    }

    #[test]
    fn action_replies_group_repeated_message_kinds() {
        use iced_mcp_protocol::MessageEntry;
        let entry = |seq, text: &str| MessageEntry {
            seq,
            at_ms: 0,
            text: text.into(),
        };
        let log = MessageLog {
            entries: vec![
                entry(1, "SaveTick"),
                entry(2, "Chart(Book(W(\"a\"), [71889.6]))"),
                entry(3, "Dock(Clicked(Pane(0)))"),
                entry(4, "Chart(Book(W(\"a\"), [71886.2]))"),
                entry(5, "SaveTick"),
                entry(6, "Chart(History(W))"),
            ],
            last: 6,
            dropped: 0,
        };
        let text = summarize_messages(&log, 10);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(lines[0].ends_with("SaveTick ×2"));
        assert!(lines[1].contains("Chart(Book") && lines[1].ends_with("×2"));
        assert!(lines[3].contains("Chart(History"));
        assert_ne!(
            message_kind("Chart(Drawing(W(\"a\"), Hover(Group(Lines), true)))"),
            message_kind("Chart(Drawing(W(\"a\"), Opened(Group(Lines), true)))")
        );
    }
}
