//! The grovecode CLI: the grove-mcp tools as shell commands, run as a process. Inside an agent
//! session's shell it acts as that session. Outside one it refuses.

mod common;

use std::process::Output;

use common::*;

fn grovecode() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_grovecode"))
}

/// Runs grovecode with `env` only.
fn run(env: &[(&str, &str)], args: &[&str]) -> Output {
    run_in(Path::new("/"), env, args)
}

/// Runs grovecode in the folder `dir`, with `env` only.
fn run_in(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    Command::new(grovecode())
        .current_dir(dir)
        .env_clear()
        .envs(env.iter().copied())
        .args(args)
        .output()
        .unwrap()
}

/// Exit code, stdout and stderr.
fn outcome(output: &Output) -> (Option<i32>, String, String) {
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

const NO_CORE: &str = "/tmp/grovecode-test-no-core.sock";

const SESSION: [(&str, &str); 3] = [
    ("GROVE_SESSION_ID", "3"),
    ("GROVE_SESSION_TOKEN", "a-token"),
    ("GROVE_SOCKET", NO_CORE),
];

/// grovecode in an agent session's shell: its id, token and socket.
struct Shell {
    socket: PathBuf,
    session: SessionId,
    token: String,
}

impl Shell {
    fn of(s: &Setup, agent: Agent) -> (SessionId, Self) {
        let session = s.spawn_parent(agent);
        let token = s.token(fake_name(agent), session);
        let shell = Self {
            socket: s.cfg.socket.clone(),
            session,
            token,
        };
        (session, shell)
    }

    fn run(&self, args: &[&str]) -> (Option<i32>, String, String) {
        self.run_in(Path::new("/"), args)
    }

    fn run_in(&self, dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
        let session = self.session.to_string();
        let socket = self.socket.to_str().unwrap();
        outcome(&run_in(
            dir,
            &[
                ("GROVE_SESSION_ID", &session),
                ("GROVE_SESSION_TOKEN", &self.token),
                ("GROVE_SOCKET", socket),
            ],
            args,
        ))
    }

    /// Runs a command that must succeed, and returns what it printed.
    fn ok(&self, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.run(args);
        assert_eq!(code, Some(0), "{args:?}: {stderr}");
        assert_eq!(stderr, "", "{args:?}");
        stdout
    }

    /// Opens a panel that must open, and returns its id.
    fn open(&self, kind: &str, title: &str, cmd: &str) -> String {
        let stdout = self.ok(&["panel", "open", kind, title, cmd]);
        let id = stdout
            .strip_prefix("Opened panel ")
            .and_then(|rest| rest.strip_suffix(".\n"))
            .unwrap_or_else(|| panic!("{stdout}"));
        id.to_owned()
    }
}

fn fake_name(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
    }
}

#[test]
fn outside_an_agent_session_it_fails_with_a_clear_message() {
    // Not even HOME, from which the socket path would come.
    for env in [&[][..], &[("HOME", "/tmp")][..]] {
        let (code, stdout, stderr) = outcome(&run(env, &["panel", "read", "7"]));

        assert_eq!(code, Some(1), "{stderr}");
        assert_eq!(stdout, "");
        assert_eq!(
            stderr,
            "grovecode: GROVE_SESSION_ID is not set, so grove did not start this agent session. \
             grovecode works only in the shell of an agent session that grove started.\n"
        );
    }
}

#[test]
fn help_lists_every_command_even_outside_an_agent_session() {
    for help in ["--help", "-h"] {
        let (code, stdout, stderr) = outcome(&run(&[], &[help]));

        assert_eq!(code, Some(0), "{help}: {stderr}");
        for command in [
            "grovecode subagent spawn <claude|codex> <prompt> [--worktree <path>]",
            "grovecode panel open <log|command> <title> <cmd>",
            "grovecode panel read <id> [--lines <n>]",
            "grovecode panel wait <id> exited [--timeout-seconds <n>]",
            "grovecode panel close <id>",
            "grovecode workspace get",
            "grovecode workspace set <json>",
            "grovecode profile get",
            "grovecode profile set <json>",
            "grovecode html show <title> (--html <html> | --path <path>) [--height <n>]",
        ] {
            assert!(stdout.contains(command), "{help}: {command} in {stdout}");
        }
    }
}

