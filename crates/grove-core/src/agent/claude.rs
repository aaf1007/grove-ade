//! The Claude Code adapter. Hooks and MCP config go in per-session files passed at launch
//! (ADR 0010).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::{
    AgentAdapter, AgentState, ChatSpec, HookEvent, LaunchCtx, Screen, hook_state, shell_quote,
};
use crate::chat::ModelChoice;
use crate::panel_request::write_private;
use crate::session::{GROVE_SESSION_ID, GROVE_SESSION_TOKEN, GROVE_SOCKET};
use crate::{CoreError, Result, SessionId, SpawnSpec};

/// The hook events grove listens to.
const HOOKS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// grove's settings file, which holds its hooks, and the session id those hooks carry. Given
/// again, the last one wins.
const OWN_FLAGS: [&str; 4] = ["--settings", "--session-id", "--resume", "-r"];

/// The tool Claude asks the user a question with.
const QUESTION_TOOL: &str = "AskUserQuestion";

/// Events whose matcher is checked against the tool name.
const TOOL_EVENTS: [&str; 3] = ["PreToolUse", "PermissionRequest", "PostToolUse"];

/// Every event the recorded CLI took in a settings file (fixtures/fake-agents/claude-code).
const KNOWN_EVENTS: [&str; 10] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStop",
    "SessionEnd",
];

pub(crate) struct ClaudeCode {
    pub command: PathBuf,
    /// The Claude sidecar, for chat mode.
    pub sidecar: PathBuf,
    pub hook_relay: PathBuf,
    pub mcp_server: Option<PathBuf>,
}

impl AgentAdapter for ClaudeCode {
    fn launch(&self, ctx: &LaunchCtx) -> Result<SpawnSpec> {
        let (settings, mcp) = self.write_files(ctx)?;
        // --mcp-config takes several values, so only the = form keeps the prompt apart.
        let mut mcp_flag = OsString::from("--mcp-config=");
        mcp_flag.push(&mcp);
        let session_flag = if ctx.resume {
            "--resume"
        } else {
            "--session-id"
        };
        let mut args = vec![
            "--settings".into(),
            settings.into(),
            mcp_flag,
            session_flag.into(),
            ctx.resume_id.into(),
        ];
        args.extend(model_args(ctx.model, ctx.launch_args));
        args.extend(ctx.prompt.map(OsString::from));
        args.extend(ctx.launch_args.iter().map(OsString::from));
        Ok(SpawnSpec {
            command: self.command.clone(),
            args,
            cols: ctx.cols,
            rows: ctx.rows,
        })
    }

    // sidecar/claude/PROTOCOL.md has these flags.
    fn launch_chat(&self, ctx: &LaunchCtx) -> Result<ChatSpec> {
        let (settings, mcp) = self.write_files(ctx)?;
        let mut args: Vec<OsString> = vec![
            "--settings".into(),
            settings.into(),
            "--mcp-config".into(),
            mcp.into(),
            "--session-id".into(),
            ctx.resume_id.into(),
            "--claude".into(),
            self.command.clone().into(),
        ];
        if ctx.resume {
            args.push("--resume".into());
        }
        Ok(ChatSpec {
            command: self.sidecar.clone(),
            args,
            stderr: ctx.dir.join("sidecar.log"),
        })
    }

    fn clashing_arg<'a>(&self, launch_args: &'a [String]) -> Option<&'a String> {
        launch_args.iter().find(|arg| {
            let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
            OWN_FLAGS.contains(&flag)
        })
    }

    fn parse_event(&self, event: &HookEvent) -> Option<AgentState> {
        hook_state(event, QUESTION_TOOL)
    }

    fn read_screen(&self, screen: &str) -> Screen {
        let lines: Vec<&str> = screen.lines().map(str::trim).collect();
        if TRUST_DIALOG.iter().all(|line| {
            lines
                .iter()
                .any(|l| l.trim_start_matches(PROMPT).trim() == *line)
        }) {
            return Screen::Dialog;
        }
        // The input box is the last two rules, with the prompt on the line after the first.
        let mut rules = (0..lines.len()).rev().filter(|&i| is_rule(lines[i]));
        let (Some(_bottom), Some(top)) = (rules.next(), rules.next()) else {
            return Screen::Other;
        };
        if !lines.get(top + 1).is_some_and(|l| l.starts_with(PROMPT)) {
            return Screen::Other;
        }
        // The last turn starts after the last prompt sent.
        let turn = &lines[..top];
        let start = turn
            .iter()
            .rposition(|l| l.starts_with(PROMPT))
            .map_or(0, |i| i + 1);
        let error = turn[start..].iter().any(|l| l.starts_with(TURN_ERROR));
        Screen::Prompt { error }
    }
}

