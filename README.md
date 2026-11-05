# iced-mcp

**Let your AI agent use your [iced](https://iced.rs) app the way you do.**

[![crates.io](https://img.shields.io/crates/v/iced-mcp.svg)](https://crates.io/crates/iced-mcp)
[![docs.rs](https://img.shields.io/docsrs/iced-mcp)](https://docs.rs/iced-mcp)
[![CI](https://github.com/allient/iced-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/allient/iced-mcp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/allient/iced-mcp/blob/main/LICENSE)

Coding agents (Claude Code, Codex, Cursor, any MCP client) can write iced code,
but they can't see what it does. iced-mcp gives them eyes and hands on a
**running** app. They launch it, read the screen, click, type and scroll,
check the result, and turn what they did into a regression test. The same
loop you run by hand, now in seconds and without you.

```
you: "add a search box to the orders list and make sure it filters"

agent → edits code → launch → snapshot → type_text "BTC" → snapshot
      → "3 of 12 rows left, all BTC" → flow_finish "search-filters"
```

## Why iced-mcp

- **The agent checks its own work.** Every action waits for the screen to
  settle, then answers with what changed *and the messages your app
  published*. No guessing from a screenshot.
- **Built for iced, not bolted on.** It reads iced's own widget tree, so the
  agent targets `#search`, the text "Save" or an icon's name, not pixel
  coordinates that break on the next layout change.
- **Tests come for free.** Record a session as a flow, add assertions, replay
  it in CI with `iced-mcp run-suite`. Visual baselines catch what text can't.
- **No OS permissions.** Input is injected inside iced, not through the OS:
  nothing to grant in System Settings, and it works on macOS, Windows and Linux.
- **Sees custom drawing too.** Charts, canvases and plots can describe what
  they drew in a few lines, so the agent can click "the 12:45 bar".
- **Zero cost when off.** One line in `main`, behind a cargo feature that
  release builds leave out. Inert unless `ICED_MCP=1`.

## Quick start

**1. Install the server** (once):

```sh
cargo install iced-mcp-server
```

**2. Register it** with your agent, e.g. `.mcp.json` for Claude Code:

```json
{ "mcpServers": { "iced": { "command": "iced-mcp" } } }
```

**3. Add the bridge to your app:**

```toml
[dependencies]
iced-mcp = { version = "0.2", optional = true }

[features]
agent = ["dep:iced-mcp"]   # left out of release builds
```

```rust
fn main() -> iced::Result {
    let app = iced::application(App::new, App::update, App::view);

    #[cfg(feature = "agent")]
    return iced_mcp::run(app); // the bridge when ICED_MCP=1, else app.run()

    #[cfg(not(feature = "agent"))]
    app.run()
}
```

Then ask your agent to *"launch `cargo run --features agent` and try the
login form"*. It starts the app itself.

> Tip: give widgets agents should find an id (`text_input(..).id("search")`).
> Ids are the most stable target.

## What the agent can do

| | |
|---|---|
| **Look** | `snapshot` (what's on screen, with ids, text and what's clickable), `screenshot`, `accessibility_tree`, `inspect_point`, `audit` (unnamed controls), `diagnose` (overlaps, clipping, text that doesn't fit, duplicate ids) |
| **Act** | `click`, `type_text`, `press_key`, `scroll`, `drag`, `hover`, `paste`, `window`, all by id, text or position |
| **Wait** | `wait_for` text or an id, `wait_idle` until the screen settles |
| **Ask the app** | `messages`, `logs`, `state`, `query`, `run_app_action`, `perf` (frame, view and update timings), `clipboard`, `fixtures` and `load_fixture` (named starting states for tests) |
| **Test** | `flow_start` → `flow_finish` → `flow_run`, `screenshot_diff`, `run_sequence`, screen recording; assertions (`expect` a widget visible, focused, left of another, its text fitting; `expect_pixel`); `iced-mcp run-suite` runs them all in CI and keeps a failure bundle (tree, diagnostics, screenshot, logs) per failure |
| **Sessions** | `launch` (in the background: it doesn't steal your keyboard focus on macOS), `connect`, `restart`, several apps at once |

A snapshot reads like this:

```
window 1 "Todos" 1024×768 — 10 of 17 nodes
[3] input "What needs doing?" #new-todo (20,68 926×31) text focused
[5] text "Add" (964,73 30×21) pointer
[11] scroll #todo-list (28,162 968×220) scroll(0,0 of 968×220)
```

Try it on the bundled demo from a clone: ask the agent to `launch`
`cargo run -p iced-mcp --example todos`.

## Go further

Optional hooks make the agent faster and your tests sturdier:

```rust
iced_mcp::attach(app)
    .state(|state| format!("{state:#?}"))                  // what the app holds
    .action("load_demo", "Loads demo data.", |_| Ok(Message::LoadDemo))
    .query("total", "The cart total.", |state, _| Ok(serde_json::json!(state.total())))
    .fixture("empty-cart", "An empty cart.", |_| Ok(State::default())) // where tests start
    .run()
```

**Agents:** [`skills/SKILL.md`](https://github.com/allient/iced-mcp/blob/main/skills/SKILL.md)
is a short skill on using iced-mcp well (launch, target by id, fixtures,
`expect`, the suite, failure bundles); drop it into your agent's skills.

**[Read the guide →](https://github.com/allient/iced-mcp/blob/main/docs/guide.md)** for every tool, naming icon-only buttons,
describing custom-drawn widgets, running flows in CI, how it works, security
and limits.

## How it fits together

```
agent ──MCP (stdio)──▶ iced-mcp server ──JSON over 127.0.0.1──▶ bridge inside your app
```

| Crate | |
|---|---|
| [`iced-mcp`](https://crates.io/crates/iced-mcp) | The bridge you add to your app |
| [`iced-mcp-server`](https://crates.io/crates/iced-mcp-server) | The MCP server (binary `iced-mcp`) |
| [`iced-mcp-protocol`](https://crates.io/crates/iced-mcp-protocol) | The wire protocol, with no iced in it, so other toolkits can reuse the server |

Requires iced **0.14**. The bridge only listens on `127.0.0.1`; set
`ICED_MCP_TOKEN` to require a token.

## Inspiration

iced-mcp follows the shape of the projects that did this first for other
toolkits: [mcp-server-tauri] (Tauri webviews), [marionette_mcp] (Flutter) and
[egui_mcp] (egui). [eguidev] showed how far in-process automation can go for
egui: layout diagnostics, structural assertions, fixtures and a smoke-test
runner. Its flows and visual baselines borrow from end-to-end
tools like Playwright.

## Contributing

Issues and pull requests are welcome. `cargo test --workspace --all-features`
runs everything. See [Development](https://github.com/allient/iced-mcp/blob/main/docs/guide.md#development) and
[Releasing](https://github.com/allient/iced-mcp/blob/main/docs/guide.md#releasing).

## License

MIT, see [LICENSE](https://github.com/allient/iced-mcp/blob/main/LICENSE).

[mcp-server-tauri]: https://github.com/hypothesi/mcp-server-tauri
[marionette_mcp]: https://github.com/leancodepl/marionette_mcp
[egui_mcp]: https://github.com/dijdzv/egui-mcp
[eguidev]: https://github.com/cortesi/eguidev