#[test]
fn a_prompt_or_command_that_is_dash_h_goes_to_grove_not_to_help() {
    for args in [
        &["subagent", "spawn", "claude", "-h"][..],
        &["panel", "open", "log", "t", "-h"],
    ] {
        // No core runs, so a call that reached grove says so.
        let (code, stdout, stderr) = outcome(&run(&SESSION, args));

        assert_eq!(code, Some(1), "{args:?}: {stderr}");
        assert_eq!(stdout, "", "{args:?}");
        assert!(
            stderr.starts_with("grovecode: grove is not running"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn a_command_it_cannot_parse_exits_2_with_the_usage() {
    let cases: [(&[&str], &str); 17] = [
        (&[], "no command"),
        (&["html", "show"], "html show takes 1 argument"),
        (
            &["html", "show", "Chart", "--height", "tall", "--html", "<p>"],
            "--height takes a whole number, not tall",
        ),
        (
            &["html", "show", "Chart", "--lines", "3"],
            "html show takes no --lines",
        ),
        (&["panel", "shut", "7"], "unknown command: panel shut"),
        (&["panel", "close"], "panel close takes 1 argument"),
        (
            &["panel", "close", "7", "8"],
            "panel close takes 1 argument",
        ),
        (
            &["subagent", "spawn", "gpt", "hi"],
            "the agent is claude or codex, not gpt",
        ),
        (
            &["panel", "read", "7", "--lines", "many"],
            "--lines takes a whole number, not many",
        ),
        (
            &["panel", "close", "7", "--lines", "3"],
            "panel close takes no --lines",
        ),
        // An unknown option at the end has no value, and is still unknown.
        (
            &["panel", "close", "7", "--force"],
            "panel close takes no --force",
        ),
        (
            &["panel", "close", "7", "--help"],
            "panel close takes no --help",
        ),
        (&["panel", "read", "7", "--lines"], "--lines needs a value"),
        (
            &["subagent", "spawn", "claude", "hi", "--worktree"],
            "--worktree needs a value",
        ),
        (
            &["workspace", "get", "now"],
            "workspace get takes 0 arguments",
        ),
        (
            &["workspace", "set", "{"],
            "the workspace is not JSON: EOF while parsing an object at line 1 column 1",
        ),
        (
            &["profile", "set", "model=sonnet"],
            "the changes are not JSON: expected value at line 1 column 1",
        ),
    ];
    for (args, error) in cases {
        let (code, stdout, stderr) = outcome(&run(&SESSION, args));

        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert_eq!(stdout, "", "{args:?}");
        assert!(
            stderr.starts_with(&format!("grovecode: {error}\n\nUsage:\n")),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn calls_that_cannot_be_right_are_refused_before_asking_grove_as_grove_mcp_refuses_them() {
    let cases: [(&[&str], &str); 4] = [
        (
            &["panel", "open", "log", "t", "  "],
            "A panel needs a command.",
        ),
        (
            &["panel", "wait", "7", "exited", "--timeout-seconds", "601"],
            "timeout_seconds is at most 600.",
        ),
        (&["panel", "read", "seven"], "There is no panel seven."),
        (&["panel", "close", "seven"], "There is no panel seven."),
    ];
    for (args, refusal) in cases {
        // No core runs, so a call that reached grove would say so.
        let (code, stdout, stderr) = outcome(&run(&SESSION, args));

        assert_eq!(code, Some(1), "{args:?}: {stderr}");
        assert_eq!(stdout, "", "{args:?}");
        assert_eq!(stderr, format!("grovecode: {refusal}\n"), "{args:?}");
    }
}

#[test]
fn with_grove_not_running_it_says_so() {
    let (code, _, stderr) = outcome(&run(&SESSION, &["subagent", "spawn", "codex", "hi"]));

    assert_eq!(code, Some(1));
    assert!(
        stderr.starts_with(&format!(
            "grovecode: grove is not running: nothing answers on {NO_CORE}"
        )),
        "{stderr}"
    );
}

#[test]
fn it_opens_waits_on_reads_and_closes_a_panel_as_its_session() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (owner, shell) = Shell::of(&s, Agent::Claude);

    let id = shell.open("command", "dev server", "pwd; echo hello");

    let (parent, panel) = s.next_panel_request();
    assert_eq!(parent, Some(owner));
    let PanelSpec::Command {
        worktree, title, ..
    } = panel
    else {
        panic!("not a command panel");
    };
    assert_eq!(
        (worktree, title.as_str()),
        (s.worktree.clone(), "dev server")
    );
    assert_eq!(
        shell.ok(&["panel", "wait", &id, "exited", "--timeout-seconds", "10"]),
        format!("Panel {id} exited with code 0.\n")
    );
    assert_eq!(
        shell.ok(&["panel", "read", &id, "--lines=10"]),
        format!(
            "Panel {id} exited with code 0.\n\n{}\nhello\n",
            s.worktree.display()
        )
    );
    assert_eq!(
        shell.ok(&["panel", "close", &id]),
        format!("Closed panel {id}.\n")
    );
    let (code, _, stderr) = shell.run(&["panel", "read", &id]);
    assert_eq!(code, Some(1));
    assert_eq!(
        stderr,
        format!("grovecode: grove refused: session {owner} has no panel {id}\n")
    );
}

#[test]
fn a_log_panel_is_asked_for_as_a_log_and_read_defaults_to_the_last_50_lines() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, shell) = Shell::of(&s, Agent::Codex);

    let id = shell.open("log", "count", "seq 1 300");

    let (_, PanelSpec::Log { title, .. }) = s.next_panel_request() else {
        panic!("not a log panel");
    };
    assert_eq!(title, "count");
    shell.ok(&["panel", "wait", &id, "exited"]);
    let read = shell.ok(&["panel", "read", &id]);
    let lines: Vec<String> = (251..=300).map(|n| n.to_string()).collect();
    assert_eq!(
        read,
        format!("Panel {id} exited with code 0.\n\n{}\n", lines.join("\n"))
    );
}

#[test]
fn a_wait_that_times_out_fails_and_the_panel_reads_as_running() {
    let s = setup(json!([{"wait_input": "\r"}]));
    let (_, shell) = Shell::of(&s, Agent::Claude);
    let id = shell.open("command", "server", "sleep 600");

    let (code, stdout, stderr) =
        shell.run(&["panel", "wait", &id, "exited", "--timeout-seconds", "1"]);

    assert_eq!(code, Some(1));
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        format!("grovecode: Panel {id} is still running after 1 s.\n")
    );
    assert_eq!(
        shell.ok(&["panel", "read", &id, "--lines", "0"]),
        format!("Panel {id} is running.\n")
    );
}

#[test]
fn it_cannot_act_on_another_sessions_panel() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, owner) = Shell::of(&s, Agent::Claude);
    let (other, stranger) = Shell::of(&s, Agent::Codex);
    let id = owner.open("command", "server", "sleep 600");
    s.next_panel_request();

    let refusal = format!("grovecode: grove refused: session {other} has no panel {id}\n");
    for args in [
        &["panel", "read", &id][..],
        &["panel", "wait", &id, "exited", "--timeout-seconds", "1"],
        &["panel", "close", &id],
    ] {
        assert_eq!(
            stranger.run(args),
            (Some(1), String::new(), refusal.clone())
        );
    }
}

#[test]
fn it_spawns_a_subagent_as_its_session() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (parent, shell) = Shell::of(&s, Agent::Codex);

    let stdout = shell.ok(&["subagent", "spawn", "claude", "write the tests"]);

    let (asked_by, PanelSpec::Agent { session, worktree }) = s.next_panel_request() else {
        panic!("not an agent panel");
    };
    assert_eq!(asked_by, Some(parent));
    assert_eq!(worktree, s.worktree);
    assert_eq!(
        stdout,
        format!(
            "Started subagent session {session} in {}.\n",
            s.worktree.display()
        )
    );
}

