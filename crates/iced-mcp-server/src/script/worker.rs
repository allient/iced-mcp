//! The script worker: the Luau VM, in a child process (`iced-mcp
//! script-worker`) the server talks to over stdin/stdout, one JSON message
//! per line ([`Start`], then [`ToHost`] / [`ToWorker`]).
//!
//! The worker holds no app and no files: every `app:method(...)` becomes a
//! [`ToHost::Call`] that the server runs and answers. Keeping the VM in its
//! own process is what lets the server enforce the deadline absolutely: a
//! built-in that never returns to the interpreter (a plain `string.find`
//! over megabytes, a backtracking pattern) cannot be interrupted from
//! inside, but its process can be killed.

use mlua::{Lua, LuaOptions, LuaSerdeExt, MultiValue, StdLib, Value, VmState};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The first line the server sends: what to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Start {
    /// The chunk name errors are reported under.
    pub(crate) name: String,
    pub(crate) source: String,
    /// The app `app` means; the default app when `None`.
    pub(crate) app: Option<String>,
    pub(crate) memory: usize,
    pub(crate) output: usize,
    /// The largest return value, as JSON bytes.
    pub(crate) return_limit: usize,
    pub(crate) timeout_ms: u64,
    pub(crate) methods: Vec<String>,
}

/// From the worker to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToHost {
    Print(String),
    /// `app:method(a, b)` at `line`; `args` are `a` and `b` as JSON.
    Call {
        app: Option<String>,
        method: String,
        line: Option<usize>,
        args: Vec<serde_json::Value>,
    },
    Done(serde_json::Value),
    Failed(String),
}

/// From the server to the worker: a call's result, or the message to raise.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToWorker {
    Ok(serde_json::Value),
    Err(String),
}

/// Builds `app` and `apps` in Lua around one Rust entry point. Each method
/// is a Lua wrapper that raises the entry point's failure with
/// `error(message, 0)`, so a script sees a plain string (`pcall` gets the
/// message, not a wrapped Rust error) whose line was already added.
const HANDLES: &str = r#"
local host, methods = ...
local function handle(name)
  local t = {}
  for _, method in ipairs(methods) do
    t[method] = function(_, a, b)
      local ok, value = host(name, method, a, b)
      if not ok then error(value, 0) end
      return value
    end
  end
  return setmetatable(t, { __tostring = function() return "app<" .. (name or "default") .. ">" end })
end
local apps = setmetatable({}, { __index = function(_, name) return handle(name) end })
return handle(nil), apps
"#;

/// The channel to the server, shared by the VM's callbacks.
struct Link<R, W> {
    input: RefCell<R>,
    output: RefCell<W>,
    /// The server went away: stop at the next chance.
    broken: Cell<bool>,
    printed: Cell<usize>,
    output_limit: usize,
}

impl<R: BufRead, W: Write> Link<R, W> {
    fn send(&self, message: &ToHost) -> bool {
        let Ok(mut line) = serde_json::to_string(message) else {
            return false;
        };
        line.push('\n');
        let mut output = self.output.borrow_mut();
        let ok = output.write_all(line.as_bytes()).is_ok() && output.flush().is_ok();
        if !ok {
            self.broken.set(true);
        }
        ok
    }

    fn receive(&self) -> Result<ToWorker, String> {
        let mut line = String::new();
        match self.input.borrow_mut().read_line(&mut line) {
            Ok(0) | Err(_) => {
                self.broken.set(true);
                Err("the server stopped the script".into())
            }
            Ok(_) => {
                serde_json::from_str(&line).map_err(|e| format!("bad reply from the server: {e}"))
            }
        }
    }

    /// Prints up to the output limit; the server cuts and notes the rest.
    fn print(&self, text: &str) {
        let printed = self.printed.get();
        if printed > self.output_limit {
            return;
        }
        // Never more than the server keeps: one byte past the limit tells
        // it to cut.
        let mut end = text.len().min(self.output_limit + 1 - printed);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.printed.set(printed + end + 1);
        self.send(&ToHost::Print(text[..end].to_owned()));
    }
}

