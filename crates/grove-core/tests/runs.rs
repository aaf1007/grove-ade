//! Run sessions: named commands a repo saves, each started in one of its worktrees.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, Core, CoreConfig, CoreError, CoreEvent, LaunchArgs, NewWorktree, ProfileSettings,
    RemoveOpts, RepoSettings, RunSession, RunSpec, SessionId, SessionInfo, SessionKind,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::sync::mpsc::unbounded_channel;

const TIMEOUT: Duration = Duration::from_secs(10);

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
// Tests keep the home folder, and the socket in it, under /tmp.
fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

// The core owns its runtime, and dropping it inside an async context panics.
// So tests make and drop the core outside, and only await its calls here.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

// Runs git with the user's config turned off, so their hooks or signing settings never apply.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Grove Test")
        .env("GIT_AUTHOR_EMAIL", "test@grove.invalid")
        .env("GIT_COMMITTER_NAME", "Grove Test")
        .env("GIT_COMMITTER_EMAIL", "test@grove.invalid")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

/// A core tracking a repo on `main` with one commit, and a linked worktree of it. Returns the
/// main worktree's path and the linked one's.
fn core_with_worktrees(dir: &Path) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf, PathBuf) {
    let repo = dir.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let (core, events) = Core::start(CoreConfig::new(dir.join("home"))).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let main = block_on(core.list_worktrees())[0].path.clone();
    let new = NewWorktree {
        branch: "feature".into(),
        ..NewWorktree::default()
    };
    let linked = block_on(core.create_worktree(&main, new))
        .unwrap()
        .worktree
        .path;
    (core, events, main, linked)
}

fn run_session(name: &str, command: &str) -> RunSession {
    RunSession {
        name: name.into(),
        command: command.into(),
        port: None,
    }
}

// Settings with no profile whose own run sessions are `runs`.
fn with_run_sessions(runs: Vec<RunSession>) -> RepoSettings {
    RepoSettings {
        overrides: ProfileSettings {
            run_sessions: Some(runs),
            ..ProfileSettings::default()
        },
        ..RepoSettings::default()
    }
}

fn run_spec(name: &str) -> RunSpec {
    RunSpec {
        name: name.into(),
        cols: 80,
        rows: 24,
    }
}

/// Waits for the session's `SessionExited` and returns its exit code.
fn wait_exit(events: &mut broadcast::Receiver<CoreEvent>, session: SessionId) -> Option<i32> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(CoreEvent::SessionExited { session: s, code }) if s == session => return code,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "session {session} never exited");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_saved_run_session_runs_its_command_in_the_chosen_worktree() {
    let dir = short_temp_dir();
    let (core, mut events, main, linked) = core_with_worktrees(dir.path());
    let out = dir.path().join("ran");
    let settings = with_run_sessions(vec![run_session(
        "where",
        &format!("pwd -P > '{}'", out.display()),
    )]);
    block_on(core.set_repo_settings(&main, settings)).unwrap();

    let session = core.spawn_run(&linked, run_spec("where")).unwrap();

    assert_eq!(wait_exit(&mut events, session), Some(0));
    assert_eq!(
        fs::read_to_string(&out).unwrap().trim_end(),
        linked.to_str().unwrap()
    );
}

#[test]
fn a_run_session_records_its_declared_port_while_it_runs() {
    let dir = short_temp_dir();
    let (core, mut events, main, linked) = core_with_worktrees(dir.path());
    let dev = RunSession {
        port: Some(5173),
        ..run_session("dev", "sleep 1000")
    };
    let settings = with_run_sessions(vec![dev, run_session("tests", "sleep 1000")]);
    block_on(core.set_repo_settings(&main, settings)).unwrap();

    let dev = core.spawn_run(&linked, run_spec("dev")).unwrap();
    let tests = core.spawn_run(&linked, run_spec("tests")).unwrap();

    assert_eq!(core.port(dev), Some(5173));
    assert_eq!(core.port(tests), None);
    core.kill(dev).unwrap();
    wait_exit(&mut events, dev);
    assert_eq!(core.port(dev), None);
}