/// The shell step's run, once the fake `name` logged it.
fn shell_run(s: &Setup, name: &str) -> Value {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(line) = s.log(name).into_iter().find(|l| l.get("shell").is_some()) {
            return line;
        }
        assert!(Instant::now() < deadline, "no shell step in the {name} log");
        thread::sleep(Duration::from_millis(10));
    }
}

/// The agent runs grovecode from its own shell, with only what grove gave its process.
fn assert_acts_as_its_session_from_the_agents_shell(agent: Agent) {
    let command = format!("'{}' panel open log tests 'echo ok'", grovecode().display());
    let mut s = setup(json!([{"shell": command}, {"wait_input": "\r"}]));

    let parent = s.spawn_parent(agent);

    let (asked_by, PanelSpec::Log { session, .. }) = s.next_panel_request() else {
        panic!("not a log panel");
    };
    assert_eq!(asked_by, Some(parent));
    let run = shell_run(&s, fake_name(agent));
    assert_eq!(run["code"], 0, "{run}");
    assert_eq!(run["stdout"], format!("Opened panel {session}.\n"), "{run}");
}

#[test]
fn claude_code_runs_it_from_its_shell_as_its_session() {
    assert_acts_as_its_session_from_the_agents_shell(Agent::Claude);
}

#[test]
fn codex_runs_it_from_its_shell_as_its_session() {
    assert_acts_as_its_session_from_the_agents_shell(Agent::Codex);
}

