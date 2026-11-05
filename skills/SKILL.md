---
name: iced-mcp
description: Drive and test running iced (Rust) desktop apps through the iced-mcp MCP server. Use when launching an iced app, reading its screen, clicking or typing in it, checking a UI change, writing or running flows (regression tests) with fixtures and expect assertions, running the suite, or reading a failure bundle.
---

# Using iced-mcp

iced-mcp gives you eyes and hands on a **running** iced app: its widget tree
as text, input injected inside the app, and the messages the app published
in reply. Full reference: `docs/guide.md`.

## Loop

1. **Start**: `launch {command: "cargo run -p my-app --features agent"}`
   (builds can take minutes; it runs in the background and does not steal
   the user's focus). Add `fixture` to start in a known state. No bridge
   yet? `setup_help`.
2. **Look**: `snapshot` → `[ref] kind "text" #id (x,y w×h) cursor`.
   `pointer` = clickable, `overlay` = menus/popups, `label` = icon button
   name, `region` = clickable area nothing named. Canvases: `screenshot`.
3. **Act** by `id` > `text` > `ref` > `x`/`y`: `click`, `type_text {id, text,
   submit}`, `press_key "Mod+S"`, `scroll`, `drag`.
4. **Verify** from the reply: every action waits for the screen to settle
   and lists the **messages** it caused. "No messages published — nothing
   reacted" means you missed the target. Then `wait_for`, `state`, `query`.
5. **Check layout** after UI edits: `diagnose` (overlaps, clipping, text
   that does not fit, duplicate ids) and `audit` (unnamed controls).

## Tests

- Record: `flow_start {name}` → act → `flow_add_step` for assertions →
  `flow_finish`. Writes `.iced-mcp/flows/<name>.json` (schema:
  `crates/iced-mcp-server/schema/flow.v1.json`).
- Start every flow from a **fixture** (`"fixture": "empty-cart"`); list them
  with `fixtures`. The app registers them with
  `attach(app).fixture(name, description, |args| Ok(State::…))`.
- Assert structure, not pixels:
  - `expect {id, visible, enabled, focused, text_equals, text_contains,
    value, fits, children, count}`;
  - geometry: `expect {text: "Save", right_of: {id: "name"}, same_row:
    {id: "name"}, tolerance: 2}`;
  - `expect_message {contains}`, `expect_query {name, contains}`,
    `expect_no_diagnostics`;
  - `expect_pixel {x, y, color}` and `screenshot_diff` only when colour is
    the point.
- Run one: `flow_run {name}`. Run all: `iced-mcp run-suite` (`--only
  'cart-*'`, `--until-fail 20` for flaky ones, `--launch CMD` for flows
  without their own `launch`). Exit 0 pass, 1 fail, 2 bad arguments.

## When a flow fails

Open the bundle path in the report: `.iced-mcp/failures/<flow>-<time>/`.
Read `failure.txt` (step and error), then `screenshot.png`, `snapshot.txt`
(the tree at failure, hidden nodes included), `diagnostics.txt`,
`messages.log` (what the app did) and `app-output.log` (panics, logs).
Fix the app or the flow, then rerun only it with `--only`.

## Pitfalls

- Give targets ids (`.id("search")`); text changes, refs change every
  snapshot.
- `expect_message` only counts messages after the step starts: put it right
  after the action that causes them.
- Text drawn on a canvas is invisible to `snapshot`; ask the widget to
  report semantics, or use `query`.
- After pulling iced-mcp changes, rebuild both the server and the app (the
  protocol version must match).
