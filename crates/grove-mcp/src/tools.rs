//! What each tool does, shared by the `grove-mcp` tools and the `grovecode` commands: it checks
//! the call, asks the core, and says what happened. `Ok` is the text of a call that worked,
//! `Err` the text of one that failed.
//!
//! These block on the socket, so an async caller runs them off its async thread.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use rmcp::schemars;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::CoreClient;

// Inlined, so the schema has no $ref for a client to follow.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(inline)]
pub enum AgentName {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(inline)]
pub enum PanelKind {
    Log,
    Command,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(inline)]
pub enum PanelState {
    Exited,
}

/// Lines `panel_read` returns when the call names no number.
pub const DEFAULT_READ_LINES: u64 = 50;
pub const DEFAULT_WAIT: u64 = 60;
pub const MAX_WAIT: u64 = 600;
/// How often `panel_wait` asks grove whether the panel's command has exited.
const WAIT_POLL: Duration = Duration::from_millis(200);
/// The most bytes an `html_show` request has. grove's socket takes 64 KiB a message, and the
/// session's id and token ride with the request. A larger page goes in a file.
const MAX_HTML_SHOW_REQUEST_BYTES: usize = 60 * 1024;

pub fn subagent_spawn(
    core: &CoreClient,
    agent: AgentName,
    prompt: &str,
    worktree: Option<&Path>,
) -> Result<String, String> {
    let agent = match agent {
        AgentName::Claude => "claude",
        AgentName::Codex => "codex",
    };
    let mut request = json!({"type": "subagent_spawn", "agent": agent, "prompt": prompt});
    if let Some(worktree) = worktree {
        request["worktree"] = json!(worktree);
    }
    let result = core.request(request).map_err(|e| e.to_string())?;
    let session = result["session"].as_str().unwrap_or_default();
    let worktree = result["worktree"].as_str().unwrap_or_default();
    Ok(format!("Started subagent session {session} in {worktree}."))
}

pub fn panel_open(
    core: &CoreClient,
    kind: PanelKind,
    title: &str,
    cmd: &str,
) -> Result<String, String> {
    if cmd.trim().is_empty() {
        return Err("A panel needs a command.".into());
    }
    let kind = match kind {
        PanelKind::Log => "log",
        PanelKind::Command => "command",
    };
    let request = json!({"type": "panel_open", "kind": kind, "title": title, "cmd": cmd});
    let result = core.request(request).map_err(|e| e.to_string())?;
    let panel = result["panel"].as_str().unwrap_or_default();
    Ok(format!("Opened panel {panel}."))
}

pub fn panel_read(core: &CoreClient, id: &str, lines: Option<u64>) -> Result<String, String> {
    check_panel_id(id)?;
    let lines = lines.unwrap_or(DEFAULT_READ_LINES);
    let request = json!({"type": "panel_read", "panel": id, "lines": lines});
    let result = core.request(request).map_err(|e| e.to_string())?;
    let mut text = panel_state(id, &result);
    let lines: Vec<&str> = result["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !lines.is_empty() {
        text = format!("{text}\n\n{}", lines.join("\n"));
    }
    Ok(text)
}

/// Waits until the panel's command has exited, for at most `timeout_seconds`, or until
/// `cancelled` says nobody waits for the answer any more.
pub fn panel_wait(
    core: &CoreClient,
    id: &str,
    state: PanelState,
    timeout_seconds: Option<u64>,
    cancelled: impl Fn() -> bool,
) -> Result<String, String> {
    // The one state there is.
    let PanelState::Exited = state;
    check_panel_id(id)?;
    let timeout = timeout_seconds.unwrap_or(DEFAULT_WAIT);
    if timeout > MAX_WAIT {
        return Err(format!("timeout_seconds is at most {MAX_WAIT}."));
    }
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let still_running = || format!("Panel {id} is still running after {timeout} s.");
    loop {
        let request = json!({"type": "panel_read", "panel": id, "lines": 0});
        let result = core.request(request).map_err(|e| e.to_string())?;
        if result["running"] == false {
            return Ok(panel_state(id, &result));
        }
        if Instant::now() >= deadline {
            return Err(still_running());
        }
        thread::sleep(WAIT_POLL);
        // Checked before each read, so a wait cancelled while it slept asks grove no more.
        if cancelled() {
            return Err(still_running());
        }
    }
}

