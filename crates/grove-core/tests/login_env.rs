use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{Core, CoreConfig, CoreEvent, SpawnSpec, Warning};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

mod common;
use common::write_script;

const TIMEOUT: Duration = Duration::from_secs(10);
const MINIMAL_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
// Tests keep the home folder, and the socket in it, under /tmp.
fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

// A stand-in login shell. It gets `-ilc <command>`, runs `body`, then the command.
fn fake_shell(dir: &Path, body: &str) -> PathBuf {
    let shell = dir.join("shell");
    write_script(&shell, &format!("{body}\nexec /bin/sh -c \"$2\""));
    shell
}

fn start(dir: &Path, cfg: CoreConfig) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf) {
    let repo = init_repo(dir);
    let (core, events) = Core::start(cfg).unwrap();
    let worktree = add_repo(&core, &repo);
    (core, events, worktree)
}

fn init_repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    repo
}

// Tracks `repo` and gives its worktree.
fn add_repo(core: &Core, repo: &Path) -> PathBuf {
    block_on(core.add_repo(repo)).unwrap();
    block_on(core.list_worktrees())[0].path.clone()
}

fn config(dir: &Path, shell: Option<PathBuf>) -> CoreConfig {
    let mut cfg = CoreConfig::new(dir.join("home"));
    cfg.login_shell = shell;
    cfg
}

// The environment a shell session sees, from `env` run inside it.
fn session_env(core: &Core, worktree: &Path) -> HashMap<String, String> {
    let session = core
        .spawn(
            worktree,
            SpawnSpec {
                command: "/bin/sh".into(),
                // Output before attach is dropped, so it waits for a line first.
                args: vec![
                    "-c".into(),
                    "read _; echo BEGIN; /usr/bin/env; echo END".into(),
                ],
                cols: 200,
                rows: 24,
            },
        )
        .unwrap();
    let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
    core.attach(session, sink).unwrap();
    core.write(session, b"\r").unwrap();

    let mut output = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let text = String::from_utf8_lossy(&output);
        if let Some((_, rest)) = text.split_once("BEGIN\r\n")
            && let Some((body, _)) = rest.split_once("END\r\n")
        {
            return body
                .split("\r\n")
                .filter_map(|line| line.split_once('='))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
        }
        // Not the output itself: it can hold the environment, tokens and all.
        assert!(Instant::now() < deadline, "no env in the session's output");
        match rx.try_recv() {
            Ok(frame) => output.extend(frame.bytes),
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

// The pid of the job a profile started, from the file it wrote it to.
fn job_pid(file: &Path) -> libc::pid_t {
    let written = || {
        fs::read_to_string(file)
            .ok()
            .filter(|pid| pid.ends_with('\n'))
    };
    assert!(
        eventually(|| written().is_some()),
        "the profile started no job"
    );
    written().unwrap().trim().parse().unwrap()
}

// The job's parent is killed with it, so init reaps it and its pid goes away.
fn exited(pid: libc::pid_t) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return false;
    }
    let error = std::io::Error::last_os_error();
    assert_eq!(error.raw_os_error(), Some(libc::ESRCH), "{error}");
    true
}

fn eventually(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

fn no_warning(events: &mut broadcast::Receiver<CoreEvent>) {
    loop {
        match events.try_recv() {
            Ok(CoreEvent::Warning(warning)) => panic!("{warning:?}"),
            Ok(_) => {}
            Err(TryRecvError::Empty) => return,
            Err(e) => panic!("{e}"),
        }
    }
}

fn login_shell_warning(events: &mut broadcast::Receiver<CoreEvent>) -> String {
    match events.try_recv() {
        Ok(CoreEvent::Warning(Warning::LoginShellEnv { reason })) => reason,
        other => panic!("no login shell warning: {other:?}"),
    }
}

// Runs this binary's test `name` again with `home` as HOME and a pty as its controlling
// terminal, as grove has when started from a terminal. It passes if that run passes.
fn under_a_terminal(name: &str, home: &Path) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", name]);
    cmd.env("GROVE_TEST_UNDER_A_TERMINAL", "1");
    cmd.env("HOME", home);
    // portable-pty makes the pty the child's controlling terminal.
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let output = thread::spawn(move || {
        let mut out = Vec::new();
        // Ends with an error once the run closes the pty. What was read stays in `out`.
        let _ = reader.read_to_end(&mut out);
        out
    });
    let status = child.wait().unwrap();
    drop(pair.master);
    let output = output.join().unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&output));
}

