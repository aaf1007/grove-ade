//! Asks `codex app-server` whether Codex trusts grove's hooks. The app server speaks
//! JSON-RPC on stdio, one message per line. grove only reads the answer and never approves a hook (ADR 0010).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::shell_env::kill_group;

/// Whether Codex holds back any of grove's hooks until the user approves it in `/hooks`.
/// Codex does so for a hook that is untrusted, or modified since the user approved it.
/// `app_server` runs `codex app-server` with the launch's `-c` overrides, and grove's hooks
/// are the ones that run `hook_command`. False when it gives no answer within `timeout`, so
/// the hooks stay unknown. Blocks for at most `timeout`.
pub(crate) fn hooks_need_approval(
    mut app_server: Command,
    worktree: &Path,
    hook_command: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    let Ok(mut child) = app_server
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own process group, so a kill also ends the git it runs.
        .process_group(0)
        .spawn()
    else {
        return false;
    };
    let (tx, rx) = mpsc::channel();
    if let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) {
        let worktree = worktree.to_path_buf();
        let hook_command = hook_command.to_owned();
        // A server that never answers keeps this thread reading until the kill below.
        let _ = thread::Builder::new()
            .name("grove-codex-app-server".into())
            .spawn(move || {
                let _ = tx.send(ask(stdin, stdout, &worktree, &hook_command));
            });
    }
    let answer = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
        .flatten();
    end(child, deadline);
    answer.unwrap_or(false)
}

/// Sends `initialize`, and once the server answers it, `initialized` and `hooks/list`. None
/// when the server fails or answers something else. Returning drops stdin, and the server
/// exits when its stdin closes.
fn ask(
    mut stdin: ChildStdin,
    stdout: ChildStdout,
    worktree: &Path,
    hook_command: &str,
) -> Option<bool> {
    let mut lines = BufReader::new(stdout).lines().map_while(Result::ok);
    let client = json!({"name": "grove", "version": env!("CARGO_PKG_VERSION")});
    send(
        &mut stdin,
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": client}}),
    )?;
    reply(&mut lines, 1)?;
    send(&mut stdin, json!({"method": "initialized"}))?;
    let cwds = [worktree.to_str()?];
    send(
        &mut stdin,
        json!({"id": 2, "method": "hooks/list", "params": {"cwds": cwds}}),
    )?;
    needs_approval(reply(&mut lines, 2)?, hook_command)
}

/// Whether a `hooks/list` result has one of grove's hooks untrusted or modified. None when it
/// is not a `hooks/list` result.
fn needs_approval(result: Value, hook_command: &str) -> Option<bool> {
    let list: HooksList = serde_json::from_value(result).ok()?;
    Some(
        list.data
            .iter()
            .flat_map(|cwd| &cwd.hooks)
            .any(|hook| hook.is_unapproved_grove_hook(hook_command)),
    )
}

fn send(stdin: &mut ChildStdin, message: Value) -> Option<()> {
    writeln!(stdin, "{message}").ok()
}

/// The result of the reply to request `id`, skipping notifications. Null for an error reply.
fn reply(lines: &mut impl Iterator<Item = String>, id: u64) -> Option<Value> {
    lines.find_map(|line| {
        let mut message: Value = serde_json::from_str(&line).ok()?;
        (message["id"] == id).then(|| message["result"].take())
    })
}

/// Waits for the server to exit until `deadline`, then kills it. It is not reaped before the
/// kill, so its process group id still belongs to it.
fn end(mut child: Child, deadline: Instant) {
    while let Ok(None) = child.try_wait() {
        if Instant::now() >= deadline {
            kill_group(&child);
            let _ = child.wait();
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// The `hooks/list` result: the hooks Codex would run in each folder asked about.
#[derive(Deserialize)]
struct HooksList {
    data: Vec<FolderHooks>,
}

#[derive(Deserialize)]
struct FolderHooks {
    hooks: Vec<Hook>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hook {
    source: String,
    // None for a hook that runs no command, such as an MCP tool.
    command: Option<String>,
    trust_status: String,
}

impl Hook {
    // Codex lists hooks from `-c` overrides as session flags, the repo's as well as grove's.
    fn is_unapproved_grove_hook(&self, hook_command: &str) -> bool {
        self.source == "sessionFlags"
            && self.command.as_deref() == Some(hook_command)
            && matches!(self.trust_status.as_str(), "untrusted" | "modified")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// The `hooks/list` result from a recorded exchange with the real app server.
    fn recorded(trust: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../fixtures/fake-agents/codex-cli/app-server/hooks-list-{trust}.json"
        ));
        let exchange: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        exchange["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|m| (m["server"]["id"] == 2).then(|| m["server"]["result"].clone()))
            .unwrap()
    }

    #[test]
    fn only_untrusted_or_modified_hooks_that_run_grove_hook_need_approval() {
        // The recording ran grove-hook from PATH, and its modified hooks have an extra argument.
        assert_eq!(
            needs_approval(recorded("untrusted"), "grove-hook codex"),
            Some(true)
        );
        assert_eq!(
            needs_approval(recorded("modified"), "grove-hook codex --session x"),
            Some(true)
        );
        assert_eq!(
            needs_approval(recorded("trusted"), "grove-hook codex"),
            Some(false)
        );
        // Hooks from a repo's own -c overrides are session flags too.
        assert_eq!(
            needs_approval(recorded("untrusted"), "'/elsewhere/grove-hook' codex"),
            Some(false)
        );
    }
}
