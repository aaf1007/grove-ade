//! The Codex adapter. Hooks go in `-c` overrides on every launch, so nothing is written to
//! disk (ADR 0010).

mod app_server;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::{
    AgentAdapter, AgentState, ChatSpec, HookEvent, LaunchCtx, Screen, hook_state, shell_quote,
};
use crate::chat::ModelChoice;
use crate::session::{GROVE_SESSION_ID, GROVE_SESSION_TOKEN, GROVE_SOCKET};
use crate::shell_env::Env;
use crate::{Result, SessionId, SpawnSpec};

/// The hook events grove listens to, in the order Codex lists them.
const HOOKS: [&str; 8] = [
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "Interrupt",
];

/// Events whose matcher is checked against the tool name.
const TOOL_EVENTS: [&str; 3] = ["PreToolUse", "PermissionRequest", "PostToolUse"];

/// How long the hook check gets. It runs beside the launch, so this only bounds an app server
/// that hangs. A first run on a fresh Codex home answered in about 320 ms here, and a new
/// binary or a busy machine adds more.
const HOOK_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The tool Codex asks the user a question with.
const QUESTION_TOOL: &str = "request_user_input";

pub(crate) struct Codex {
    pub command: PathBuf,
    pub hook_relay: PathBuf,
    pub mcp_server: PathBuf,
}

impl AgentAdapter for Codex {
    fn launch(&self, ctx: &LaunchCtx) -> Result<SpawnSpec> {
        Ok(SpawnSpec {
            command: self.command.clone(),
            args: self.args(
                ctx.session,
                ctx.resume.then_some(ctx.resume_id),
                ctx.prompt,
                ctx.model,
                ctx.launch_args,
            ),
            cols: ctx.cols,
            rows: ctx.rows,
        })
    }

    // The app server takes no other launch args, and no --no-daemon: it is not the shared daemon.
    fn launch_chat(&self, ctx: &LaunchCtx) -> Result<ChatSpec> {
        Ok(ChatSpec {
            command: self.command.clone(),
            args: self.app_server_args(ctx.session, ctx.launch_args),
            stderr: ctx.dir.join("app-server.log"),
        })
    }

    // Codex applies -c in order, so a key grove sets, or one that holds it such as `hooks`, would
    // replace grove's. Other keys, such as `hooks.Notification`, add to them.
    fn clashing_arg<'a>(&self, launch_args: &'a [String]) -> Option<&'a String> {
        let own: Vec<Vec<&str>> = HOOKS
            .iter()
            .map(|&event| vec!["hooks", event])
            .chain([
                vec!["mcp_servers", "grove"],
                vec!["shell_environment_policy", "set", GROVE_SESSION_ID],
            ])
            .collect();
        for (holder, flag) in config_overrides(launch_args) {
            let (key, value) = flag.split_once('=').unwrap_or((flag, ""));
            let key: Vec<&str> = key
                .split('.')
                .map(|part| part.trim().trim_matches(['"', '\'']))
                .collect();
            let replaces = own
                .iter()
                .any(|own| own.starts_with(&key) || key.starts_with(own));
            // Codex keeps only the variables this list names, after it sets grove's. A pattern
            // that matches GROVE_SESSION_ID, such as GROVE_*, does not count: the list must name
            // it.
            let drops_session_id =
                key == ["shell_environment_policy", "include_only"] && !names_session_id(value);
            if replaces || drops_session_id {
                return Some(holder);
            }
        }
        None
    }

    fn parse_event(&self, event: &HookEvent) -> Option<AgentState> {
        hook_state(event, QUESTION_TOOL)
    }

    fn read_screen(&self, screen: &str) -> Screen {
        let lines: Vec<&str> = screen.lines().map(str::trim).collect();
        let shows = |dialog: &[&str; 2]| {
            dialog
                .iter()
                .all(|start| lines.iter().any(|l| l.starts_with(start)))
        };
        if shows(&HOOKS_REVIEW) || shows(&APPROVAL) {
            return Screen::Dialog;
        }
        // No recording shows a turn that failed, so the prompt never reads as failed. Such a
        // turn fires no hook, and the stuck check shows it as Idle.
        if lines.iter().any(|l| l.starts_with(PROMPT)) {
            Screen::Prompt { error: false }
        } else {
            Screen::Other
        }
    }
}

