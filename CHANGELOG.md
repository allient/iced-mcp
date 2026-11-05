# Changelog

All notable changes to iced-mcp. The three crates share one version.

## 0.3.0 — 2026-11-04

### Added

- **Luau scripts**: `script_eval {source, app?, timeout_ms?}` runs one
  sandboxed Luau script instead of many tool calls. `app` (and
  `apps["name"]` for other apps) has a method per step (`click`,
  `type_text`, `wait_for`, `load_fixture`, `query`, `expect`,
  `expect_pixel`, `diagnose`, `snapshot`, `find`/`find_all`,
  `screenshot_diff`, `messages`, `state`, `logs`…) taking the step's
  arguments; a failing step raises its own message with the script line.
  The reply has the printed output, the return value as JSON, the steps
  with their lines and the duration; a failure writes a bundle with
  `script.luau`.
- `script_api` and `crates/iced-mcp-server/iced-mcp.d.luau`: the typed
  API, for agents and editors. Scripts are type-checked with
  `luau-analyze` (on `PATH` or `ICED_MCP_LUAU_ANALYZE`) before they touch
  the app; without it the result says the check did not run.
- Sandbox: Luau sandbox mode, no `io`/`os`/`debug`/`require`/
  `loadstring`, a wall-clock deadline (60 s default), 64 MiB of memory and
  64 KiB of output. Each script runs on its own thread, so other MCP
  requests go on between its steps.
- `iced-mcp run-script FILE [--launch CMD] [--root DIR] [--timeout-ms N]`
  (exit 0/1/2), and `run-suite` runs `.iced-mcp/scripts/*.luau` after the
  flows on the `--launch` app, with the same `--only`, `--repeat`,
  `--until-fail`, `--fail-fast` and failure bundles.
- Example scripts for the todos example in
  `examples/todos/.iced-mcp/scripts/`.

### Changed

- The server depends on `mlua` (Luau, built from C++ source): building it
  needs a C++ compiler.
- The protocol is unchanged (version 6): a 0.2 bridge works with this
  server.

## 0.2.0 — 2026-11-04

### Added

- **Layout diagnostics**: `diagnose` reports overlapping interactive widgets,
  content clipped by its container or the window (or squeezed to nothing),
  text that does not fit its widget, and duplicate widget ids. The flow step
  `expect_no_diagnostics` fails a flow on them. The bridge measures iced's
  `text` paragraphs and reports `text_fit` on text nodes.

- **Suite runner**: `iced-mcp run-suite [--only GLOB] [--repeat N]
  [--until-fail N] [--fail-fast] [--update-baselines] [--launch CMD]` runs
  every flow with PASS/FAIL per flow, a summary and exit status 0/1/2.
- **Failure bundles**: a failing flow step writes
  `.iced-mcp/failures/<flow>-<timestamp>/` with the tree, diagnostics,
  screenshot, step log, app output and recent messages; `flow_run` and the
  CLIs report its path.
- **Fixtures**: `attach(app).fixture(name, description, |args| Ok(state))`
  (or `fixture_messages`) registers named starting states. MCP `fixtures`
  and `load_fixture`, `launch {fixture, fixture_args}` and
  `ICED_MCP_FIXTURE`/`ICED_MCP_FIXTURE_ARGS` start an app in one, and a
  flow's `fixture` is loaded before its first step. The todos example has
  `empty` and `groceries`.
- **Background launches**: apps the server launches no longer take keyboard
  focus on macOS (activation handed back unless a click caused it,
  accessory app, no App Nap); `launch {background: false}` opts out.
  `ICED_MCP_BACKGROUND` asks for it; other platforms are documented, not
  handled.
- **Structural assertions**: the `expect` step checks a target's presence,
  visibility, enabled/focused state, text, value, geometry relative to
  another target (`left_of`, `same_row`, `aligned_left`… within a
  tolerance), text fit and child count; `expect_pixel` checks a pixel's
  colour. Failures say what was expected and what was found.
- The flow schema (`flow.v1.json`) now lists every step action with its
  arguments, and `run_sequence`/`flow_add_step` take the same action names.
- `skills/SKILL.md`: an agent skill on using iced-mcp well.

### Changed

- Protocol version 6 (see *Upgrading* in the guide): rebuild the app and the
  server together.
- The workspace version is 0.2.0 (unpublished): the wire changed, and
  `cargo package` must verify against these sources, not the published 0.1.0.

## 0.1.0

First release.