fn under_a_terminal_now() -> bool {
    std::env::var_os("GROVE_TEST_UNDER_A_TERMINAL").is_some()
}

// The base environment comes from grove's own, so this test runs itself again with a test
// HOME holding a profile, and with a variable a parent Claude Code session would set.
#[test]
fn a_session_gets_what_the_login_profile_exports_and_nothing_else_from_groves_environment() {
    if std::env::var_os("GROVE_TEST_PROFILE").is_none() {
        let dir = short_temp_dir();
        fs::write(
            dir.path().join(".profile"),
            "export FROM_PROFILE=yes\necho NOISE=before\n",
        )
        .unwrap();
        let run = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "a_session_gets_what_the_login_profile_exports_and_nothing_else_from_groves_environment",
            ])
            .env("GROVE_TEST_PROFILE", "1")
            .env("HOME", dir.path())
            .env("CLAUDECODE", "1")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stdout)
        );
        return;
    }
    let dir = short_temp_dir();
    // dash as a login shell reads /etc/profile and $HOME/.profile.
    let (core, mut events, worktree) =
        start(dir.path(), config(dir.path(), Some("/bin/dash".into())));

    let env = session_env(&core, &worktree);

    assert_eq!(env.get("FROM_PROFILE").map(String::as_str), Some("yes"));
    assert_eq!(env.get("HOME"), std::env::var("HOME").ok().as_ref());
    assert_eq!(env.get("SHELL").map(String::as_str), Some("/bin/dash"));
    for leaked in ["CLAUDECODE", "GROVE_TEST_PROFILE", "NOISE"] {
        assert!(!env.contains_key(leaked), "{leaked} leaked");
    }
    no_warning(&mut events);
}

#[test]
fn a_session_gets_the_terminal_variables_and_a_default_lang() {
    let dir = short_temp_dir();
    let shell = fake_shell(
        dir.path(),
        "export TERM=dumb COLORTERM= TERM_PROGRAM=Apple_Terminal",
    );
    let (core, _events, worktree) = start(dir.path(), config(dir.path(), Some(shell)));

    let env = session_env(&core, &worktree);

    assert_eq!(env["TERM"], "xterm-256color");
    assert_eq!(env["COLORTERM"], "truecolor");
    assert_eq!(env["TERM_PROGRAM"], "grove");
    assert_eq!(env["LANG"], "en_US.UTF-8");
}

#[test]
fn a_lang_from_the_login_profile_is_kept() {
    let dir = short_temp_dir();
    let shell = fake_shell(dir.path(), "export LANG=fr_FR.UTF-8");
    let (core, _events, worktree) = start(dir.path(), config(dir.path(), Some(shell)));

    let env = session_env(&core, &worktree);

    assert_eq!(env["LANG"], "fr_FR.UTF-8");
}

#[test]
fn profile_output_around_the_environment_is_ignored() {
    let dir = short_temp_dir();
    // The fake prints after its command too, so the command runs in a child shell.
    let shell = dir.path().join("shell");
    write_script(&shell, "echo BEFORE=1\n/bin/sh -c \"$2\"\necho AFTER=1");
    let (core, mut events, worktree) = start(dir.path(), config(dir.path(), Some(shell)));

    let env = session_env(&core, &worktree);

    assert!(!env.contains_key("BEFORE"));
    assert!(!env.contains_key("AFTER"));
    no_warning(&mut events);
}

