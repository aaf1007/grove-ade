//! Panel requests on the core socket. Each carries its session id and that session's token, so
//! one process cannot act as another session. Hook messages carry no token.
//!
//! A client writes one JSON request, ends its half of the connection, and reads one JSON reply
//! up to the core's end: `{"result": ...}` or `{"error": "..."}`.
//!
//! ```json
//! {"session": "3", "token": "<64 hex digits>", "request": {"type": "session_id"}}
//! {"session": "3", "token": "...", "request": {"type": "subagent_spawn", "agent": "codex",
//!  "prompt": "write the tests", "worktree": "/optional/tracked/worktree"}}
//! {"session": "3", "token": "...", "request": {"type": "panel_open", "kind": "log",
//!  "title": "tests", "cmd": "cargo test"}}
//! {"session": "3", "token": "...", "request": {"type": "panel_read", "panel": "7", "lines": 50}}
//! {"session": "3", "token": "...", "request": {"type": "panel_close", "panel": "7"}}
//! {"session": "3", "token": "...", "request": {"type": "workspace_get"}}
//! {"session": "3", "token": "...", "request": {"type": "workspace_set", "workspace": {"chat":
//!  [{"agent": "claude", "session": 3, "shown": true}], "panels": ["files", "logs"],
//!  "selected": "logs", "terminal": true}}}
//! {"session": "3", "token": "...", "request": {"type": "profile_get"}}
//! {"session": "3", "token": "...", "request": {"type": "profile_set", "changes": {"agent":
//!  {"model": "sonnet"}}}}
//! {"session": "3", "token": "...", "request": {"type": "html_show", "title": "Build workflow",
//!  "html": "<svg>...</svg>", "height": 320}}
//! {"session": "3", "token": "...", "request": {"type": "html_show", "title": "Build workflow",
//!  "path": "/the/worktree/docs/build.html"}}
//! ```

use std::fmt::Display;
use std::fs::{OpenOptions, Permissions};
use std::hint::black_box;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::random_bytes;
use crate::{Agent, CoreError, Result, SessionId};

/// A panel an agent session asked grove to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelSpec {
    /// The agent panel of a subagent: an agent session grove started in chat mode.
    Agent {
        session: SessionId,
        worktree: PathBuf,
    },
    /// A log panel: the output of a command the agent session ran, which the user only reads.
    Log {
        session: SessionId,
        worktree: PathBuf,
        title: String,
    },
    /// A command panel: a command the agent session ran, in a terminal the user can type into.
    Command {
        session: SessionId,
        worktree: PathBuf,
        title: String,
    },
}

/// The file in a session's folder that holds its token.
const TOKEN_FILE: &str = "token";

/// The secret of one agent session's process. A new process of the session gets a new one. It
/// has no Debug, so it never lands in a log.
pub(crate) struct SessionToken(String);

impl SessionToken {
    /// A new token of 32 random bytes, written to the session's folder `dir`, readable only by
    /// the user.
    pub(crate) fn create(dir: &Path) -> Result<Self> {
        let bytes: [u8; 32] = random_bytes()?;
        let token = Self(bytes.iter().map(|b| format!("{b:02x}")).collect());
        let path = dir.join(TOKEN_FILE);
        write_private(&path, token.0.as_bytes())
            .map_err(CoreError::io("write the session token to", &path))?;
        Ok(token)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `presented` is this token. The time it takes does not depend on where the two
    /// differ.
    pub(crate) fn matches(&self, presented: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), presented.as_bytes())
    }
}

/// Writes `bytes` to `path`, a file that holds a token, readable only by the user.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        // The path is easy to guess, so a symlink there must not send the token elsewhere.
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // The mode above applies only to a new file, and a file left from before keeps its own.
    // Set before the token goes in.
    file.set_permissions(Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}

// Every byte is compared, so the time shows only the length, and every token has 64 digits.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a
        .iter()
        .zip(b)
        .fold(0u8, |diff, (x, y)| black_box(diff | (x ^ y)));
    diff == 0
}

/// Whether a socket message is a panel request. Only those have a `request` field.
pub(crate) fn is_panel_request(message: &Value) -> bool {
    message.get("request").is_some()
}

#[derive(Deserialize)]
struct PanelRequest {
    session: Option<String>,
    token: Option<String>,
    request: Value,
}