/// How lines of the "Hooks need review" prompt start. It shows at launch while grove's hooks
/// are new or changed, and no hook runs until the user answers.
const HOOKS_REVIEW: [&str; 2] = ["Hooks need review", "Hooks can run outside the sandbox"];

/// How lines of an approval prompt start, as in the idle-prompt recording.
const APPROVAL: [&str; 2] = [
    "Would you like to",
    "Press enter to confirm or esc to cancel",
];

/// Starts the input line, each prompt sent above it, and the chosen line of a dialog.
const PROMPT: char = '›';

impl Codex {
    /// The arguments for a new conversation, or for going on with the one `resume` names.
    fn args(
        &self,
        session: SessionId,
        resume: Option<&str>,
        prompt: Option<&str>,
        model: Option<&ModelChoice>,
        launch_args: &[String],
    ) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        if let Some(id) = resume {
            args.extend(["resume".into(), id.into()]);
        }
        // On the shared daemon, hooks and MCP servers get the daemon's environment, which has
        // no GROVE_SESSION_ID.
        args.push("--no-daemon".into());
        for flag in self.overrides(session) {
            args.extend(["-c".into(), flag.into()]);
        }
        args.extend(model_args(model, launch_args));
        args.extend(prompt.map(OsString::from));
        args.extend(launch_args.iter().map(OsString::from));
        args
    }

    /// Whether Codex holds back the hooks a launch of `session` gets until the user approves
    /// them in `/hooks`. Asks `codex app-server` with the same `-c` overrides, grove's and then
    /// the repo's, in the worktree and with the session's environment. A repo's overrides can
    /// change which hooks Codex loads, as `features.hooks` does. The app server takes no other
    /// launch args. False when it gives no answer in time. Blocks for at most
    /// `HOOK_CHECK_TIMEOUT`.
    pub(crate) fn hooks_need_approval(
        &self,
        session: SessionId,
        worktree: &Path,
        env: &Env,
        launch_args: &[String],
    ) -> bool {
        let mut app_server = Command::new(&self.command);
        app_server
            .args(self.app_server_args(session, launch_args))
            .current_dir(worktree)
            .env_clear()
            .envs(env);
        app_server::hooks_need_approval(
            app_server,
            worktree,
            &self.hook_command(),
            HOOK_CHECK_TIMEOUT,
        )
    }

    /// The arguments for `codex app-server`: grove's `-c` overrides, then the repo's.
    fn app_server_args(&self, session: SessionId, launch_args: &[String]) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["app-server".into()];
        let repo = config_overrides(launch_args)
            .into_iter()
            .map(|(_, flag)| flag.to_owned());
        for flag in self.overrides(session).into_iter().chain(repo) {
            args.extend(["-c".into(), flag.into()]);
        }
        args
    }

    /// What each of grove's hooks runs.
    fn hook_command(&self) -> String {
        format!("{} codex", shell_quote(&self.hook_relay))
    }

    /// The `-c` overrides. Codex stores the user's approval of each hook against a hash of its
    /// entry, so the hooks are the same on every launch: no session id in them. grove-hook
    /// reads it from Codex's environment instead.
    fn overrides(&self, session: SessionId) -> Vec<String> {
        let command = self.hook_command();
        // The 1 second timeout backs up grove-hook's own 200 ms budget.
        let handler = format!(
            r#"{{type="command",command={},timeout=1}}"#,
            toml_string(&command)
        );
        let mut flags: Vec<String> = HOOKS
            .iter()
            .map(|&event| {
                let matcher = if TOOL_EVENTS.contains(&event) {
                    r#"matcher="*","#
                } else {
                    ""
                };
                format!("hooks.{event}=[{{{matcher}hooks=[{handler}]}}]")
            })
            .collect();
        // grove-mcp reads the session's id, token and socket from Codex's environment, so no
        // token goes in argv.
        flags.push(format!(
            r#"mcp_servers.grove={{command={},args=[],env_vars=["{GROVE_SESSION_ID}","{GROVE_SESSION_TOKEN}","{GROVE_SOCKET}"]}}"#,
            toml_string(&self.mcp_server.to_string_lossy())
        ));
        // Shell commands such as grovecode get the id even under a user policy that drops
        // most of the environment.
        flags.push(format!(
            "shell_environment_policy.set.{GROVE_SESSION_ID}={}",
            toml_string(&session.to_string())
        ));
        flags
    }
}

