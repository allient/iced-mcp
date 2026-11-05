//! The connection to an app's bridge, and the app process when we launched it.

use iced_mcp_protocol::{
    AppInfo, Envelope, Hello, Outcome, Reply, Request, Response, ENV_ENABLE, ENV_PORT, ENV_TOKEN,
};

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

/// Lines of app output kept for `logs`.
const LOG_LINES: usize = 3000;
/// The longest any single request may take on our side.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(150);

pub(crate) struct Conn {
    pub(crate) port: u16,
    pub(crate) info: AppInfo,
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl Conn {
    pub(crate) async fn open(port: u16, token: Option<String>) -> Result<Self, String> {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| format!("cannot reach 127.0.0.1:{port}: {e}"))?;
        let _ = stream.set_nodelay(true);
        let (read, write) = stream.into_split();
        let mut conn = Self {
            port,
            info: AppInfo {
                name: String::new(),
                toolkit: String::new(),
                protocol: 0,
                pid: 0,
                windows: Vec::new(),
                messages: false,
                fixture: None,
                fixture_error: None,
                background: false,
            },
            reader: BufReader::new(read),
            writer: write,
            next_id: 1,
        };
        conn.hello(token).await?;
        Ok(conn)
    }

    /// Refreshes [`Conn::info`] (window list, sizes, titles).
    pub(crate) async fn hello(&mut self, token: Option<String>) -> Result<&AppInfo, String> {
        match self.request(Request::Hello(Hello { token })).await? {
            Response::Hello(info) => {
                if info.protocol != iced_mcp_protocol::PROTOCOL_VERSION {
                    return Err(format!(
                        "the app speaks protocol {} but this server speaks {}: rebuild one of them",
                        info.protocol,
                        iced_mcp_protocol::PROTOCOL_VERSION
                    ));
                }
                self.info = info;
                Ok(&self.info)
            }
            other => Err(format!("unexpected reply to hello: {other:?}")),
        }
    }

    pub(crate) async fn request(&mut self, request: Request) -> Result<Response, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line =
            serde_json::to_string(&Envelope { id, request }).map_err(|e| e.to_string())?;
        line.push('\n');

        let exchange = async {
            self.writer
                .write_all(line.as_bytes())
                .await
                .map_err(|e| format!("the app connection broke: {e}"))?;
            loop {
                let mut reply = String::new();
                let read = self
                    .reader
                    .read_line(&mut reply)
                    .await
                    .map_err(|e| format!("the app connection broke: {e}"))?;
                if read == 0 {
                    return Err("the app closed the connection (did it exit?)".to_owned());
                }
                let reply: Reply = serde_json::from_str(&reply)
                    .map_err(|e| format!("bad reply from the app: {e}"))?;
                // A reply to an earlier, timed-out request: skip it.
                if reply.id != id {
                    continue;
                }
                return match reply.outcome {
                    Outcome::Ok(response) => Ok(response),
                    Outcome::Error(error) => Err(error),
                };
            }
        };
        tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| "the app did not answer in time".to_owned())?
    }
}

/// A process we started, with its output captured.
pub(crate) struct Launched {
    pub(crate) child: Child,
    pub(crate) command: String,
    pub(crate) logs: Logs,
}

/// The app's output lines, each numbered so `logs` can read on from a cursor.
#[derive(Clone, Default)]
pub(crate) struct Logs(Arc<Mutex<LogLines>>);

#[derive(Default)]
struct LogLines {
    lines: VecDeque<(u64, String)>,
    next: u64,
}

/// What a `logs` read returned.
pub(crate) struct LogRead {
    pub(crate) lines: Vec<String>,
    /// Pass back as `since` to read only newer lines.
    pub(crate) cursor: u64,
    /// Matching lines left out to stay within the character budget.
    pub(crate) dropped: usize,
}

impl Logs {
    fn push(&self, line: String) {
        let mut log = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if log.lines.len() == LOG_LINES {
            log.lines.pop_front();
        }
        log.next += 1;
        let n = log.next;
        log.lines.push_back((n, line));
    }