// A session would inherit grove's own environment, so this test runs itself again with
// GROVE_SESSION_ID set, as when grove starts from inside an agent session.
#[test]
fn a_run_session_gets_no_grove_session_id() {
    if std::env::var_os("GROVE_SESSION_ID").is_none() {
        let run = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "a_run_session_gets_no_grove_session_id"])
            .env("GROVE_SESSION_ID", "41")
            .env("GROVE_SOCKET", "/tmp/elsewhere.sock")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert!(run.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
        return;
    }
    let dir = short_temp_dir();
    let (core, mut events, main, linked) = core_with_worktrees(dir.path());
    let out = dir.path().join("env");
    let command = format!(
        "echo \"${{GROVE_SESSION_ID-none}} ${{GROVE_SOCKET-none}}\" > '{}'",
        out.display()
    );
    let settings = with_run_sessions(vec![run_session("env", &command)]);
    block_on(core.set_repo_settings(&main, settings)).unwrap();

    let session = core.spawn_run(&linked, run_spec("env")).unwrap();

    assert_eq!(wait_exit(&mut events, session), Some(0));
    assert_eq!(fs::read_to_string(&out).unwrap(), "none none\n");
}

#[test]
fn a_forgotten_repo_comes_back_without_its_old_settings() {
    let dir = short_temp_dir();
    let (core, _events, main, _linked) = core_with_worktrees(dir.path());
    let settings = with_run_sessions(vec![run_session("dev", "pnpm dev")]);
    block_on(core.set_repo_settings(&main, settings)).unwrap();

    block_on(core.remove_worktree(&main, RemoveOpts::ForgetRepo)).unwrap();

    assert!(matches!(
        core.repo_settings(&main),
        Err(CoreError::UnknownRepo(_))
    ));
    block_on(core.add_repo(&main)).unwrap();
    assert_eq!(core.repo_settings(&main).unwrap(), RepoSettings::default());
}

#[test]
fn settings_with_a_bad_run_session_are_refused_and_the_old_ones_kept() {
    let dir = short_temp_dir();
    let (core, _events, main, _linked) = core_with_worktrees(dir.path());
    let good = with_run_sessions(vec![run_session("dev", "pnpm dev")]);
    block_on(core.set_repo_settings(&main, good.clone())).unwrap();

    for bad in [
        vec![
            run_session("dev", "pnpm dev"),
            run_session("dev", "pnpm start"),
        ],
        vec![run_session(" ", "pnpm dev")],
        vec![run_session("dev", "")],
    ] {
        let settings = with_run_sessions(bad);
        let err = block_on(core.set_repo_settings(&main, settings)).unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidRunSession { .. }),
            "{err:?}"
        );
    }
    assert_eq!(core.repo_settings(&main).unwrap(), good);
}

#[test]
fn a_repos_run_sessions_are_its_profiles_unless_its_overrides_set_their_own() {
    let dir = short_temp_dir();
    let (core, _events, main, linked) = core_with_worktrees(dir.path());
    assert_eq!(core.run_sessions(&main).unwrap(), []);

    let rust_crate = RepoSettings {
        profile: Some("Rust crate".into()),
        ..RepoSettings::default()
    };
    block_on(core.set_repo_settings(&main, rust_crate)).unwrap();
    assert_eq!(
        core.run_sessions(&main).unwrap(),
        [run_session("tests", "cargo test")]
    );

    let own = vec![run_session("dev", "pnpm dev")];
    let overridden = RepoSettings {
        profile: Some("Rust crate".into()),
        ..with_run_sessions(own.clone())
    };
    block_on(core.set_repo_settings(&main, overridden)).unwrap();
    assert_eq!(core.run_sessions(&main).unwrap(), own);

    for untracked in [linked, dir.path().join("elsewhere")] {
        let err = core.run_sessions(&untracked).unwrap_err();
        assert!(matches!(err, CoreError::UnknownRepo(_)), "{err:?}");
    }
}

#[test]
fn a_run_session_the_repo_never_saved_does_not_start() {
    let dir = short_temp_dir();
    let (core, _events, main, linked) = core_with_worktrees(dir.path());

    let err = core.spawn_run(&linked, run_spec("dev")).unwrap_err();

    let CoreError::UnknownRunSession { repo, name } = err else {
        panic!("{err:?}");
    };
    assert_eq!((repo, name.as_str()), (main, "dev"));
}

fn launch_args(agent: Agent, args: &[&str]) -> RepoSettings {
    let args: Vec<String> = args.iter().map(|&a| a.into()).collect();
    let launch_args = match agent {
        Agent::Claude => LaunchArgs {
            claude: Some(args),
            ..LaunchArgs::default()
        },
        Agent::Codex => LaunchArgs {
            codex: Some(args),
            ..LaunchArgs::default()
        },
    };
    RepoSettings {
        overrides: ProfileSettings {
            launch_args,
            ..ProfileSettings::default()
        },
        ..RepoSettings::default()
    }
}