/// `-m` and the `model_reasoning_effort` override for the session's choice. Each is left out
/// when the repo's launch args set it: Codex refuses a second `-m`, and the repo's override
/// would win anyway.
fn model_args(choice: Option<&ModelChoice>, launch_args: &[String]) -> Vec<OsString> {
    let Some(choice) = choice else {
        return Vec::new();
    };
    let overrides = |wanted: &str| {
        config_overrides(launch_args).into_iter().any(|(_, flag)| {
            let key = flag.split_once('=').map_or(flag, |(key, _)| key);
            key.trim().trim_matches(['"', '\'']) == wanted
        })
    };
    let model_flag = launch_args.iter().any(|arg| {
        arg == "--model"
            || arg.starts_with("--model=")
            || (arg.starts_with("-m") && !arg.starts_with("--"))
    });
    let mut args = Vec::new();
    if !model_flag && !overrides("model") {
        args.extend(["-m".into(), OsString::from(&choice.model)]);
    }
    if let Some(effort) = &choice.effort
        && !overrides("model_reasoning_effort")
    {
        let flag = format!("model_reasoning_effort={}", toml_string(effort));
        args.extend(["-c".into(), flag.into()]);
    }
    args
}

/// The `-c` overrides in `args` as `key=value`, each with the arg that holds it: `-c k=v`,
/// `--config k=v`, `-ck=v`, `-c=k=v` and `--config=k=v`.
fn config_overrides(args: &[String]) -> Vec<(&String, &str)> {
    let mut overrides = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (holder, flag) = if arg == "-c" || arg == "--config" {
            let Some(value) = args.next() else { break };
            (value, value.as_str())
        } else if let Some(value) = arg.strip_prefix("--config=") {
            (arg, value)
        } else if let Some(value) = arg.strip_prefix("-c") {
            (arg, value.strip_prefix('=').unwrap_or(value))
        } else {
            continue;
        };
        overrides.push((holder, flag));
    }
    overrides
}

/// Whether a TOML list such as `["PATH", 'HOME']` names GROVE_SESSION_ID. Codex parses the value
/// as TOML too, so a comment names nothing, and neither does a value that is not a list.
fn names_session_id(value: &str) -> bool {
    let Ok(mut table) = format!("value = {value}").parse::<toml::Table>() else {
        return false;
    };
    let Some(toml::Value::Array(names)) = table.remove("value") else {
        return false;
    };
    names.iter().any(|name| {
        name.as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(GROVE_SESSION_ID))
    })
}