/// `iced-mcp script-worker`: reads a [`Start`] from `input`, runs it and
/// reports on `output`. Returns the process exit status.
pub(crate) fn main(mut input: impl BufRead + 'static, output: impl Write + 'static) -> i32 {
    let mut first = String::new();
    let start: Start = match input.read_line(&mut first) {
        Ok(_) => match serde_json::from_str(&first) {
            Ok(start) => start,
            Err(_) => return 2,
        },
        Err(_) => return 2,
    };
    let link = Rc::new(Link {
        input: RefCell::new(input),
        output: RefCell::new(output),
        broken: Cell::new(false),
        printed: Cell::new(0),
        output_limit: start.output,
    });
    let end = match run(&link, &start) {
        Ok(value) => {
            let size = serde_json::to_string(&value).map_or(0, |t| t.len());
            if size > start.return_limit {
                ToHost::Failed(format!(
                    "{}: the return value is {size} bytes of JSON, over the {} KiB limit: return less (print or query in parts)",
                    start.name,
                    start.return_limit / 1024
                ))
            } else {
                ToHost::Done(value)
            }
        }
        Err(message) => ToHost::Failed(message),
    };
    i32::from(!link.send(&end))
}

fn run<R: BufRead + 'static, W: Write + 'static>(
    link: &Rc<Link<R, W>>,
    start: &Start,
) -> Result<serde_json::Value, String> {
    let lua = sandbox(link, start).map_err(|e| format!("cannot start Luau: {e}"))?;
    let values = lua
        .load(start.source.as_str())
        .set_name(format!("={}", start.name))
        .eval::<MultiValue>()
        .map_err(|e| error_text(&e, start.memory))?;
    Ok(return_value(&lua, values))
}

/// A Luau VM with only safe libraries, the `app`/`apps`/`print` globals,
/// the deadline interrupt and the memory limit, in sandbox mode.
fn sandbox<R: BufRead + 'static, W: Write + 'static>(
    link: &Rc<Link<R, W>>,
    start: &Start,
) -> mlua::Result<Lua> {
    let libs = StdLib::COROUTINE
        | StdLib::TABLE
        | StdLib::STRING
        | StdLib::UTF8
        | StdLib::BIT
        | StdLib::MATH
        | StdLib::BUFFER;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    let globals = lua.globals();
    // mlua installs a filesystem `require` for Luau: scripts get none.
    for name in [
        "require",
        "loadstring",
        "getfenv",
        "setfenv",
        "dofile",
        "load",
    ] {
        globals.raw_set(name, Value::Nil)?;
    }

    let printer = link.clone();
    globals.set(
        "print",
        lua.create_function(move |_, args: MultiValue| {
            let parts: Vec<String> = args
                .iter()
                .map(|v| v.to_string().unwrap_or_else(|_| format!("{v:?}")))
                .collect();
            printer.print(&parts.join("\t"));
            Ok(())
        })?,
    )?;
    let caller = link.clone();
    let entry = lua.create_function(
        move |lua, (app, method, first, second): (Option<String>, String, Value, Value)| {
            // 0 = this function, 1 = the Lua wrapper, 2 = the script.
            let line = current_line(lua, 2);
            let reply = to_args(lua, [first, second]).and_then(|args| {
                if !caller.send(&ToHost::Call {
                    app,
                    method,
                    line,
                    args,
                }) {
                    return Err("the server stopped the script".to_owned());
                }
                match caller.receive()? {
                    ToWorker::Ok(value) => Ok(value),
                    ToWorker::Err(message) => Err(message),
                }
            });
            match reply {
                Ok(value) => match to_lua(lua, &value) {
                    Ok(value) => Ok((true, value)),
                    Err(e) => Ok((false, Value::String(lua.create_string(e)?))),
                },
                Err(message) => Ok((false, Value::String(lua.create_string(message)?))),
            }
        },
    )?;
    let (app, apps): (Value, Value) = lua
        .load(HANDLES)
        .set_name("=iced-mcp")
        .call((entry, start.methods.clone()))?;
    // `app` names the app `script_eval {app}` asked for.
    let app = match (&start.app, apps.as_table()) {
        (Some(name), Some(table)) => table.get::<Value>(name.as_str())?,
        _ => app,
    };
    globals.set("app", app)?;
    globals.set("apps", apps)?;

    lua.sandbox(true)?;
    lua.set_memory_limit(start.memory)?;
    let deadline = Instant::now() + Duration::from_millis(start.timeout_ms);
    let chunk = start.name.clone();
    let timeout_ms = start.timeout_ms;
    let watched = link.clone();
    lua.set_interrupt(move |lua| {
        let reason = if watched.broken.get() {
            Some("the server stopped the script".to_owned())
        } else {
            (Instant::now() >= deadline)
                .then(|| format!("the script timed out after {timeout_ms} ms (raise timeout_ms)"))
        };
        match reason {
            Some(reason) => Err(mlua::Error::RuntimeError(match current_line(lua, 0) {
                Some(line) => format!("{chunk}:{line}: {reason}"),
                None => format!("{chunk}: {reason}"),
            })),
            None => Ok(VmState::Continue),
        }
    });
    Ok(lua)
}