// A later -c wins in Codex, and a later --settings in Claude, so these would replace grove's
// hooks or session id and break its status.
#[test]
fn launch_args_that_would_replace_what_grove_passes_are_refused() {
    let dir = short_temp_dir();
    let (core, _events, main, _linked) = core_with_worktrees(dir.path());
    let allowed = [
        (
            Agent::Codex,
            vec!["-c", "hooks.Notification=[]", "--search"],
        ),
        (
            Agent::Codex,
            vec!["-c", r#"shell_environment_policy.inherit="core""#],
        ),
        (
            Agent::Codex,
            vec!["-c", r#"shell_environment_policy.set.FOO="1""#],
        ),
        (
            Agent::Codex,
            vec![
                "-c",
                r#"shell_environment_policy.include_only=["PATH", "grove_session_id"]"#,
            ],
        ),
        (
            Agent::Codex,
            vec!["-c", r#"mcp_servers.docs.command="docs-mcp""#],
        ),
        (Agent::Claude, vec!["--add-dir", "../shared", "--verbose"]),
    ];
    for (agent, args) in allowed {
        block_on(core.set_repo_settings(&main, launch_args(agent, &args))).unwrap();
    }
    let kept = core.repo_settings(&main).unwrap();

    let refused = [
        (
            Agent::Codex,
            vec!["-c", "hooks.SessionStart=[]"],
            "hooks.SessionStart=[]",
        ),
        (
            Agent::Codex,
            vec!["-c", r#"mcp_servers.grove.command="other""#],
            r#"mcp_servers.grove.command="other""#,
        ),
        (Agent::Codex, vec!["-c", "mcp_servers={}"], "mcp_servers={}"),
        (
            Agent::Codex,
            vec!["--search", "--config=hooks={}"],
            "--config=hooks={}",
        ),
        (
            Agent::Codex,
            vec!["--config", "hooks . Stop=[]"],
            "hooks . Stop=[]",
        ),
        (
            Agent::Codex,
            vec![r#"-cshell_environment_policy={inherit="core"}"#],
            r#"-cshell_environment_policy={inherit="core"}"#,
        ),
        (Agent::Codex, vec!["-c=hooks.Stop=[]"], "-c=hooks.Stop=[]"),
        (
            Agent::Codex,
            vec![r#"-c=shell_environment_policy.set.GROVE_SESSION_ID="9""#],
            r#"-c=shell_environment_policy.set.GROVE_SESSION_ID="9""#,
        ),
        // Codex keeps only these after it sets grove's GROVE_SESSION_ID.
        (
            Agent::Codex,
            vec![
                "-c",
                r#"shell_environment_policy.include_only=["PATH","HOME"]"#,
            ],
            r#"shell_environment_policy.include_only=["PATH","HOME"]"#,
        ),
        // Codex reads the value as TOML, so the comment is not in the list.
        (
            Agent::Codex,
            vec![
                "-c",
                r#"shell_environment_policy.include_only=["PATH"] # "GROVE_SESSION_ID""#,
            ],
            r#"shell_environment_policy.include_only=["PATH"] # "GROVE_SESSION_ID""#,
        ),
        (Agent::Claude, vec!["--settings", "mine.json"], "--settings"),
        (
            Agent::Claude,
            vec!["--verbose", "--session-id=x"],
            "--session-id=x",
        ),
        (Agent::Claude, vec!["--resume"], "--resume"),
        (Agent::Claude, vec!["-r"], "-r"),
    ];
    for (agent, args, expected) in refused {
        let err = block_on(core.set_repo_settings(&main, launch_args(agent, &args))).unwrap_err();
        let CoreError::InvalidLaunchArgs { agent: a, arg } = &err else {
            panic!("{err:?}");
        };
        assert_eq!((*a, arg.as_str()), (agent, expected), "{args:?}");
    }
    assert_eq!(core.repo_settings(&main).unwrap(), kept);
}

// The worktree's run sessions as `sessions` lists them.
fn run_infos(core: &Core, worktree: &Path) -> Vec<SessionInfo> {
    let sessions = core.sessions(worktree).unwrap();
    sessions
        .into_iter()
        .filter(|info| matches!(info.kind, SessionKind::Run { .. }))
        .collect()
}

fn stopped(session: SessionId, name: &str) -> SessionInfo {
    SessionInfo {
        session,
        kind: SessionKind::Run { name: name.into() },
        running: false,
    }
}

// Waits until `sessions` lists the run sessions of `worktree` as `expected`.
fn wait_runs_listed(core: &Core, worktree: &Path, expected: &[SessionInfo]) {
    let deadline = Instant::now() + TIMEOUT;
    while run_infos(core, worktree) != expected {
        assert!(
            Instant::now() < deadline,
            "listed {:?}, not {expected:?}",
            run_infos(core, worktree)
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn attachable(core: &Core, session: SessionId) -> bool {
    let (sink, _frames) = unbounded_channel();
    match core.attach(session, sink) {
        Ok(_) => true,
        Err(CoreError::SessionNotRunning(s)) if s == session => false,
        Err(e) => panic!("{e:?}"),
    }
}

#[test]
fn a_run_session_that_exited_keeps_its_final_screen_and_is_listed_stopped() {
    let dir = short_temp_dir();
    let (core, mut events, main, linked) = core_with_worktrees(dir.path());
    let settings = with_run_sessions(vec![run_session("tests", "echo all 12 passed")]);
    block_on(core.set_repo_settings(&main, settings)).unwrap();

    let session = core.spawn_run(&linked, run_spec("tests")).unwrap();
    assert_eq!(wait_exit(&mut events, session), Some(0));

    // Its reader may still be draining the last output when the exit is reported.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (sink, mut frames) = unbounded_channel();
        let screen =
            String::from_utf8_lossy(&core.attach(session, sink).unwrap().bytes).into_owned();
        assert!(frames.try_recv().is_err());
        if screen.contains("all 12 passed") {
            break;
        }
        assert!(Instant::now() < deadline, "{screen:?}");
        thread::sleep(Duration::from_millis(10));
    }
    wait_runs_listed(&core, &linked, &[stopped(session, "tests")]);
}

#[test]
fn starting_a_run_session_again_releases_the_one_that_exited() {
    let dir = short_temp_dir();
    let (core, mut events, main, linked) = core_with_worktrees(dir.path());
    let settings = with_run_sessions(vec![run_session("tests", "true")]);
    block_on(core.set_repo_settings(&main, settings)).unwrap();
    let first = core.spawn_run(&linked, run_spec("tests")).unwrap();
    wait_exit(&mut events, first);
    wait_runs_listed(&core, &linked, &[stopped(first, "tests")]);

    let second = core.spawn_run(&linked, run_spec("tests")).unwrap();

    assert!(!attachable(&core, first));
    wait_exit(&mut events, second);
    wait_runs_listed(&core, &linked, &[stopped(second, "tests")]);
    assert!(attachable(&core, second));
}

#[test]
fn forgetting_or_deleting_a_worktree_releases_its_run_sessions_that_exited() {
    let dir = short_temp_dir();
    let (core, mut events, main, linked) = core_with_worktrees(dir.path());
    let settings = with_run_sessions(vec![
        run_session("tests", "true"),
        run_session("dev", "sleep 1000"),
    ]);
    block_on(core.set_repo_settings(&main, settings)).unwrap();
    let new = NewWorktree {
        branch: "other".into(),
        ..NewWorktree::default()
    };
    let other = block_on(core.create_worktree(&main, new))
        .unwrap()
        .worktree
        .path;
    let mut ended = Vec::new();
    for worktree in [&linked, &other] {
        let exited = core.spawn_run(worktree, run_spec("tests")).unwrap();
        wait_exit(&mut events, exited);
        wait_runs_listed(&core, worktree, &[stopped(exited, "tests")]);
        // Killed by the removal, so it ends as the worktree goes.
        let killed = core.spawn_run(worktree, run_spec("dev")).unwrap();
        ended.extend([exited, killed]);
    }

    block_on(core.remove_worktree(&linked, RemoveOpts::Forget)).unwrap();
    let delete = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };
    block_on(core.remove_worktree(&other, delete)).unwrap();

    // The killed ones' exits are handled on the core's runtime, after the removal returns.
    let deadline = Instant::now() + TIMEOUT;
    while let Some(session) = ended.iter().find(|&&s| attachable(&core, s)) {
        assert!(Instant::now() < deadline, "{session} still attachable");
        thread::sleep(Duration::from_millis(10));
    }
}
