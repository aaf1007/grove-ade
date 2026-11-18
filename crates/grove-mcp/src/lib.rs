//! The core socket client that `grove-mcp` and the `grovecode` CLI share. Every request carries
//! the calling agent session's id and token, so a session can act only as itself.
//!
//! One request per connection: write one JSON request, end the write half, read one JSON reply,
//! `{"result": ...}` or `{"error": "..."}`, up to the core's end.

use std::env;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

pub mod tools;

/// The variables grove puts in an agent session's environment and MCP config.
pub const GROVE_SESSION_ID: &str = "GROVE_SESSION_ID";
pub const GROVE_SESSION_TOKEN: &str = "GROVE_SESSION_TOKEN";
pub const GROVE_SOCKET: &str = "GROVE_SOCKET";

/// How long each read or write of a request may wait. A core that is stuck must not hold the
/// agent's tool call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One agent session's way to the core.
pub struct CoreClient {
    socket: PathBuf,
    session_id: String,
    session_token: String,
    timeout: Duration,
}

impl CoreClient {
    /// The client for the session grove started this process in, from its environment. The
    /// socket defaults to `~/.grovecode/grove.sock`.
    pub fn from_env() -> Result<Self, RequestError> {
        let var = |name| env::var(name).map_err(|_| RequestError::MissingVariable(name));
        // The session first: outside an agent session, that is what is missing.
        let session_id = var(GROVE_SESSION_ID)?;
        let session_token = var(GROVE_SESSION_TOKEN)?;
        let socket = match env::var_os(GROVE_SOCKET) {
            Some(path) => PathBuf::from(path),
            None => env::home_dir()
                .ok_or(RequestError::MissingVariable("HOME"))?
                .join(".grovecode/grove.sock"),
        };
        Ok(Self {
            socket,
            session_id,
            session_token,
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// Sends `request`, such as `{"type": "subagent_spawn", ...}`, and returns the reply's
    /// result.
    pub fn request(&self, request: Value) -> Result<Value, RequestError> {
        let kind = request["type"].as_str().unwrap_or_default().to_owned();
        let message = json!({
            "session": self.session_id,
            "token": self.session_token,
            "request": request,
        });
        let mut stream =
            UnixStream::connect(&self.socket).map_err(|source| RequestError::CoreNotRunning {
                socket: self.socket.clone(),
                source,
            })?;
        let timeout = Some(self.timeout);
        let failed = |source: io::Error| match source.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => RequestError::TimedOut {
                request: kind.clone(),
            },
            _ => RequestError::Io(source),
        };
        stream.set_write_timeout(timeout).map_err(failed)?;
        stream.set_read_timeout(timeout).map_err(failed)?;
        let sent = stream
            .write_all(message.to_string().as_bytes())
            .and_then(|()| stream.shutdown(Shutdown::Write));
        // The core may refuse before it has read it all, such as a request too large, and
        // reply. So read its reply even when sending failed.
        let mut reply = Vec::new();
        let read = stream.read_to_end(&mut reply);
        let reply: Value = match serde_json::from_slice(&reply) {
            Ok(reply) => reply,
            Err(_) => {
                sent.map_err(failed)?;
                read.map_err(failed)?;
                return Err(RequestError::BadReply);
            }
        };
        match (reply.get("result"), reply.get("error")) {
            (Some(result), None) => Ok(result.clone()),
            (None, Some(Value::String(error))) => Err(RequestError::Refused(error.clone())),
            _ => Err(RequestError::BadReply),
        }
    }
}

#[derive(Debug)]
pub enum RequestError {
    /// grove did not start this process, or started it without the variable.
    MissingVariable(&'static str),
    /// Nothing listens on the socket, so grove is not running.
    CoreNotRunning { socket: PathBuf, source: io::Error },
    /// The connection failed midway.
    Io(io::Error),
    /// The core did not answer in time. It may still act on the request, of this type.
    TimedOut { request: String },
    /// The core refused the request and said why.
    Refused(String),
    /// The core's reply was not a reply. The core may have ended the connection, for one.
    BadReply,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVariable(name) => {
                write!(
                    f,
                    "{name} is not set, so grove did not start this agent session"
                )
            }
            Self::CoreNotRunning { socket, source } => write!(
                f,
                "grove is not running: nothing answers on {} ({source})",
                socket.display()
            ),
            Self::Io(source) => write!(f, "talking to grove failed: {source}"),
            Self::TimedOut { request } => {
                write!(f, "grove did not answer in time")?;
                match request.as_str() {
                    "subagent_spawn" => write!(
                        f,
                        ". It may still start the subagent, so check before you try again"
                    ),
                    "panel_open" => write!(
                        f,
                        ". It may still open the panel, which would count toward the 8 a \
                         session may hold"
                    ),
                    _ => Ok(()),
                }
            }
            Self::Refused(reason) => write!(f, "grove refused: {reason}"),
            Self::BadReply => write!(f, "grove sent no reply"),
        }
    }
}

impl std::error::Error for RequestError {}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;

    #[test]
    fn a_core_that_never_answers_gives_a_timeout_that_fits_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("grove.sock");
        // It accepts the connection but never replies.
        let listener = UnixListener::bind(&socket).unwrap();
        let client = CoreClient {
            socket,
            session_id: "1".into(),
            session_token: "t".into(),
            timeout: Duration::from_millis(200),
        };
        let cases = [
            (
                json!({"type": "subagent_spawn", "agent": "codex", "prompt": "hi"}),
                "grove did not answer in time. It may still start the subagent, so check \
                 before you try again",
            ),
            (
                json!({"type": "panel_open", "kind": "log", "title": "t", "cmd": "true"}),
                "grove did not answer in time. It may still open the panel, which would count \
                 toward the 8 a session may hold",
            ),
            (
                json!({"type": "panel_read", "panel": "7", "lines": 0}),
                "grove did not answer in time",
            ),
        ];

        for (request, text) in cases {
            let error = client.request(request).unwrap_err();

            assert!(matches!(error, RequestError::TimedOut { .. }), "{error:?}");
            assert_eq!(error.to_string(), text);
        }
        drop(listener);
    }
}