/// `to` as a path relative to `from`, through `..` where it must go up.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path: PathBuf = from[shared..].iter().map(|_| "..").collect();
    path.extend(&to[shared..]);
    path
}

/// Spawns a subagent with `--worktree path` from a shell in the folder `dir`, and checks it
/// starts in `other`.
fn assert_spawns_in(s: &mut Setup, shell: &Shell, dir: &Path, path: &Path, other: &Path) {
    let path = path.to_str().unwrap();
    let (code, stdout, stderr) = shell.run_in(
        dir,
        &["subagent", "spawn", "codex", "hi", "--worktree", path],
    );

    assert_eq!(code, Some(0), "{path}: {stderr}");
    let (_, PanelSpec::Agent { session, worktree }) = s.next_panel_request() else {
        panic!("not an agent panel");
    };
    assert_eq!(worktree, other, "{path}");
    assert_eq!(
        stdout,
        format!(
            "Started subagent session {session} in {}.\n",
            other.display()
        )
    );
}

#[test]
fn worktree_starts_the_subagent_in_another_tracked_worktree() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let other = s.other_worktree();
    let (_, shell) = Shell::of(&s, Agent::Claude);

    assert_spawns_in(&mut s, &shell, Path::new("/"), &other, &other);
}

#[test]
fn a_relative_worktree_is_taken_from_the_shells_folder() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let other = s.other_worktree();
    let (_, shell) = Shell::of(&s, Agent::Claude);
    let from = s.worktree.clone();
    let path = relative(&from, &other);
    assert!(path.starts_with(".."), "{}", path.display());

    assert_spawns_in(&mut s, &shell, &from, &path, &other);
}

