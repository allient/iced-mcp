# iced-mcp guide

The full reference: every tool, the hooks an app can add, how it works,
regression testing in CI, limits and releasing. Start with the
[README](../README.md) for the five-minute setup.

## Name icon-only controls

A button that draws only a glyph reports nothing an agent can read, so it
shows as a bare `box … pointer`. Give it an accessible name — the
`aria-label` of iced — with **no dependency on this crate**: in a wrapper
widget's `operate`, report a custom operation whose state is a `String`:

```rust
fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer,
           operation: &mut dyn Operation) {
    let mut label = self.label.clone();               // e.g. "Close"
    operation.custom(None, layout.bounds(), &mut label);
    self.content.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
}
```

The snapshot then lists `label "Close" … pointer` in place of the box, and
`click {text: "Close"}` finds it. Other operations ignore a `String` custom
state, so `iced_test` selectors and focus handling are unchanged. A tooltip
wrapper can do the same with its tooltip text, so every control with a
tooltip gets a name for free.

## Tools

| Group | Tools | |
|---|---|---|
| Session | `launch` · `connect` · `disconnect` · `restart` · `status` · `setup_help` | Several apps at once: each has a `name`, tools take `app`, the last connected is the default. `launch` reports build/start failures with the app's last output; `restart` relaunches with the same command; `setup_help` explains adding the bridge |
| Look | `snapshot` · `accessibility_tree` · `highlight` · `screenshot` · `inspect_point` · `audit` · `diagnose` | Screen outline; role/name/value and AccessKit-compatible tree for widgets and canvas semantics, with per-window coverage counts; temporary live target outline; PNG of a window/node/region, optionally highlighted; point inspection; semantic gaps; layout findings (see [Layout diagnostics](#layout-diagnostics)) |
| Act | `click` · `type_text` · `paste` · `press_key` · `scroll` · `drag` · `hover` · `window` | Targets: `id`, `text` (substring or `exact`), `ref`, or `x`/`y`. Every action **waits for the screen to settle** and replies with the **messages the app published** |
| Wait | `wait_for` · `wait_idle` | Until text/id appears or disappears; until the screen stops changing |
| App side | `messages` · `logs` · `state` · `perf` · `clipboard` · `app_actions` · `run_app_action` · `query` · `fixtures` · `load_fixture` | Message log; stdout/stderr with a cursor and character budget; the app's state; frame/view/update timings; clipboard; actions and read-only queries the app registered (`app_actions` returns complete output schemas in structured content; queries answer JSON, also as MCP `structuredContent`); the app's named starting states (see [Fixtures](#fixtures)) |
| Regression | `screenshot_diff` · `flow_start` · `flow_add_step` · `flow_finish` · `flow_run` | Visual baselines; record a session as a flow and replay it as a test |
| Batch & video | `run_sequence` · `record_start` · `record_stop` | Several steps in one call; screen recording to mp4 |

`run_sequence`, `flow_add_step` and flows share one step vocabulary: `click`,
`type_text`, `press_key`, `scroll`, `drag`, `hover`, `paste`, `wait_for`,
`wait_idle`, `sleep`, `run_app_action`, `load_fixture`, `query`, `expect_query` (an assertion
on a query's JSON), `expect_message` (an assertion that a message containing
some text arrives), `screenshot_diff`, `expect_no_diagnostics` (an assertion
that `diagnose` finds nothing, optionally of some `kinds`) and `echo`.

A snapshot looks like:

```
window 1 "Todos" 1024×768 — 10 of 17 nodes
[3] input "What needs doing?" #new-todo (20,68 926×31) text focused
[5] text "Add" (964,73 30×21) pointer
[11] scroll #todo-list (28,162 968×220) scroll(0,0 of 968×220)
  [13] text "one" (28,162 52×21) pointer
[16] region (72,112 80×32) pointer
[17] region (72,144 80×88) pointer overlay
```

`pointer`/`text`/`grab`… is the cursor the app shows there — the clickability
hint. `overlay` marks menus, dropdowns and popups. `label` is an icon-only
control's accessible name (above). `region` is an interactive area no widget
described (see below). Action replies group repeated messages
(`SaveTick ×3`) and shorten long payloads; `messages` has them in full.

## Describe custom-drawn content (canvas semantics)

A chart, heatmap or 3D view is one widget to the tree, so `snapshot` sees one
box. A widget can describe what it drew — the accessibility tree of a canvas —
again with no dependency on this crate: in its `operate`, report a custom
operation whose state is `(&'static str, String)`:

```rust
let json = r#"{"role":"bar","name":"BTCUSDT M1 12:45","value":"O 71856 H 71930 L 71837 C 71890",
              "id":"bar-1767225900","bounds":{"x":1043,"y":324,"width":12,"height":44},"children":[]}"#;
operation.custom(self.id.as_ref(), layout.bounds(), &mut ("iced-mcp/semantics@1", json.to_owned()));
// or with the typed helper: SemanticNode::payload(&nodes) from iced-mcp-protocol
```

- **Format:** one node or an array. Each node has `role`, `name`, optional
  `value` (text or any JSON), `description`, `placeholder`, `disabled`,
  `checked`, `indeterminate`, `selected`, `expanded`, optional `id`, `bounds` (logical window
  pixels) and optional `children`. Omitted state means unknown. It is specified
  by a JSON Schema (draft 2020-12),
  [`iced-mcp-protocol/schema/semantics.v1.json`](../iced-mcp-protocol/schema/semantics.v1.json),
  generated from the Rust types and checked by a test.
- **Versioning:** the tag carries the version (`iced-mcp/semantics@1`; the bare
  tag reads as 1), and a root may also say `"v": 1`. A newer version shows up
  as an `error` node instead of being misread.
- **Roles:** use an ARIA/AccessKit role where one fits (`button`, `checkbox`,
  `switch`, `radio`, `tab`, `cell`, `slider`, `spinbutton`, `img`…) and a domain role otherwise (`chart`, `bar`, `axis`,
  `drawing`, `handle`, `profile`, `heatmap`…; the list is `ROLES` in the
  protocol). Unknown roles pass through.
- **Agents see** `[193] bar "BTCUSDT M1 06:41 PM" = O 71746.2 H … #bar-1767224460 (1043,324 12×44)`
  indented under the widget, at most 20 per role per parent with an
  "… N more" line (`snapshot` with `query` lists them all). `click`,
  `wait_for`, `inspect_point` and flows work on them by name. Settling ignores
  them, so a live chart still settles. Clickable widgets win over drawn
  content with the same text.
- `accessibility_action` dispatches supported actions by numeric `node_id` and
  `snapshotId` from `accessibility_tree`: `click`, `focus`, `expand`, `collapse`, `scroll_up`,
  `scroll_down`, `replace_selected_text`, `set_value`, `increment` and
  `decrement`. `set_value` takes a number for semantic sliders and numeric
  inputs, `true`/`false` for switches and checkboxes, the exact enabled option
  label for a semantic select, the format reported by a semantic time field
  (such as `13:45` or `01:45 PM`), `YYYY-MM-DD` for a single date, or
  a hexadecimal color string, or `YYYY-MM-DD/YYYY-MM-DD` for an inclusive date range. Numeric
  increment/decrement use the reported step. Actions use current
  visible geometry and send normal synthetic input through
  the app. Focus and text selection
  use a direct iced widget operation when the widget has a stable ID; other
  actions use pointer/key input.
- `audit` checks visible nodes for missing names, roles, toggle/selection
  state, numeric value/step and slider range/track, select options, calendar
  month, custom-widget semantic children and approximate hit-test regions. A
  clean result is a diagnostic, not proof of complete widget coverage.
- Describe what is visible, not the whole data set; the bridge keeps at most
  2000 nodes per widget.

## Layout diagnostics

`diagnose` checks the current screen for layout mistakes and lists each
finding with its kind, the nodes' `[ref]`s and ids, their boxes and a short
explanation (also as JSON in `structuredContent`). `kinds` narrows it to some
of them. It is a separate tool from `audit`: `audit` asks whether widgets
*say* what they are (names, roles, state); `diagnose` asks whether they are
*laid out* right, and its findings are what `expect_no_diagnostics` fails on.

| Kind | What it reports | How |
|---|---|---|
| `overlap` | Two interactive widgets covering the same pixels | Visible leaves that show a cursor (`pointer`, `text`…), whose boxes intersect by more than 1 px both ways. Skipped: a widget and its own children, `label` nodes (they name the control under them), `region`s (approximate), drawn semantic content, and an overlay over the base (menus are meant to cover it) |
| `clipped` | Content sticking out of its container or the window, or squeezed to nothing | A node whose box crosses its nearest reported container by more than 1 px; a node whose visible part crosses the window edge (only the outermost one); an input, focusable, label or unmeasured text that the layout shrank to zero width or height. Scrolled content is not clipping: children of a scrollable are its content, and what is scrolled out of view is hidden, not clipped |
| `text_overflow` | Text that needs more room than its widget had | The bridge reads the paragraph iced's `text` widget lays out and compares its minimum size with the space it was given (`text_fit` on the node), with half a pixel of slack. Catches `Wrapping::None` text cut off by a narrow box, a word longer than the line, and text in a fixed or squeezed height |
| `duplicate_id` | The same widget id on two different widgets | Nodes of one window with the same `#id` and different boxes (a widget that reports itself twice with one box, like a text input, counts once). Hidden nodes count: `#id` targets and widget operations still reach only one |

Limits, honestly:

- iced's layout clamps most children to the space they are given, so a
  widget rarely sticks out of its container: content that does not fit is
  usually *squeezed* (reported) or, for text, overflows its paragraph
  (reported). `float`/translated content and custom widgets that position
  children themselves are where `clipped` finds real crossings.
- Text is measured only for iced's `text` widget (including the labels of
  buttons and other widgets built on it) in the base layer. Text inputs,
  rich text, text in overlays and text drawn by custom widgets or canvases
  have no `text_fit`: unknown, never reported as fitting.
- A text input squeezed to less than its text needs, but not to zero, is
  not found.
- Overlap uses the cursor hint, so a clickable widget that shows the default
  arrow is not considered interactive, and two controls whose padding
  overlaps but whose labels do not are not reported.
- A clean result means none of these rules fired on what the widget tree
  reports; it is not proof the layout is right. Pair it with
  `screenshot_diff` for what only pixels show.

## Queries

```rust
iced_mcp::attach(app)
    .query("bar_at", "The bar under a point. Args: {\"x\": f32, \"y\": f32}.", |state, args| {
        let x = args["x"].as_f64().ok_or("needs x")?;
        Ok(serde_json::to_value(state.chart.bar_at(x)).map_err(|e| e.to_string())?)
    })
     // or .query_with_schema(name, description, schema, f), where `schema` is
     // the answer's JSON Schema (e.g. from schemars), exposed by app_actions
    .run()
```

A query reads the state on the UI thread between frames and changes nothing.
Agents call it with `query {name, args}` and get JSON back. Flows assert on it
with `expect_query {name, args, contains}`.

## Regression testing

**Flows** turn an agent session into a test. `flow_start {name}` begins
recording: every successful action on that app is saved with the arguments it
was called with (by id/text, not coordinates, so it survives layout changes).
Add assertions with `flow_add_step` (`expect_message`, `wait_for`,
`screenshot_diff`), then `flow_finish` writes `.iced-mcp/flows/<name>.json`.
`flow_run {name}` replays it: one line per step and `PASS|FAIL`, stopping at the
first failure (the rest are skipped). A flow recorded on an app started with
`launch` is end to end — the replay launches a fresh copy with the same command
and stops it afterwards; otherwise it is a fragment with a stated
`prerequisite` that runs on the connected app.

**Visual baselines.** `screenshot_diff {baseline}` saves
`.iced-mcp/baselines/<name>.png` the first time and compares later: the share of
pixels that differ, each changed region in logical coordinates (so you can
`inspect_point` or click it), and a diff image (green brighter, red darker,
yellow boxes). In a flow it fails above `max_mismatch` (default 0.5%);
`flow_run {update_baselines: true}` re-records them.

Flow files follow a JSON Schema (draft 2020-12),
[`crates/iced-mcp-server/schema/flow.v1.json`](../crates/iced-mcp-server/schema/flow.v1.json),
so editors can validate and complete them.

**In CI**, without an agent:

```sh
iced-mcp run-flow <name> [--root DIR] [--update-baselines] [--prerequisite-ok]
# exit 0: every step passed · 1: a step failed · 2: bad arguments
```

**A whole suite** runs every flow (or the ones you pick) with one line per
flow and a summary:

```sh
iced-mcp run-suite [--root DIR] [--only GLOB]… [--repeat N | --until-fail N]
                   [--fail-fast] [--update-baselines] [--launch CMD] [--prerequisite-ok]
# PASS add-todo (2.1s)
# FAIL broken (0.7s) — [2] ✗ wait_for "never there" (0.5s) — never appeared …
#     bundle: .iced-mcp/failures/broken-20261104-192124-371
# FAIL — 1 passed, 1 failed of 2 run (2 flow(s) × 1 round(s)) in 2.8s
# exit 0: every flow passed · 1: a flow failed · 2: bad arguments, or no flow matched
```

`--only` takes `*`/`?` globs and can repeat. `--repeat N` runs the suite N
times; `--until-fail N` runs it up to N times and stops after the first round
with a failure (for hunting flaky flows). End-to-end flows launch their own
app. Fragments (no `launch`) need one: `--launch CMD` starts it once, runs
every fragment on it and relaunches it if it dies; give each fragment a
`fixture` so it does not depend on what ran before it.

**Failure bundles.** When a step fails (in `flow_run`, `run-flow` or
`run-suite`), the run captures the app while it is still up into
`.iced-mcp/failures/<flow>-<timestamp>/`, and the report names the path:

| File | |
|---|---|
| `failure.txt` | the failing step, its error, the app, when |
| `steps.log` | the run report, one line per step |
| `snapshot.txt` · `snapshot.json` | the widget tree at failure: the outline with hidden nodes, and the raw nodes |
| `diagnostics.txt` · `diagnostics.json` | layout findings at failure |
| `screenshot.png` | the window, full resolution |
| `messages.log` | the app's last 200 messages |
| `app-output.log` | its last 400 lines of stdout/stderr (apps the server launched) |
| `flow.json` | the flow that ran |

What cannot be captured (an app that crashed has no screenshot) is replaced
by `<file>.error.txt` saying why; the bundle is written anyway. Read
`failure.txt` first, then the screenshot and `snapshot.txt`.

It needs a GUI session (on macOS: a logged-in session, which GitHub's macOS
runners have). Use a prebuilt binary in the flow's launch command, record
baselines on the same runner type (scale factor and fonts change pixels), and
prefer `expect_query` / `expect_message` / `wait_for` over pixel diffs.

Commit `.iced-mcp/flows/` and `.iced-mcp/baselines/`; ignore
`.iced-mcp/diffs/`, `.iced-mcp/recordings/` and `.iced-mcp/failures/`:

```gitignore
.iced-mcp/diffs/
.iced-mcp/recordings/
.iced-mcp/failures/
```

## Settling

After every action the server polls the widget tree until it stops changing
(150 ms stable, at most 1.2 s) and says how long that took, so the reply and
the next look show the result rather than a frame in between. Digits are left
out of the comparison and boxes snapped to 4 px, so a ticking price or a clock
does not keep a live app "busy". `wait_idle` exposes the same check with your
own limits; not settling is reported, not an error.

## Sharing state and actions

```rust
iced_mcp::attach(app)
    .state(|state| format!("{state:#?}"))     // `state` tool; leave secrets out
    .action("open_settings", "Opens settings. No args.", |_| Ok(Message::OpenSettings))
    .action("add_todo", "Adds a todo. Args: {\"text\": string}.", |args| {
        let text = args.get("text").and_then(|t| t.as_str()).ok_or("needs text")?;
        Ok(Message::Add(text.to_owned()))
    })
    .run()
```

An action returns the message that performs it, which then goes through your
`update` like any other — it cannot do anything your messages cannot. Use
actions for what has no widget: opening a scene, flipping a
feature. `iced_mcp::run_with_state(app, describe)` is the one-line form for
state alone.

## Fixtures

A fixture is a named state the app can be put in — an empty cart, a
signed-in user, a chart with a year of data — so a test starts from a known
place instead of from whatever the previous one left:

```rust
iced_mcp::attach(app)
    .fixture("empty-cart", "An empty cart. No args.", |_| Ok(Shop::default()))
    .fixture("full-cart", "Args: {\"items\": n}.", |args| {
        let n = args["items"].as_u64().ok_or("needs {\"items\": n}")? as usize;
        Ok(Shop::with_items(n))
    })
    .fixture_messages("signed-out", "No user. No args.", |_| Ok(vec![Message::SignOut]))
    .run()
```

- `fixture` builds a **fresh state** and replaces the app's with it: nothing
  from before survives, which is what makes tests independent. Prefer it.
- `fixture_messages` returns the **messages** that reach the state, sent
  through `update` in order — for state that cannot be built from outside.
  They must undo whatever an earlier test left, or the fixture isolates
  nothing.

Using them:

- `fixtures` lists them (they also show in `app_actions` as kind `fixture`);
  `load_fixture {name, args}` loads one into the running app (also a
  `run_sequence`/flow step).
- `launch {command, fixture, fixture_args}` starts the app in one: the
  server sets `ICED_MCP_FIXTURE` (and `ICED_MCP_FIXTURE_ARGS`, JSON) and
  waits until the bridge reports it loaded. Any app run under
  `iced_mcp::run` honours the variables, so
  `ICED_MCP=1 ICED_MCP_FIXTURE=full-cart cargo run` works by hand too. The
  start-up fixture loads after the app's boot task finishes, so the app's
  own start-up work cannot overwrite it.
- A flow's `"fixture"` (and `"fixture_args"`) is loaded before its first
  step: an end-to-end flow starts its app in it; a fragment loads it into
  the app it runs on. A flow recorded on an app launched in a fixture keeps
  it. A fragment with a fixture needs no `prerequisite_ok`: the fixture is
  its prerequisite.

## Performance

`perf` reports the frame interval (fps while redrawing), time in `view`, and
time in `update` overall and per message kind, costliest first. `reset: true`
starts a new measurement: reset, interact, read. Both run on the UI thread, so
anything near 16 ms is a dropped frame.

## How it works

- **Attaching.** `iced_mcp::attach(program)` returns a `Program` that wraps
  yours: same state, update, view, subscriptions and theme, plus the bridge's
  messages, and runs through `iced_winit` like `Application::run` does.
- **Seeing.** Widgets describe themselves through iced's `Operation`
  (containers, scrollables, focusables, text inputs, text). The bridge walks
  base and overlay with one, computes what is scrolled out of view, and asks
  each candidate what cursor it shows. Widgets that do not implement
  `operate` (`pick_list`, sliders, canvases, many custom widgets) are found by
  **hit-testing a grid** with `mouse_interaction`: cells showing a cursor no
  reported widget accounts for merge into `region` nodes.
- **Agent semantics.** `accessibility_tree` returns a tree built with
  AccessKit's core node model: standard roles, names/values, bounds, focus,
  scroll state and supported actions. Custom-drawn chart and plot nodes keep
  their domain roles, and each node includes an MCP target hint. This helps
  agents interpret and target the UI without a native screen-reader adapter.
- **Highlighting.** `highlight` draws a temporary orange outline in the running
  window for a `text`, `id`, `ref`, or x/y target. `screenshot` can combine
  that with capture (`highlight: true`) and keeps the surrounding window
  visible.
- **Acting.** A wrapper widget around each window's view injects synthetic
  events from inside the widget tree, one frame of events per redraw, with
  iced's own routing: overlay first, base cursor hidden while the pointer is
  over the overlay, captured events not passed on. Because input never goes
  through the OS, it needs no accessibility permission, works on any
  platform, and does not move the real mouse.
- **Reacting.** Every app message is recorded (bounded `Debug` text in a ring
  buffer), so an action's reply says what the app did with it.

## Background launches (keyboard focus)

An app the server launches (`launch`, `restart`, flows, `run-suite`) runs
**in the background** by default: it does not take keyboard focus from the
editor or terminal you are typing in. Injected input does not need focus — it
is delivered inside the widget tree — so clicks, typing and keys work the
same while the window is unfocused or covered (verified with the todos
example: typing into `#new-todo` and clicking a button while another app
stayed frontmost). `launch {background: false}` (or a flow's
`"launch": {"background": false}`) launches it like a normal app.

The server asks with `ICED_MCP_BACKGROUND=1`; what the bridge can do with it
depends on the platform, because iced 0.14 creates winit's event loop and
windows itself and exposes neither winit's activation settings nor
`with_active(false)`:

| OS | What happens |
|---|---|
| macOS | Handled. winit activates the app when it finishes launching; the bridge watches the app's activation and hands it straight back to the app that was frontmost, unless a mouse click caused it (you clicked the window: it keeps focus). The app becomes an accessory app (no Dock icon, not in Cmd-Tab, no menu bar) and opts out of App Nap so a covered window keeps answering. For a moment at start-up (about one 20 ms sample in our measurements) the app is active, so a keystroke typed in that instant can land in it. `status`/`launch` say "In the background" when the guard is on |
| Windows | Not handled by iced-mcp. Windows' foreground lock usually keeps a window opened by a background process from taking focus (it flashes in the taskbar instead), but that is the OS's rule, not a guarantee; not verified |
| Linux | Not handled by iced-mcp. X11 depends on the window manager's focus-stealing prevention; Wayland compositors generally do not focus a new window without an activation token. Not verified |

An app you start yourself and `connect` to is never changed.

## Security

Anything that can reach the port can drive the app. The bridge binds
`127.0.0.1` only; set `ICED_MCP_TOKEN` to require a token on connect. Keep the
bridge behind a cargo feature that release builds do not enable.

## Limits

- Text drawn on a canvas or by a custom renderer is invisible to `snapshot`;
  use `screenshot` and click by coordinates (the reply gives the scale).
- Widgets behind a modal still report the cursor of whatever covers them
  (iced exposes no z-order), so trust `overlay` and the dialog's own nodes.
- `region` edges are within a grid cell (8 px) of the real ones; strips one
  cell thick are dropped as the edges of something already listed. A list
  without `operate` (e.g. a `pick_list` menu) is one region: screenshot it to
  pick an item.
- Hover effects last until the next real mouse event over the window.
- A minimised window may not redraw, so requests to it time out.

## Upgrading

The bridge and the server check each other's protocol version on connect and
refuse a mismatch with a clear message, so after pulling changes rebuild both
sides:

1. Update the server (`cargo install iced-mcp-server`, or
   `cargo build -p iced-mcp-server` from a clone), then reconnect the MCP server (`/mcp` →
   reconnect `iced`, or restart the agent session) so it runs the new binary.
2. Rebuild the app (`launch` does this when its command is `cargo run …`).

| Protocol | Changes |
|---|---|
| 6 | `text_fit` on text nodes (layout diagnostics); fixtures (`fixture` request, `fixture` action kind, `fixture`/`fixture_error` in `hello`); `background` in `hello` |
| 5 | Direct widget operations for focus/text selection, including overlays |
| 4 | Read-only app queries (`query`, `expect_query`, `query_with_schema`); canvas semantics (`iced-mcp/semantics@1`, `semantic` nodes); JSON Schemas for semantics and flows; `iced-mcp run-flow` for CI; clickable matches win over drawn ones |
| 3 | `perf`, `state`, `clipboard`, app `actions`; server: multiple apps, settling after actions, `wait_idle`, `screenshot_diff`, flows, screen recording, `inspect_point`, `audit`, `paste`, `restart`, `setup_help`, log cursors |
| 2 | `label` nodes (accessible names of icon-only controls); thin `region` strips dropped; action replies group repeated messages; identical overlapping text listed once |
| 1 | First release |

## Environment

| Variable | |
|---|---|
| `ICED_MCP` | `1` enables the bridge under `iced_mcp::run` |
| `ICED_MCP_PORT` | Port (default 9339; `launch` picks a free one) |
| `ICED_MCP_TOKEN` | Require this token on connect |
| `ICED_MCP_FIXTURE` | Load this fixture once the app has booted (`launch {fixture}` sets it) |
| `ICED_MCP_FIXTURE_ARGS` | The fixture's args, as JSON |
| `ICED_MCP_BACKGROUND` | `1`: do not take keyboard focus (macOS; `launch` sets it unless `background: false`) |

## Roadmap

- Tests for multi-window `daemon` apps.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

The JSON Schemas under `crates/*/schema/` are checked by tests; after a
deliberate change regenerate them with `ICED_MCP_UPDATE_SCHEMA=1 cargo test
--workspace --all-features` (and bump the schema version if older files would
no longer validate).

## Releasing

All three crates share one version (`[workspace.package]` in `Cargo.toml`).
Bump it, commit, then push a tag `vX.Y.Z`: the release workflow checks that
the tag matches the version, runs the checks, and publishes
`iced-mcp-protocol`, `iced-mcp` and `iced-mcp-server` to crates.io in that
order. It needs a `CARGO_REGISTRY_TOKEN` repository secret (a crates.io API
token with the publish scope).

A change to the wire protocol bumps `PROTOCOL_VERSION` in
`iced-mcp-protocol` and gets a row in the table under *Upgrading*.