#[test]
fn a_profile_that_prints_a_lot_still_gives_its_environment() {
    let dir = short_temp_dir();
    // 64 MiB of noise, with no line breaks, before the environment.
    let shell = fake_shell(
        dir.path(),
        "export FROM_PROFILE=yes\nhead -c 67108864 /dev/zero | tr '\\0' x",
    );
    let mut cfg = config(dir.path(), Some(shell));
    // Printing that much took 10 s on a loaded machine. The timeout is not under test here.
    cfg.login_shell_timeout = Duration::from_secs(60);
    let (core, mut events, worktree) = start(dir.path(), cfg);

    no_warning(&mut events);
    let env = session_env(&core, &worktree);
    assert_eq!(env.get("FROM_PROFILE").map(String::as_str), Some("yes"));
}

#[test]
fn a_login_shell_that_hangs_past_the_timeout_leaves_the_minimal_environment_and_a_warning() {
    let dir = short_temp_dir();
    let pid_file = dir.path().join("job.pid");
    let shell = fake_shell(
        dir.path(),
        &format!(
            "export FROM_PROFILE=yes\nsleep 1000 &\necho $! > '{}'\nwait",
            pid_file.display()
        ),
    );
    let mut cfg = config(dir.path(), Some(shell));
    let default_timeout = cfg.login_shell_timeout;
    // Long enough for the profile to start its job first.
    cfg.login_shell_timeout = Duration::from_secs(2);
    let repo = init_repo(dir.path());

    // Only the start is timed, not the git commands around it.
    let started = Instant::now();
    let (core, mut events) = Core::start(cfg).unwrap();
    let took = started.elapsed();

    // The start gave up at the 2 s given, not at the default, and did not wait for the shell.
    assert!(took < default_timeout, "{took:?}");
    let reason = login_shell_warning(&mut events);
    assert!(
        reason.contains("did not print its environment within 2s"),
        "{reason}"
    );
    let worktree = add_repo(&core, &repo);
    let env = session_env(&core, &worktree);
    assert_eq!(env["PATH"], MINIMAL_PATH);
    assert!(!env.contains_key("FROM_PROFILE"));
    assert_eq!(env["TERM"], "xterm-256color");
    // What the profile started is gone, with the shell.
    let job = job_pid(&pid_file);
    assert!(eventually(|| exited(job)));
}

#[test]
fn a_login_shell_that_exits_without_printing_leaves_the_minimal_environment_and_a_warning() {
    let dir = short_temp_dir();
    let shell = fake_shell(dir.path(), "export FROM_PROFILE=yes\nexit 1");
    let (core, mut events, worktree) = start(dir.path(), config(dir.path(), Some(shell)));

    let reason = login_shell_warning(&mut events);
    assert!(reason.contains("printed no environment"), "{reason}");
    let env = session_env(&core, &worktree);
    assert_eq!(env["PATH"], MINIMAL_PATH);
    assert!(!env.contains_key("FROM_PROFILE"));
}

#[test]
fn an_empty_environment_between_the_markers_leaves_the_minimal_environment_and_a_warning() {
    let dir = short_temp_dir();
    // As if `env -0` failed: the markers with nothing between them.
    let shell = fake_shell(
        dir.path(),
        "printf '\\n%s\\n\\n%s\\n' __grove_env_start__ __grove_env_end__\nexit 0",
    );
    let (core, mut events, worktree) = start(dir.path(), config(dir.path(), Some(shell)));

    let reason = login_shell_warning(&mut events);
    assert!(reason.contains("printed no environment"), "{reason}");
    let env = session_env(&core, &worktree);
    assert_eq!(env["PATH"], MINIMAL_PATH);
    assert!(env.contains_key("HOME"));
}

#[test]
fn a_login_shell_that_does_not_exist_leaves_the_minimal_environment_and_a_warning() {
    let dir = short_temp_dir();
    let shell = dir.path().join("no-such-shell");
    let (core, mut events, worktree) = start(dir.path(), config(dir.path(), Some(shell)));

    let reason = login_shell_warning(&mut events);
    assert!(core.warnings().iter().any(|entry| matches!(
        &entry.warning, Warning::LoginShellEnv { reason: active } if active == &reason
    )));
    assert!(reason.contains("could not start"), "{reason}");
    let env = session_env(&core, &worktree);
    assert_eq!(env["PATH"], MINIMAL_PATH);
}

