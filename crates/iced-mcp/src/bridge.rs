//! The loopback socket the MCP server connects to. Each connection gets a
//! thread that reads request lines, hands them to the app as messages, and
//! writes back each reply once the app has produced it.

use crate::Config;
use iced_mcp_protocol::{Envelope, Outcome, Reply, Request};

use futures::channel::mpsc::UnboundedSender;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::time::Duration;

/// How long a request may wait for the app. Typing a long text runs one frame
/// per character, so this is generous; a window that never renders (minimised
/// on some platforms) is the usual reason to hit it.
const REPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// A request on its way into the app, with where to send the answer.
#[derive(Debug, Clone)]
pub(crate) struct Command {
    pub(crate) request: Request,
    pub(crate) reply: std::sync::mpsc::SyncSender<Outcome>,
}

/// Binds the socket and serves it on a background thread.
pub(crate) fn start(config: &Config, inbox: UnboundedSender<Command>) -> std::io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))?;
    let port = listener.local_addr()?.port();
    let token = config.token.clone();

    std::thread::Builder::new()
        .name("iced-mcp".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let inbox = inbox.clone();
                let token = token.clone();
                let _ = std::thread::Builder::new()
                    .name("iced-mcp-conn".into())
                    .spawn(move || serve(stream, &inbox, token.as_deref()));
            }
        })?;

    Ok(port)
}

fn serve(stream: TcpStream, inbox: &UnboundedSender<Command>, token: Option<&str>) {
    let _ = stream.set_nodelay(true);
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let mut writer = std::io::BufWriter::new(writer);
    let mut authorised = token.is_none();

    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else {
            return;
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Envelope>(&line) {
            Err(error) => Reply {
                id: 0,
                outcome: Outcome::Error(format!("malformed request: {error}")),
            },
            Ok(Envelope { id, request }) => {
                if !authorised {
                    let presented = match &request {
                        Request::Hello(hello) => hello.token.as_deref(),
                        _ => None,
                    };
                    if presented != token {
                        let _ = write_reply(
                            &mut writer,
                            &Reply {
                                id,
                                outcome: Outcome::Error(
                                    "unauthorised: send hello with the token".into(),
                                ),
                            },
                        );
                        return;
                    }
                    authorised = true;
                }
                Reply {
                    id,
                    outcome: dispatch(inbox, request),
                }
            }
        };
        if write_reply(&mut writer, &reply).is_err() {
            return;
        }
    }
}

fn dispatch(inbox: &UnboundedSender<Command>, request: Request) -> Outcome {
    let (reply, answer) = sync_channel(1);
    if inbox.unbounded_send(Command { request, reply }).is_err() {
        return Outcome::Error("the app is shutting down".into());
    }
    match answer.recv_timeout(REPLY_TIMEOUT) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Timeout) => Outcome::Error(
            "timed out waiting for the app: is the window minimised or hidden, so it never redraws?"
                .into(),
        ),
        Err(RecvTimeoutError::Disconnected) => {
            Outcome::Error("the app dropped the request (window closed?)".into())
        }
    }
}

fn write_reply(writer: &mut impl Write, reply: &Reply) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, reply)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
