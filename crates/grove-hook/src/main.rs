//! The hook relay. It sends one agent hook payload to the core socket with the session id.

use std::env;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::panic;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Map, Value, json};

/// The payload fields grove keeps: the event, the agent's own session id, and the
/// field each event's hook matcher reads. grove-hook drops the rest.
const KEPT: [&str; 8] = [
    "hook_event_name",
    "session_id",
    "tool_name",
    "source",
    "reason",
    "notification_type",
    "error",
    "agent_type",
];

/// Time to connect to the core and write the message. The agent waits on the hook,
/// so a core that is stuck must not hold it.
const BUDGET: Duration = Duration::from_millis(200);

// Every failure ends the same way: exit 0 with no output, so the agent never notices.
// Claude reads hook output as context, and exit 2 blocks the tool call.
fn main() {
    panic::set_hook(Box::new(|_| {}));
    let (connecting, worker) = mpsc::channel();
    let spawned = thread::Builder::new().spawn(move || relay(connecting));
    // The worker sends one message when it starts to connect. When it ends, its sender
    // drops and both waits return at once.
    if spawned.is_ok() && worker.recv().is_ok() {
        // Returning from main ends the process, which cuts off a stuck connect or write.
        let _ = worker.recv_timeout(BUDGET);
    }
}

/// Reads the hook payload on stdin and writes one message to the core socket.
fn relay(connecting: Sender<()>) -> anyhow::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    let (agent, session) = match &args[..] {
        [flag, session, agent] if flag == "--session" && agent == "claude" => {
            ("claude", session.clone())
        }
        [agent] if agent == "codex" => ("codex", env::var("GROVE_SESSION_ID")?),
        _ => anyhow::bail!("unknown command line"),
    };
    anyhow::ensure!(!session.is_empty(), "empty session id");

    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let mut event: Map<String, Value> = serde_json::from_slice(&input)?;
    event.retain(|key, _| KEPT.contains(&key.as_str()));

    let mut line =
        serde_json::to_vec(&json!({"session": session, "agent": agent, "event": event}))?;
    line.push(b'\n');
    let socket = match env::var_os("GROVE_SOCKET") {
        Some(path) => PathBuf::from(path),
        None => env::home_dir()
            .context("no home folder")?
            .join(".grovecode/grove.sock"),
    };
    connecting.send(())?;
    UnixStream::connect(socket)?.write_all(&line)?;
    Ok(())
}