/// A TOML basic string.
fn toml_string(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r"\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use serde_json::{Value, json};

    use super::*;

    fn codex() -> Codex {
        Codex {
            command: "codex".into(),
            hook_relay: "/home/it's/bin/grove-hook".into(),
            mcp_server: "/home/it's/bin/grove-mcp".into(),
        }
    }

    /// Runs the fake Codex with `args` and no steps, and returns what its log says.
    fn run_fake(args: &[OsString]) -> Vec<Value> {
        let dir = tempfile::tempdir().unwrap();
        let scenario = dir.path().join("scenario.json");
        fs::write(&scenario, json!({"steps": []}).to_string()).unwrap();
        let log = dir.path().join("fake.log");
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents/codex");
        let status = Command::new(fake)
            .args(args)
            .env("FAKE_CODEX_SCENARIO", &scenario)
            .env("FAKE_CODEX_LOG", &log)
            .env("HOME", dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn a_resume_goes_on_with_the_id_and_keeps_the_hook_entries_byte_identical() {
        let id = "019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b";
        let launch = codex().args(SessionId(7), None, Some("hi"), None, &[]);
        let resume = codex().args(SessionId(7), Some(id), None, None, &[]);

        assert_eq!(resume[..2], [OsString::from("resume"), OsString::from(id)]);
        assert_eq!(resume[2..], launch[..launch.len() - 1]);
        // The fake takes the resume as the real CLI would, with the same hooks.
        let launched = run_fake(&launch);
        let resumed = run_fake(&resume);
        assert_eq!(resumed[1]["config"], launched[1]["config"]);
        assert_eq!(resumed.last().unwrap()["exit"], 0);
    }

    #[test]
    fn the_app_server_gets_the_launch_overrides_and_only_the_repos_c_overrides() {
        let repo = [
            "-m".to_owned(),
            "o3".to_owned(),
            "-c".to_owned(),
            "model=\"o3\"".to_owned(),
        ];
        let launch = codex().args(SessionId(7), None, None, None, &[]);
        let app_server = codex().app_server_args(SessionId(7), &repo);

        assert_eq!(app_server[0], "app-server");
        // Everything of the launch but --no-daemon, then the repo's -c.
        assert_eq!(app_server[1..app_server.len() - 2], launch[1..]);
        assert_eq!(app_server[app_server.len() - 2..], ["-c", "model=\"o3\""]);
    }

    /// A recorded screen as the headless parser shows it, at the recorded size.
    fn screen(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../fixtures/fake-agents/codex-cli/screens/{name}.ansi"
        ));
        let (replies, _) = std::sync::mpsc::channel();
        let mut terminal = crate::terminal::Terminal::new(120, 40, replies);
        terminal.advance(&fs::read(path).unwrap());
        terminal.screen_text()
    }

    #[test]
    fn the_input_prompt_reads_as_the_prompt() {
        assert_eq!(
            codex().read_screen(&screen("idle-prompt")),
            Screen::Prompt { error: false }
        );
    }

    #[test]
    fn hooks_need_review_and_an_approval_prompt_read_as_dialogs() {
        // The approval prompt as the idle-prompt recording shows it before the user approved.
        let approval = "\
› GROVE_SHELL

• Running touch note.txt

  Would you like to run the following command?

  Environment: local

  Reason: Create note.txt

  $ touch note.txt

› 1. Yes, proceed (y)
  2. Yes, and don't ask again for commands that start with `touch note.txt` (p)
  3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel                                    ⚠ 1 warning · f2 to view";

        assert_eq!(codex().read_screen(&screen("hooks-review")), Screen::Dialog);
        assert_eq!(codex().read_screen(approval), Screen::Dialog);
    }

    #[test]
    fn a_screen_with_no_input_prompt_reads_as_other() {
        let starting = "  >_ OpenAI Codex (v0.158.0)\n\n  Nice place you’ve got here.";
        assert_eq!(codex().read_screen(starting), Screen::Other);
    }

    #[test]
    fn the_model_and_effort_go_in_flags_unless_the_launch_args_set_them() {
        let choice = ModelChoice {
            model: "gpt-5.5-mini".into(),
            effort: Some("low".into()),
        };
        let args = |launch_args: &[&str]| -> Vec<String> {
            let launch_args: Vec<String> = launch_args.iter().map(|&a| a.into()).collect();
            model_args(Some(&choice), &launch_args)
                .into_iter()
                .map(|a| a.into_string().unwrap())
                .collect()
        };
        let model = ["-m", "gpt-5.5-mini"];
        let effort = ["-c", r#"model_reasoning_effort="low""#];

        assert_eq!(args(&[]), [model, effort].concat());
        // Codex refuses a second -m, so any of these leaves grove's out.
        for repo in [
            &["-m", "o3"][..],
            &["-mo3"],
            &["--model", "o3"],
            &["--model=o3"],
            &["-c", r#"model="o3""#],
        ] {
            assert_eq!(args(repo), effort, "{repo:?}");
        }
        assert_eq!(args(&["-c", "model_reasoning_effort=high"]), model);
        assert!(model_args(None, &[]).is_empty());
    }

    #[test]
    fn toml_strings_escape_quotes_backslashes_and_control_characters() {
        assert_eq!(toml_string("a\"b\\c\td"), r#""a\"b\\c\u0009d""#);
    }
}
