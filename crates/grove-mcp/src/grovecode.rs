//! grovecode: the grove-mcp tools as shell commands, for agents that prefer the shell. Run in an
//! agent session's shell, it acts as that session, with the id, token and socket grove put in
//! the session's environment.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use grove_mcp::CoreClient;
use grove_mcp::tools::{self, AgentName, PanelKind, PanelState};
use serde_json::Value;

const USAGE: &str = "\
Usage:
  grovecode subagent spawn <claude|codex> <prompt> [--worktree <path>]
  grovecode panel open <log|command> <title> <cmd>
  grovecode panel read <id> [--lines <n>]
  grovecode panel wait <id> exited [--timeout-seconds <n>]
  grovecode panel close <id>
  grovecode workspace get
  grovecode workspace set <json>
  grovecode profile get
  grovecode profile set <json>

Run it in the shell of an agent session that grove started, and it acts as that session. It
prints what happened and exits 0, or prints why not to stderr and exits 1. A command it cannot
parse exits 2. Put -- before an argument that starts with --. A relative --worktree is taken
from the current folder. workspace set takes the whole workspace as workspace get prints it,
and profile set the settings to change, as profile get prints them under settings.
";

/// A command as typed, checked as far as it can be without grove.
enum Call {
    SubagentSpawn {
        agent: AgentName,
        prompt: String,
        worktree: Option<PathBuf>,
    },
    PanelOpen {
        kind: PanelKind,
        title: String,
        cmd: String,
    },
    PanelRead {
        id: String,
        lines: Option<u64>,
    },
    PanelWait {
        id: String,
        state: PanelState,
        timeout_seconds: Option<u64>,
    },
    PanelClose {
        id: String,
    },
    WorkspaceGet,
    WorkspaceSet {
        workspace: Value,
    },
    ProfileGet,
    ProfileSet {
        changes: Value,
    },
}

fn main() -> ExitCode {
    let call = match parse(env::args_os().skip(1).collect()) {
        Ok(Some(call)) => call,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprint!("grovecode: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let core = match CoreClient::from_env() {
        Ok(core) => core,
        Err(error) => {
            eprintln!(
                "grovecode: {error}. grovecode works only in the shell of an agent session that \
                 grove started."
            );
            return ExitCode::FAILURE;
        }
    };
    match run(&core, call) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(text) => {
            eprintln!("grovecode: {text}");
            ExitCode::FAILURE
        }
    }
}

fn run(core: &CoreClient, call: Call) -> Result<String, String> {
    match call {
        Call::SubagentSpawn {
            agent,
            prompt,
            worktree,
        } => tools::subagent_spawn(core, agent, &prompt, worktree.as_deref()),
        Call::PanelOpen { kind, title, cmd } => tools::panel_open(core, kind, &title, &cmd),
        Call::PanelRead { id, lines } => tools::panel_read(core, &id, lines),
        // Nothing cancels it but the shell's own Ctrl-C, which ends grovecode.
        Call::PanelWait {
            id,
            state,
            timeout_seconds,
        } => tools::panel_wait(core, &id, state, timeout_seconds, || false),
        Call::PanelClose { id } => tools::panel_close(core, &id),
        Call::WorkspaceGet => tools::workspace_get(core),
        Call::WorkspaceSet { workspace } => tools::workspace_set(core, workspace),
        Call::ProfileGet => tools::profile_get(core),
        Call::ProfileSet { changes } => tools::profile_set(core, changes),
    }
}

