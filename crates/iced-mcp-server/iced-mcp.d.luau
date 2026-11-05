--!nonstrict
-- iced-mcp.d.luau: the API of iced-mcp scripts (`script_eval`, `iced-mcp
-- run-script`, `.iced-mcp/scripts/*.luau`). For editors (luau-lsp
-- `--definitions`) and agents (`script_api`). The server type-checks scripts
-- against this file with `luau-analyze` when it is installed.
--
-- Every method maps onto the step of the same name (the `run_sequence` /
-- flow vocabulary) and takes that step's arguments as a table. A failing
-- step raises a Lua error with the step's own message, prefixed with the
-- script line: `script:7: nothing visible matches #save…`. Catch it with
-- `pcall` when a failure is expected.
--
-- A script sets its own deadline (default 60 s, at most 30 min) with a line
-- among its leading comments: `--! timeout_ms=300000`.

-- What to act on (the same fields are inline in each *Args type): one of `id`, `text`, `ref` (from `snapshot`/`find`), or `x` + `y`.
export type Target = {
	id: string?,
	text: string?,
	exact: boolean?,
	ref: number?,
	x: number?,
	y: number?,
	nth: number?,
	window: string?,
}

export type Rect = { x: number, y: number, width: number, height: number }

-- One widget on screen, as `snapshot` and `find` return it.
export type Node = {
	ref: number,
	index: number,
	parent: number?,
	kind: string, -- container, scrollable, focusable, text_input, text, custom, region, label, semantic
	layer: string, -- base or overlay
	id: string?,
	text: string?,
	bounds: Rect,
	visible: Rect?, -- nil when scrolled away or clipped
	focused: boolean?,
	interaction: string?, -- the cursor shown over it: pointer, text, grab…
	role: string?,
	value: string?,
	label: string?,
	description: string?,
	placeholder: string?,
	disabled: boolean?,
	checked: boolean?,
	selected: boolean?,
	expanded: boolean?,
	choices: { string }?,
	numeric_value: number?,
	text_fit: any?,
	scroll: any?,
}

export type Snapshot = { window: string, width: number, height: number, nodes: { Node } }

-- `find` / `find_all`: every field given must match. `role` matches a
-- semantic node's role or a node's kind (`text_input`, `text`…).
export type Selector = {
	id: string?,
	text: string?,
	exact: boolean?,
	role: string?,
	window: string?,
}