/// `--model` and `--effort` for the session's choice. Each is left out when the repo's launch
/// args set it, so the repo's choice wins.
fn model_args(choice: Option<&ModelChoice>, launch_args: &[String]) -> Vec<OsString> {
    let Some(choice) = choice else {
        return Vec::new();
    };
    let sets = |flag: &str| {
        launch_args
            .iter()
            .any(|arg| arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag) == flag)
    };
    let mut args = Vec::new();
    if !sets("--model") {
        args.extend(["--model".into(), OsString::from(&choice.model)]);
    }
    if let Some(effort) = &choice.effort
        && !sets("--effort")
    {
        args.extend(["--effort".into(), OsString::from(effort)]);
    }
    args
}

/// Lines of the folder trust dialog, which shows before any hook runs in a folder Claude does
/// not trust yet.
const TRUST_DIALOG: [&str; 2] = ["Accessing workspace:", "Yes, I trust this folder"];

/// Starts the input line, and each prompt sent above it.
const PROMPT: char = '❯';

/// How the CLI shows a turn that ended on an API error, as on the recorded error-prompt
/// screen.
const TURN_ERROR: &str = "⏺ API Error";

/// A full line of box drawing, as above and below the input box.
fn is_rule(line: &str) -> bool {
    !line.is_empty() && line.chars().all(|c| c == '─')
}

impl ClaudeCode {
    /// Writes the session's settings file and MCP config, and returns their paths.
    fn write_files(&self, ctx: &LaunchCtx) -> Result<(PathBuf, PathBuf)> {
        let settings = ctx.dir.join("claude-settings.json");
        write_json(&settings, &self.settings(ctx.session))?;
        // Claude Code ignores a bad settings file without a word, so check it as written.
        let written = fs::read(&settings).map_err(CoreError::io("read", &settings))?;
        check_settings(&written).map_err(|reason| CoreError::BadSettings {
            path: settings.clone(),
            reason,
        })?;
        let mcp = ctx.dir.join("mcp.json");
        // It holds the token, so only the user may read it.
        write_private(&mcp, self.mcp_config(ctx).to_string().as_bytes())
            .map_err(CoreError::io("write", &mcp))?;
        Ok((settings, mcp))
    }

    /// grove-mcp, with the session's id and token and the socket in its own environment. No
    /// server of grove's without grove-mcp.
    fn mcp_config(&self, ctx: &LaunchCtx) -> Value {
        let Some(mcp_server) = &self.mcp_server else {
            return json!({"mcpServers": {}});
        };
        json!({"mcpServers": {"grove": {
            "command": mcp_server,
            "args": [],
            "env": {
                GROVE_SESSION_ID: ctx.session.to_string(),
                GROVE_SESSION_TOKEN: ctx.token,
                GROVE_SOCKET: ctx.socket,
            },
        }}})
    }

    fn settings(&self, session: SessionId) -> Value {
        let command = format!(
            "{} --session {session} claude",
            shell_quote(&self.hook_relay)
        );
        // The 1 second timeout backs up grove-hook's own 200 ms budget.
        let handler = json!({"type": "command", "command": command, "timeout": 1});
        let hooks: Map<String, Value> = HOOKS
            .iter()
            .map(|&event| {
                let group = if TOOL_EVENTS.contains(&event) {
                    json!({"matcher": "*", "hooks": [handler]})
                } else {
                    json!({"hooks": [handler]})
                };
                (event.to_owned(), json!([group]))
            })
            .collect();
        json!({"hooks": hooks})
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, value.to_string()).map_err(CoreError::io("write", path))
}