/// What a panel request asks for, read only once its token checks out.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Request {
    /// The calling session's id.
    SessionId,
    /// Start a subagent of the calling session, in `worktree` or else its own.
    SubagentSpawn {
        agent: Agent,
        prompt: String,
        worktree: Option<PathBuf>,
    },
    /// Run `cmd` with the user's shell in a new panel in the calling session's worktree.
    PanelOpen {
        kind: CommandPanelKind,
        title: String,
        cmd: String,
    },
    /// The last `lines` lines of one of the calling session's panels, and whether it runs.
    PanelRead { panel: String, lines: usize },
    /// Close one of the calling session's panels and kill its processes.
    PanelClose { panel: String },
    /// The workspace the calling session shows in: its own, or its top parent's.
    WorkspaceGet,
    /// Replace the workspace the calling session shows in. Read by `set_agent_workspace`, so a
    /// workspace that is not valid gets a reply that says why.
    WorkspaceSet { workspace: Value },
    /// The profile of the calling session's repo.
    ProfileGet,
    /// Change the profile of the calling session's repo. Read by `change_agent_profile`, as
    /// `WorkspaceSet` is.
    ProfileSet { changes: Value },
    /// Show a page in the calling session's conversation: `html`, or the `.html` file at `path`
    /// in its worktree. One of the two.
    HtmlShow {
        title: String,
        html: Option<String>,
        path: Option<PathBuf>,
        height: Option<u32>,
    },
}

/// Why a workspace or profile request was refused, where a `CoreError` does not say.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Refused {
    #[error("the workspace is not valid: {0}")]
    BadWorkspace(String),
    #[error("the profile changes are not valid: {0}")]
    BadChanges(String),
    #[error("the repo at {} uses no profile", .0.display())]
    NoProfile(PathBuf),
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// How the UI shows a panel an agent session opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CommandPanelKind {
    Log,
    Command,
}

/// The reply to every request the token check refuses, whatever the reason, so it tells the
/// caller nothing about which part was wrong.
const NOT_AUTHORIZED: &str = "not authorized";

/// The calling session and what panel request `message` asks for, or the reply that refuses
/// it. `authenticate` gives the agent session whose running process has the session id and
/// token the request carries.
pub(crate) fn read(
    message: Value,
    authenticate: impl FnOnce(&str, &str) -> Option<SessionId>,
) -> std::result::Result<(SessionId, Request), Value> {
    let Ok(request) = serde_json::from_value::<PanelRequest>(message) else {
        return Err(error("bad request"));
    };
    let (Some(session), Some(token)) = (request.session, request.token) else {
        return Err(error(NOT_AUTHORIZED));
    };
    let Some(session) = authenticate(&session, &token) else {
        return Err(error(NOT_AUTHORIZED));
    };
    let request = serde_json::from_value(request.request).map_err(|_| error("unknown request"))?;
    Ok((session, request))
}

/// The reply that gives a session its own id.
pub(crate) fn session_id_reply(session: SessionId) -> Value {
    json!({"result": {"session": session.to_string()}})
}

/// The reply to a subagent spawn: the new session and its worktree, or why it did not start.
pub(crate) fn spawned_reply(spawned: Result<(SessionId, PathBuf)>) -> Value {
    match spawned {
        Ok((session, worktree)) => {
            json!({"result": {"session": session.to_string(), "worktree": worktree}})
        }
        Err(e) => error(&e.to_string()),
    }
}

/// The reply to a panel request that `answer` gives the result of.
pub(crate) fn reply<E: Display>(answer: std::result::Result<Value, E>) -> Value {
    match answer {
        Ok(result) => json!({ "result": result }),
        Err(e) => error(&e.to_string()),
    }
}

pub(crate) fn error(message: &str) -> Value {
    json!({ "error": message })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_bytes_match() {
        let token = "a".repeat(64);
        let mut last = token.clone();
        last.replace_range(63.., "b");
        let mut first = token.clone();
        first.replace_range(..1, "b");

        assert!(constant_time_eq(token.as_bytes(), token.as_bytes()));
        assert!(!constant_time_eq(token.as_bytes(), last.as_bytes()));
        assert!(!constant_time_eq(token.as_bytes(), first.as_bytes()));
        assert!(!constant_time_eq(token.as_bytes(), &token.as_bytes()[..63]));
        assert!(!constant_time_eq(token.as_bytes(), b""));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn a_token_matches_itself_and_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let token = SessionToken::create(dir.path()).unwrap();
        let other = SessionToken::create(dir.path()).unwrap();

        assert!(token.matches(token.as_str()));
        assert!(!token.matches(other.as_str()));
        assert!(!token.matches(&token.as_str().to_uppercase()));
    }
}