    pub(crate) fn tail(&self, count: usize, filter: Option<&str>) -> Vec<String> {
        self.read(None, count, usize::MAX, filter).lines
    }

    /// The newest `count` lines after `since` that contain `filter`, oldest
    /// first, trimmed from the oldest end to fit `max_chars`.
    pub(crate) fn read(
        &self,
        since: Option<u64>,
        count: usize,
        max_chars: usize,
        filter: Option<&str>,
    ) -> LogRead {
        let log = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let filter = filter.map(str::to_lowercase);
        let matching: Vec<&String> = log
            .lines
            .iter()
            .filter(|(n, _)| since.is_none_or(|s| *n > s))
            .filter(|(_, line)| {
                filter
                    .as_deref()
                    .is_none_or(|f| line.to_lowercase().contains(f))
            })
            .map(|(_, line)| line)
            .collect();
        let newest = &matching[matching.len().saturating_sub(count)..];
        let mut budget = max_chars;
        let mut kept: Vec<String> = Vec::new();
        for line in newest.iter().rev() {
            let cost = line.chars().count() + 1;
            if cost > budget {
                break;
            }
            budget -= cost;
            kept.push((*line).clone());
        }
        kept.reverse();
        LogRead {
            dropped: matching.len() - kept.len(),
            lines: kept,
            cursor: log.next,
        }
    }
}

#[derive(Clone)]
pub(crate) struct LaunchSpec {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) env: HashMap<String, String>,
    pub(crate) port: u16,
    pub(crate) token: Option<String>,
}

pub(crate) fn spawn(spec: &LaunchSpec) -> Result<Launched, String> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(&spec.env)
        .env(ENV_ENABLE, "1")
        .env(ENV_PORT, spec.port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(token) = &spec.token {
        command.env(ENV_TOKEN, token);
    }
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start `{}`: {e}", spec.program))?;

    let logs = Logs::default();
    if let Some(stdout) = child.stdout.take() {
        pump(BufReader::new(stdout), logs.clone(), "out");
    }
    if let Some(stderr) = child.stderr.take() {
        pump(BufReader::new(stderr), logs.clone(), "err");
    }
    Ok(Launched {
        child,
        command: std::iter::once(spec.program.as_str())
            .chain(spec.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        logs,
    })
}

fn pump(
    reader: impl tokio::io::AsyncBufRead + Unpin + Send + 'static,
    logs: Logs,
    stream: &'static str,
) {
    tokio::spawn(async move {
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            logs.push(format!("[{stream}] {line}"));
        }
    });
}

/// Waits for a launched app's bridge to answer, watching for an early exit
/// (a build error, a panic at start-up).
pub(crate) async fn wait_for_bridge(
    launched: &mut Launched,
    port: u16,
    token: Option<String>,
    timeout: Duration,
) -> Result<Conn, String> {
    let started = Instant::now();
    loop {
        if let Ok(Some(status)) = launched.child.try_wait() {
            let tail = launched.logs.tail(40, None).join("\n");
            return Err(format!(
                "the app exited ({status}) before its bridge came up. Last output:\n{tail}"
            ));
        }
        if let Ok(mut conn) = Conn::open(port, token.clone()).await {
            // The socket opens before the first frame: wait for a window.
            loop {
                if !conn.info.windows.is_empty() {
                    return Ok(conn);
                }
                if started.elapsed() > timeout {
                    return Err("the bridge answered but no window rendered in time".into());
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
                conn.hello(token.clone()).await?;
            }
        }
        if started.elapsed() > timeout {
            let tail = launched.logs.tail(20, None).join("\n");
            return Err(format!(
                "no bridge on port {port} after {}s. Is the app built with iced-mcp attached and ICED_MCP honoured? Last output:\n{tail}",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// A free loopback port for a new app.
pub(crate) fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .map_err(|e| format!("no free port: {e}"))
}