/// Checks a settings file has the shape the recorded CLI took: known events, each a list of
/// groups with an optional matcher, each hook a command with an optional timeout.
fn check_settings(text: &[u8]) -> std::result::Result<(), String> {
    let settings: Value = serde_json::from_slice(text).map_err(|e| e.to_string())?;
    let hooks = settings
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or("no hooks object")?;
    for (event, groups) in hooks {
        if !KNOWN_EVENTS.contains(&event.as_str()) {
            return Err(format!("unknown hook event {event}"));
        }
        let groups = groups.as_array().ok_or(format!("{event} is not a list"))?;
        for group in groups {
            if group.get("matcher").is_some_and(|m| !m.is_string()) {
                return Err(format!("a {event} matcher is not a string"));
            }
            let handlers = group
                .get("hooks")
                .and_then(Value::as_array)
                .ok_or(format!("a {event} group has no hooks list"))?;
            for handler in handlers {
                let is_command = handler["type"] == "command"
                    && handler["command"].as_str().is_some_and(|c| !c.is_empty());
                let timeout_ok = handler
                    .get("timeout")
                    .is_none_or(|t| t.as_f64().is_some_and(|t| t > 0.0));
                if !is_command || !timeout_ok {
                    return Err(format!("a {event} hook is not a command hook: {handler}"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(settings: Value) -> std::result::Result<(), String> {
        check_settings(settings.to_string().as_bytes())
    }

    #[test]
    fn the_settings_grove_writes_pass_the_check() {
        let claude = ClaudeCode {
            command: "claude".into(),
            sidecar: "claude-sidecar".into(),
            hook_relay: "/home/it's/bin/grove-hook".into(),
            mcp_server: None,
        };
        assert_eq!(check(claude.settings(SessionId(7))), Ok(()));
    }

    #[test]
    fn a_bad_settings_file_fails_the_check() {
        let hook = json!({"type": "command", "command": "grove-hook"});
        assert!(check_settings(b"{\"hooks\": ").is_err());
        assert!(check(json!({})).is_err());
        assert!(check(json!({"hooks": {"Stopp": [{"hooks": [hook]}]}})).is_err());
        assert!(check(json!({"hooks": {"Stop": {"hooks": [hook]}}})).is_err());
        assert!(check(json!({"hooks": {"Stop": [{"matcher": 1, "hooks": [hook]}]}})).is_err());
        assert!(check(json!({"hooks": {"Stop": [{"hooks": [{"type": "prompt"}]}]}})).is_err());
        assert!(check(json!({"hooks": {"Stop": [{"hooks": [{"type": "command"}]}]}})).is_err());
        let slow = json!({"type": "command", "command": "grove-hook", "timeout": "1"});
        assert!(check(json!({"hooks": {"Stop": [{"hooks": [slow]}]}})).is_err());
    }

    fn read(screen: &str) -> Screen {
        let claude = ClaudeCode {
            command: "claude".into(),
            sidecar: "claude-sidecar".into(),
            hook_relay: "grove-hook".into(),
            mcp_server: None,
        };
        claude.read_screen(screen)
    }

    #[test]
    fn only_an_error_in_the_last_turn_fails_the_prompt() {
        let rule = "─".repeat(40);
        let earlier = format!(
            "❯ first\n⏺ API Error: 500 overloaded\n❯ second\n⏺ Done.\n{rule}\n❯\n{rule}\n  ? for shortcuts"
        );
        let last =
            format!("❯ first\n⏺ Done.\n❯ second\n⏺ API Error: 500 overloaded\n{rule}\n❯\n{rule}");
        let tool_error =
            format!("❯ first\n  ⎿  Error: exit code 1\n⏺ Fixed it.\n{rule}\n❯\n{rule}");

        assert_eq!(read(&earlier), Screen::Prompt { error: false });
        assert_eq!(read(&last), Screen::Prompt { error: true });
        assert_eq!(read(&tool_error), Screen::Prompt { error: false });
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn the_model_and_effort_go_in_flags_unless_the_launch_args_set_them() {
        let choice = ModelChoice {
            model: "haiku".into(),
            effort: Some("low".into()),
        };
        let args = |launch_args: &[&str]| {
            let launch_args: Vec<String> = launch_args.iter().map(|&a| a.into()).collect();
            strings(model_args(Some(&choice), &launch_args))
        };

        assert_eq!(args(&[]), ["--model", "haiku", "--effort", "low"]);
        assert_eq!(args(&["--model", "opus"]), ["--effort", "low"]);
        assert_eq!(args(&["--effort=max"]), ["--model", "haiku"]);
        assert!(model_args(None, &[]).is_empty());
    }

    #[test]
    fn the_hook_command_quotes_the_relay_path() {
        assert_eq!(
            shell_quote(Path::new("/home/it's/grove-hook")),
            r"'/home/it'\''s/grove-hook'"
        );
    }
}