#[test]
fn workspace_and_profile_commands_act_on_the_shells_session_and_its_repo() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let plain = ProfileSettings {
        agent: grove_core::AgentDefaults {
            model: Some("opus".into()),
            ..Default::default()
        },
        ..ProfileSettings::default()
    };
    s.use_profile(&s.worktree, "Plain", plain);
    let (session, shell) = Shell::of(&s, Agent::Claude);
    let mut workspace: Value = serde_json::from_str(&shell.ok(&["workspace", "get"])).unwrap();
    workspace["panels"]
        .as_array_mut()
        .unwrap()
        .push(json!("logs"));
    s.drain_events();

    let set = shell.ok(&["workspace", "set", &workspace.to_string()]);

    assert_eq!(set, "Changed the workspace.\n");
    let CoreEvent::WorkspaceChanged { session: owner, by } = s.next_change() else {
        panic!("not a workspace change");
    };
    assert_eq!((owner, by), (session, Some(session)));
    let got: Value = serde_json::from_str(&shell.ok(&["workspace", "get"])).unwrap();
    assert_eq!(got, workspace);

    let set = shell.ok(&["profile", "set", r#"{"agent": {"model": "sonnet"}}"#]);

    assert_eq!(set, "Changed profile Plain.\n");
    let CoreEvent::ProfilesChanged { by } = s.next_change() else {
        panic!("not a profile change");
    };
    assert_eq!(by, Some(session));
    let got: Value = serde_json::from_str(&shell.ok(&["profile", "get"])).unwrap();
    assert_eq!(got["name"], "Plain");
    assert_eq!(got["settings"]["agent"]["model"], "sonnet");

    workspace["panels"] = json!(["widget"]);
    for args in [
        ["workspace", "set", &workspace.to_string()],
        ["profile", "set", r#"{"agent": {"modle": "sonnet"}}"#],
    ] {
        let (code, stdout, stderr) = shell.run(&args);

        assert_eq!(code, Some(1), "{args:?}: {stderr}");
        assert_eq!(stdout, "");
        assert!(stderr.starts_with("grovecode: grove refused: "), "{stderr}");
    }
    assert!(!s.changed_by_now());
}

/// grovecode in the shell of a Claude session in chat mode.
fn chat_shell(s: &Setup) -> Shell {
    let session = s.spawn_parent_in(Agent::Claude, grove_core::Mode::Chat);
    Shell {
        socket: s.cfg.socket.clone(),
        session,
        token: s.token("claude-sidecar", session),
    }
}

/// The pages in the session's conversation, once there are `n`: title, HTML and height.
fn pages(s: &Setup, session: SessionId, n: usize) -> Vec<(String, String, Option<u32>)> {
    use grove_core::AgentMessage;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (sink, _frames) = tokio::sync::mpsc::unbounded_channel();
        let pages: Vec<_> = s
            .core
            .subscribe(session, sink)
            .unwrap()
            .into_iter()
            .filter_map(|message| match message {
                AgentMessage::InlinePage {
                    title,
                    html,
                    height,
                    ..
                } => Some((title, html, height)),
                _ => None,
            })
            .collect();
        if pages.len() >= n {
            return pages;
        }
        assert!(Instant::now() < deadline, "{} of {n} pages", pages.len());
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn it_shows_a_page_from_html_and_from_a_path_taken_from_the_shells_folder() {
    let s = setup(json!([]));
    let shell = chat_shell(&s);
    fs::create_dir(s.worktree.join("docs")).unwrap();
    fs::write(s.worktree.join("docs/chart.html"), "<p>from a file</p>").unwrap();

    let inline = shell.run(&["html", "show", "Inline", "--html", "<p>inline</p>"]);
    let file = shell.run_in(
        &s.worktree.join("docs"),
        &[
            "html",
            "show",
            "File",
            "--path",
            "chart.html",
            "--height=400",
        ],
    );

    for (code, stdout, stderr) in [&inline, &file] {
        assert_eq!(*code, Some(0), "{stderr}");
        assert!(
            stdout.starts_with("Showed the page in the conversation as page-"),
            "{stdout}"
        );
    }
    assert_eq!(
        pages(&s, shell.session, 2),
        [
            ("Inline".to_owned(), "<p>inline</p>".to_owned(), None),
            (
                "File".to_owned(),
                "<p>from a file</p>".to_owned(),
                Some(400)
            ),
        ]
    );
}

#[test]
fn a_page_grove_refuses_fails_with_the_reason() {
    let s = setup(json!([]));
    let shell = chat_shell(&s);

    let (code, stdout, stderr) = shell.run(&["html", "show", "Chart"]);

    assert_eq!(code, Some(1));
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "grovecode: grove refused: html_show takes html or a path, one of the two\n"
    );
}