/// The command `args` name, or None for help.
fn parse(args: Vec<std::ffi::OsString>) -> Result<Option<Call>, String> {
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| format!("{} is not UTF-8", arg.display()))
        })
        .collect::<Result<Vec<String>, String>>()?;
    // Only as the first argument, so a prompt or a command can be -h.
    if matches!(args.first().map(String::as_str), Some("-h" | "--help")) {
        return Ok(None);
    }
    let mut words = Vec::new();
    // An option's value is None when nothing follows it.
    let mut options: Vec<(String, Option<String>)> = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            words.extend(args.by_ref());
            break;
        }
        match arg.strip_prefix("--") {
            Some(option) => options.push(match option.split_once('=') {
                Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
                None => (option.to_owned(), args.next()),
            }),
            None => words.push(arg),
        }
    }

    let command: Vec<&str> = words.iter().take(2).map(String::as_str).collect();
    let (name, takes, allowed): (&str, usize, &[&str]) = match command[..] {
        [] => return Err("no command".into()),
        ["subagent", "spawn"] => ("subagent spawn", 2, &["worktree"]),
        ["panel", "open"] => ("panel open", 3, &[]),
        ["panel", "read"] => ("panel read", 1, &["lines"]),
        ["panel", "wait"] => ("panel wait", 2, &["timeout-seconds"]),
        ["panel", "close"] => ("panel close", 1, &[]),
        ["workspace", "get"] => ("workspace get", 0, &[]),
        ["workspace", "set"] => ("workspace set", 1, &[]),
        ["profile", "get"] => ("profile get", 0, &[]),
        ["profile", "set"] => ("profile set", 1, &[]),
        _ => return Err(format!("unknown command: {}", command.join(" "))),
    };
    for (option, value) in &options {
        if !allowed.contains(&option.as_str()) {
            return Err(format!("{name} takes no --{option}"));
        }
        if value.is_none() {
            return Err(format!("--{option} needs a value"));
        }
    }
    let mut rest = words.into_iter().skip(2);
    if rest.len() != takes {
        let plural = if takes == 1 { "" } else { "s" };
        return Err(format!("{name} takes {takes} argument{plural}"));
    }
    let mut next = || rest.next().unwrap_or_default();
    // The last one counts when an option comes twice.
    let option = |wanted: &str| {
        options
            .iter()
            .rev()
            .find(|(o, _)| o == wanted)
            .and_then(|(_, value)| value.clone())
    };
    let number = |wanted: &str| {
        option(wanted)
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| format!("--{wanted} takes a whole number, not {value}"))
            })
            .transpose()
    };

    // `not_json` says what is not JSON, as in "the workspace is".
    let json = |not_json: &str, text: String| {
        serde_json::from_str::<Value>(&text).map_err(|e| format!("{not_json} not JSON: {e}"))
    };

    let call = match name {
        "subagent spawn" => Call::SubagentSpawn {
            agent: match next().as_str() {
                "claude" => AgentName::Claude,
                "codex" => AgentName::Codex,
                other => return Err(format!("the agent is claude or codex, not {other}")),
            },
            prompt: next(),
            worktree: option("worktree").map(worktree).transpose()?,
        },
        "panel open" => Call::PanelOpen {
            kind: match next().as_str() {
                "log" => PanelKind::Log,
                "command" => PanelKind::Command,
                other => return Err(format!("the kind is log or command, not {other}")),
            },
            title: next(),
            cmd: next(),
        },
        "panel read" => Call::PanelRead {
            id: next(),
            lines: number("lines")?,
        },
        "panel wait" => Call::PanelWait {
            id: next(),
            state: match next().as_str() {
                "exited" => PanelState::Exited,
                other => return Err(format!("the state is exited, not {other}")),
            },
            timeout_seconds: number("timeout-seconds")?,
        },
        "panel close" => Call::PanelClose { id: next() },
        "workspace get" => Call::WorkspaceGet,
        "workspace set" => Call::WorkspaceSet {
            workspace: json("the workspace is", next())?,
        },
        "profile get" => Call::ProfileGet,
        _ => Call::ProfileSet {
            changes: json("the changes are", next())?,
        },
    };
    Ok(Some(call))
}

/// The worktree `path` names. grove matches the path exactly and tracks worktrees by their real
/// path, so a relative one is resolved from the current folder, through `..` and symlinks. An
/// absolute one goes as given, as grove-mcp sends it.
fn worktree(path: String) -> Result<PathBuf, String> {
    if Path::new(&path).is_absolute() {
        return Ok(PathBuf::from(path));
    }
    fs::canonicalize(&path).map_err(|e| format!("--worktree {path}: {e}"))
}