/// The line the Lua code at stack `level` is on.
fn current_line(lua: &Lua, level: usize) -> Option<usize> {
    lua.inspect_stack(level, |debug| debug.current_line())
        .flatten()
}

/// An mlua error as one readable message: the innermost cause, without
/// mlua's callback wrapping or tracebacks.
fn error_text(error: &mlua::Error, memory: usize) -> String {
    match error {
        mlua::Error::CallbackError { cause, .. } => error_text(cause, memory),
        mlua::Error::WithContext { cause, .. } => error_text(cause, memory),
        mlua::Error::RuntimeError(message) => message
            .split("\nstack traceback:")
            .next()
            .unwrap_or(message)
            .to_owned(),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::MemoryError(_) => format!(
            "the script ran out of memory (limit {} MiB)",
            memory / (1024 * 1024)
        ),
        other => other.to_string(),
    }
}

/// The script's return values as JSON: none → null, one → itself, several
/// → an array. What JSON cannot hold (functions, userdata) becomes text.
fn return_value(lua: &Lua, values: MultiValue) -> serde_json::Value {
    let mut list: Vec<serde_json::Value> = values.into_iter().map(|v| to_json(lua, v)).collect();
    match list.len() {
        0 => serde_json::Value::Null,
        1 => list.remove(0),
        _ => serde_json::Value::Array(list),
    }
}

pub(crate) fn to_json(lua: &Lua, value: Value) -> serde_json::Value {
    match lua.from_value::<serde_json::Value>(value.clone()) {
        Ok(json) => json,
        Err(_) => serde_json::Value::String(value.to_string().unwrap_or_default()),
    }
}

/// A method's arguments as JSON (`nil` → null); anything JSON cannot hold
/// is refused here.
fn to_args(lua: &Lua, values: [Value; 2]) -> Result<Vec<serde_json::Value>, String> {
    values
        .into_iter()
        .map(|value| match value {
            Value::Nil => Ok(serde_json::Value::Null),
            Value::Function(_)
            | Value::Thread(_)
            | Value::UserData(_)
            | Value::LightUserData(_) => {
                Err(format!("bad args: cannot pass a {}", value.type_name()))
            }
            other => lua
                .from_value::<serde_json::Value>(other)
                .map_err(|e| format!("bad args: {e}")),
        })
        .collect()
}

pub(crate) fn to_lua(lua: &Lua, value: &serde_json::Value) -> Result<Value, String> {
    let options = mlua::serde::SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false);
    lua.to_value_with(value, options).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_converts_both_ways() {
        let lua = Lua::new();
        let json = serde_json::json!({ "a": [1, 2.5, "x", true], "b": { "c": null }, "e": [] });
        let value = to_lua(&lua, &json).unwrap();
        let back = to_json(&lua, value);
        assert_eq!(back["a"], serde_json::json!([1, 2.5, "x", true]));
        assert!(back["b"].get("c").is_none(), "null becomes nil");
        assert_eq!(back["e"], serde_json::json!([]));
        // Whole numbers stay integers, which step parameters need.
        let table: Value = lua.load("return { ms = 250, x = 10.5 }").eval().unwrap();
        let args = to_args(&lua, [table, Value::Nil]).unwrap();
        assert_eq!(args[0], serde_json::json!({ "ms": 250, "x": 10.5 }));
        assert!(args[1].is_null());
        let function = Value::Function(lua.create_function(|_, ()| Ok(())).unwrap());
        assert_eq!(
            to_args(&lua, [function.clone(), Value::Nil]).unwrap_err(),
            "bad args: cannot pass a function"
        );
        assert_eq!(
            to_json(&lua, function)
                .as_str()
                .map(|s| s.starts_with("function")),
            Some(true)
        );
    }
}