export type ClickArgs = { id: string?, text: string?, exact: boolean?, ref: number?, x: number?, y: number?, nth: number?, window: string?, button: string?, count: number?, modifiers: string?, snapshot: boolean? }
export type TypeArgs = { id: string?, exact: boolean?, ref: number?, x: number?, y: number?, nth: number?, window: string?, text: string, clear: boolean?, submit: boolean?, snapshot: boolean? }
export type KeyArgs = { keys: string, ["repeat"]: number?, window: string?, snapshot: boolean? }
export type ScrollArgs = { id: string?, text: string?, exact: boolean?, ref: number?, x: number?, y: number?, nth: number?, window: string?, dx: number?, dy: number?, unit: string?, snapshot: boolean? }
export type DragArgs = {
	-- the target (see Target)
	id: string?,
	text: string?,
	exact: boolean?,
	ref: number?,
	x: number?,
	y: number?,
	nth: number?,
	window: string?,
	to_x: number?,
	to_y: number?,
	dx: number?,
	dy: number?,
	steps: number?,
	button: string?,
	modifiers: string?,
	snapshot: boolean?,
}
export type PasteArgs = { id: string?, exact: boolean?, ref: number?, x: number?, y: number?, nth: number?, window: string?, text: string }
export type WaitArgs = { text: string?, exact: boolean?, id: string?, gone: boolean?, timeout_ms: number?, window: string? }
export type WaitIdleArgs = { window: string?, stable_ms: number?, timeout_ms: number? }
export type ExpectArgs = {
	-- the target (see Target)
	id: string?,
	text: string?,
	exact: boolean?,
	ref: number?,
	x: number?,
	y: number?,
	nth: number?,
	window: string?,
	exists: boolean?,
	visible: boolean?,
	enabled: boolean?,
	focused: boolean?,
	text_equals: string?,
	text_contains: string?,
	value: string?,
	fits: boolean?,
	children: number?,
	count: number?,
	left_of: Target?,
	right_of: Target?,
	above: Target?,
	below: Target?,
	inside: Target?,
	same_row: Target?,
	same_column: Target?,
	aligned_left: Target?,
	aligned_right: Target?,
	aligned_top: Target?,
	aligned_bottom: Target?,
	tolerance: number?,
	timeout_ms: number?,
}
export type ExpectPixelArgs = { x: number, y: number, color: string, tolerance: number?, timeout_ms: number?, window: string? }
export type DiagnoseArgs = { window: string?, kinds: { string }? }
export type ExpectMessageArgs = { contains: string, timeout_ms: number?, since: number? }
export type ExpectQueryArgs = { name: string, args: any?, contains: string, timeout_ms: number? }
export type DiffArgs = { id: string?, text: string?, exact: boolean?, ref: number?, x: number?, y: number?, nth: number?, window: string?, baseline: string, update: boolean?, region: Rect?, max_mismatch: number?, tolerance: number? }
-- `name` is the file: `.iced-mcp/screenshots/<name>.png` under the project root.
export type ScreenshotArgs = { id: string?, text: string?, exact: boolean?, ref: number?, x: number?, y: number?, nth: number?, window: string?, name: string, region: Rect?, highlight: boolean?, max_width: number? }
export type MessagesArgs = { since: number?, filter: string?, limit: number? }
export type Message = { seq: number, at_ms: number, text: string }
export type StateArgs = { filter: string?, max_chars: number? }
export type LogsArgs = { tail: number?, filter: string?, max_chars: number? }
export type Finding = { kind: string, refs: { number }, ids: { string? }, bounds: { Rect }, explanation: string }

-- One connected app. Methods that act wait for the screen to settle and
-- return the step's reply: what was hit and the messages the app published.
export type App = {
	-- Act
	click: (self: App, target: ClickArgs | string) -> string,
	type_text: (self: App, args: TypeArgs | string) -> string,
	press_key: (self: App, keys: KeyArgs | string) -> string,
	scroll: (self: App, args: ScrollArgs) -> string,
	drag: (self: App, args: DragArgs) -> string,
	hover: (self: App, target: Target | string) -> string,
	paste: (self: App, args: PasteArgs | string) -> string,
	-- Wait
	wait_for: (self: App, args: WaitArgs | string) -> string,
	wait_idle: (self: App, args: WaitIdleArgs?) -> string,
	sleep: (self: App, ms: number) -> string,
	-- The app's side
	run_app_action: (self: App, name: string, args: any?) -> string,
	query: (self: App, name: string, args: any?) -> any,
	load_fixture: (self: App, name: string, args: any?) -> string,
	fixture: (self: App, name: string, args: any?) -> string,
	messages: (self: App, args: MessagesArgs?) -> { Message },
	state: (self: App, args: StateArgs?) -> string,
	logs: (self: App, args: LogsArgs?) -> { string },
	-- Look
	snapshot: (self: App, window: string?) -> Snapshot,
	find: (self: App, selector: Selector | string) -> Node?,
	find_all: (self: App, selector: Selector | string) -> { Node },
	diagnose: (self: App, args: DiagnoseArgs?) -> { Finding },
	screenshot: (self: App, args: ScreenshotArgs | string) -> string,
	screenshot_diff: (self: App, args: DiffArgs | string) -> string,
	-- Assert (raise on failure)
	expect: ((self: App, args: ExpectArgs) -> string) & ((self: App, node: Node, checks: ExpectArgs) -> string),
	expect_pixel: (self: App, args: ExpectPixelArgs) -> string,
	expect_no_diagnostics: (self: App, args: DiagnoseArgs?) -> string,
	expect_message: (self: App, args: ExpectMessageArgs | string) -> string,
	expect_query: (self: App, args: ExpectQueryArgs) -> string,
}

-- The app `script_eval {app}` names, else the default (last connected) one.
declare app: App
-- Any connected app by name: `apps["admin"]:click{ id = "ok" }`.
declare apps: { [string]: App }