pub fn panel_close(core: &CoreClient, id: &str) -> Result<String, String> {
    check_panel_id(id)?;
    core.request(json!({"type": "panel_close", "panel": id}))
        .map_err(|e| e.to_string())?;
    Ok(format!("Closed panel {id}."))
}

pub fn workspace_get(core: &CoreClient) -> Result<String, String> {
    let result = core
        .request(json!({"type": "workspace_get"}))
        .map_err(|e| e.to_string())?;
    Ok(pretty(&result))
}

/// `workspace` is the whole workspace, in the shape `workspace_get` gives. grove checks it.
pub fn workspace_set(core: &CoreClient, workspace: Value) -> Result<String, String> {
    core.request(json!({"type": "workspace_set", "workspace": workspace}))
        .map_err(|e| e.to_string())?;
    Ok("Changed the workspace.".into())
}

pub fn profile_get(core: &CoreClient) -> Result<String, String> {
    let result = core
        .request(json!({"type": "profile_get"}))
        .map_err(|e| e.to_string())?;
    Ok(pretty(&result))
}

/// `changes` holds the settings to change, in the shape of `profile_get`'s `settings`. grove
/// checks them.
pub fn profile_set(core: &CoreClient, changes: Value) -> Result<String, String> {
    let result = core
        .request(json!({"type": "profile_set", "changes": changes}))
        .map_err(|e| e.to_string())?;
    let profile = result["profile"].as_str().unwrap_or_default();
    Ok(format!("Changed profile {profile}."))
}

/// Shows a page in the calling session's conversation: `html`, or the `.html` file at `path`.
/// grove checks that it is one of the two, and that the path is inside the session's worktree.
pub fn html_show(
    core: &CoreClient,
    title: &str,
    html: Option<&str>,
    path: Option<&Path>,
    height: Option<u32>,
) -> Result<String, String> {
    let mut request = json!({"type": "html_show", "title": title});
    if let Some(html) = html {
        request["html"] = json!(html);
    }
    if let Some(path) = path {
        request["path"] = json!(path);
    }
    if let Some(height) = height {
        request["height"] = json!(height);
    }
    if request.to_string().len() > MAX_HTML_SHOW_REQUEST_BYTES {
        return Err(
            "The page is too large to send inline. Write it to an .html file in the worktree \
             and pass its path."
                .into(),
        );
    }
    let result = core.request(request).map_err(|e| e.to_string())?;
    let page = result["page"].as_str().unwrap_or_default();
    // grove finds the page by this id when the conversation resumes, so the text must name it.
    Ok(format!("Showed the page in the conversation as {page}."))
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

// Panel ids are session ids, which are numbers.
fn check_panel_id(id: &str) -> Result<(), String> {
    match id.parse::<u64>() {
        Ok(_) => Ok(()),
        Err(_) => Err(format!("There is no panel {id}.")),
    }
}

/// Whether a panel runs or how it exited, from a panel_read result.
fn panel_state(id: &str, result: &Value) -> String {
    if result["running"] != false {
        format!("Panel {id} is running.")
    } else if let Some(code) = result["code"].as_i64() {
        format!("Panel {id} exited with code {code}.")
    } else {
        format!("Panel {id} was ended by a signal.")
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn a_wait_cancelled_between_reads_asks_grove_no_more() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("grove.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        // A core that says the panel runs. Left blocked in accept when the test ends.
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut message = Vec::new();
                stream.read_to_end(&mut message).unwrap();
                counted.fetch_add(1, Ordering::SeqCst);
                let reply = json!({"result": {"lines": [], "running": true, "code": null}});
                let _ = stream.write_all(reply.to_string().as_bytes());
            }
        });
        let core = CoreClient {
            socket,
            session_id: "1".into(),
            session_token: "t".into(),
            timeout: Duration::from_secs(10),
        };
        let started = Instant::now();
        // The cancel arrives while the wait sleeps between its reads.
        let cancelled = || started.elapsed() >= WAIT_POLL / 2;

        let waited = panel_wait(&core, "7", PanelState::Exited, Some(60), cancelled);

        assert_eq!(waited, Err("Panel 7 is still running after 60 s.".into()));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
}