// $SHELL may be a bare name. It is looked up in the PATH the login shell starts with.
#[test]
fn a_login_shell_given_by_name_is_found_on_the_minimal_path() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = start(dir.path(), config(dir.path(), Some("dash".into())));

    let env = session_env(&core, &worktree);

    assert_eq!(env.get("SHELL").map(String::as_str), Some("dash"));
    no_warning(&mut events);
}

// Run from a folder that holds a `shell`, so one taken from the current folder would start.
#[test]
fn a_login_shell_given_by_name_and_not_on_the_minimal_path_is_not_run_from_the_current_folder() {
    if std::env::var_os("GROVE_TEST_IN_FOLDER").is_none() {
        let dir = short_temp_dir();
        fake_shell(dir.path(), "");
        let run = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "a_login_shell_given_by_name_and_not_on_the_minimal_path_is_not_run_from_the_current_folder",
            ])
            .env("GROVE_TEST_IN_FOLDER", "1")
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stdout)
        );
        return;
    }
    let dir = short_temp_dir();
    let (_core, mut events, _worktree) =
        start(dir.path(), config(dir.path(), Some("shell".into())));

    let reason = login_shell_warning(&mut events);
    assert!(reason.contains("could not start shell"), "{reason}");
}

#[test]
fn without_a_login_shell_sessions_get_the_minimal_environment_and_no_warning() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = start(dir.path(), config(dir.path(), None));

    let env = session_env(&core, &worktree);

    assert_eq!(env["PATH"], MINIMAL_PATH);
    no_warning(&mut events);
}

// An interactive zsh in a background process group of its terminal stops itself, so the read
// must not leave it grove's terminal.
#[test]
fn started_from_a_terminal_an_interactive_zsh_profile_still_gives_its_path() {
    let name = "started_from_a_terminal_an_interactive_zsh_profile_still_gives_its_path";
    if !under_a_terminal_now() {
        let home = short_temp_dir();
        fs::write(
            home.path().join(".zshrc"),
            "export PATH=\"$HOME/from-zshrc:$PATH\"\n",
        )
        .unwrap();
        under_a_terminal(name, home.path());
        return;
    }
    let dir = short_temp_dir();
    let started = Instant::now();
    // The default timeout, 5 s.
    let (core, mut events, worktree) =
        start(dir.path(), config(dir.path(), Some("/bin/zsh".into())));

    no_warning(&mut events);
    assert!(started.elapsed() < Duration::from_secs(3));
    let env = session_env(&core, &worktree);
    let home = std::env::var("HOME").unwrap();
    assert!(
        env["PATH"].starts_with(&format!("{home}/from-zshrc:")),
        "{}",
        env["PATH"]
    );
}

#[test]
fn started_from_a_terminal_a_zsh_profile_that_hangs_is_killed_with_what_it_started() {
    let name = "started_from_a_terminal_a_zsh_profile_that_hangs_is_killed_with_what_it_started";
    if !under_a_terminal_now() {
        let home = short_temp_dir();
        fs::write(
            home.path().join(".zshrc"),
            "sleep 1000 &\necho $! > \"$HOME/job.pid\"\nwait\n",
        )
        .unwrap();
        under_a_terminal(name, home.path());
        return;
    }
    let dir = short_temp_dir();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let mut cfg = config(dir.path(), Some("/bin/zsh".into()));
    // Long enough for zsh to start and the profile to start its job first.
    cfg.login_shell_timeout = Duration::from_secs(2);
    let (_core, mut events, _worktree) = start(dir.path(), cfg);

    let reason = login_shell_warning(&mut events);
    assert!(reason.contains("did not print its environment"), "{reason}");
    let job = job_pid(&home.join("job.pid"));
    assert!(eventually(|| exited(job)));
}
