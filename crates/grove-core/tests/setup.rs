//! Setup and cleanup scripts: from the repo's profile, run around adding and deleting a worktree.

use std::fs;
use std::future::poll_fn;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::Command;
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, AgentSpec, BranchOutcome, Core, CoreConfig, CoreError, CoreEvent, Mode, NewWorktree,
    ProfileSettings, RemoveOpts, RepoSettings, SessionId, Setup, SetupState,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

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
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Grove Test")
        .env("GIT_AUTHOR_EMAIL", "test@grove.invalid")
        .env("GIT_COMMITTER_NAME", "Grove Test")
        .env("GIT_COMMITTER_EMAIL", "test@grove.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed in {}",
        dir.display()
    );
    String::from_utf8(out.stdout).unwrap()
}

// Stands in for Claude Code. It logs whether setup had finished when it started.
fn fake_claude(dir: &Path) -> PathBuf {
    let fake = dir.join("claude");
    let script = format!(
        "#!/bin/sh\nif [ -e setup-done ]; then echo after-setup; else echo before-setup; fi >> '{}'\nexec sleep 1000\n",
        agent_log(dir).display()
    );
    fs::write(&fake, script).unwrap();
    let status = Command::new("chmod").arg("+x").arg(&fake).status().unwrap();
    assert!(status.success());
    fake
}

fn agent_log(dir: &Path) -> PathBuf {
    dir.join("agent.log")
}

fn claude_in_terminal_mode() -> AgentSpec {
    AgentSpec {
        agent: Some(Agent::Claude),
        mode: Mode::Terminal,
        prompt: None,
        cols: 80,
        rows: 24,
    }
}

/// A core tracking a repo on `main` with one commit, whose repo settings set `setup` and
/// `cleanup`, and with Claude Code replaced by `fake_claude`. Returns the main worktree's path.
fn core_with_scripts(
    dir: &Path,
    setup: Option<&str>,
    cleanup: Option<&str>,
) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf) {
    let repo = dir.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let mut cfg = CoreConfig::new(dir.join("home"));
    cfg.claude_command = fake_claude(dir);
    let (core, events) = Core::start(cfg).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let main = block_on(core.list_worktrees())[0].path.clone();
    let settings = RepoSettings {
        overrides: ProfileSettings {
            setup: setup.map(Into::into),
            cleanup: cleanup.map(Into::into),
            ..ProfileSettings::default()
        },
        ..RepoSettings::default()
    };
    block_on(core.set_repo_settings(&main, settings)).unwrap();
    (core, events, main)
}

fn new_worktree(branch: &str) -> NewWorktree {
    NewWorktree {
        branch: branch.into(),
        ..NewWorktree::default()
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

/// Waits for the `Setup` event that ends the setup of `worktree`.
fn wait_setup_end(events: &mut broadcast::Receiver<CoreEvent>, worktree: &Path) -> Setup {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(CoreEvent::Setup { worktree: w, setup })
                if w == worktree && setup.state != SetupState::Running =>
            {
                return setup;
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "setup never ended");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn adding_a_worktree_runs_its_setup_script_in_it_as_a_run_session() {
    let dir = short_temp_dir();
    let out = dir.path().join("ran");
    let script = format!("pwd -P > '{}'", out.display());
    let (core, mut events, main) = core_with_scripts(dir.path(), Some(&script), None);

    let added = block_on(core.create_worktree(&main, new_worktree("feature"))).unwrap();

    let setup = added.setup.expect("a setup session");
    assert_eq!(wait_exit(&mut events, setup), Some(0));
    assert_eq!(
        fs::read_to_string(&out).unwrap().trim_end(),
        added.worktree.path.to_str().unwrap()
    );
}

#[test]
fn a_setup_that_fails_is_reported_with_its_exit_code() {
    let dir = short_temp_dir();
    let (core, mut events, main) = core_with_scripts(dir.path(), Some("exit 3"), None);

    let added = block_on(core.create_worktree(&main, new_worktree("feature"))).unwrap();

    let session = added.setup.unwrap();
    let worktree = &added.worktree.path;
    let failed = Setup {
        session,
        state: SetupState::Failed { code: Some(3) },
    };
    assert_eq!(wait_setup_end(&mut events, worktree), failed);
    assert_eq!(core.setup(worktree), Some(failed));
}

#[test]
fn a_setup_that_exits_0_is_reported_as_succeeded() {
    let dir = short_temp_dir();
    let (core, mut events, main) = core_with_scripts(dir.path(), Some("true"), None);

    let added = block_on(core.create_worktree(&main, new_worktree("feature"))).unwrap();

    let worktree = &added.worktree.path;
    let succeeded = Setup {
        session: added.setup.unwrap(),
        state: SetupState::Succeeded,
    };
    assert_eq!(wait_setup_end(&mut events, worktree), succeeded);
    assert_eq!(core.setup(worktree), Some(succeeded));
}

#[test]
fn a_blank_setup_script_runs_no_setup() {
    let dir = short_temp_dir();
    let (core, _events, main) = core_with_scripts(dir.path(), Some("  "), None);

    let added = block_on(core.create_worktree(&main, new_worktree("feature"))).unwrap();

    assert_eq!(added.setup, None);
    assert_eq!(core.setup(&added.worktree.path), None);
}

// Polls `attach` until the snapshot shows `text`, since the shell prints after the session starts.
fn wait_attached_text(core: &Core, session: SessionId, text: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (sink, _frames) = tokio::sync::mpsc::unbounded_channel();
        let snapshot = core.attach(session, sink).unwrap();
        if String::from_utf8_lossy(&snapshot.bytes).contains(text) {
            return;
        }
        assert!(Instant::now() < deadline, "{text:?} never showed");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn setup_output_can_be_attached_while_it_runs_and_after_it_fails() {
    let dir = short_temp_dir();
    let go = dir.path().join("go");
    let script = format!(
        "echo first-line; while [ ! -e '{}' ]; do sleep 0.05; done; echo npm-ERR-missing; exit 3",
        go.display()
    );
    let (core, mut events, main) = core_with_scripts(dir.path(), Some(&script), None);
    let added = block_on(core.create_worktree(&main, new_worktree("feature"))).unwrap();
    let session = added.setup.unwrap();

    wait_attached_text(&core, session, "first-line");
    fs::write(&go, "").unwrap();
    wait_setup_end(&mut events, &added.worktree.path);

    wait_attached_text(&core, session, "npm-ERR-missing");
}

const DELETE: RemoveOpts = RemoveOpts::Delete {
    confirmed: false,
    delete_branch: false,
};

#[test]
fn delete_runs_cleanup_in_the_worktree_first_then_removes_it() {
    let dir = short_temp_dir();
    let out = dir.path().join("cleaned");
    // a.txt is only there while the worktree is.
    let script = format!("test -f a.txt && pwd -P > '{}'", out.display());
    let (core, mut events, main) = core_with_scripts(dir.path(), Some("true"), Some(&script));
    let added = block_on(core.create_worktree(&main, new_worktree("feature"))).unwrap();
    let worktree = added.worktree.path;
    wait_setup_end(&mut events, &worktree);

    block_on(core.remove_worktree(&worktree, DELETE)).unwrap();

    assert_eq!(
        fs::read_to_string(&out).unwrap().trim_end(),
        worktree.to_str().unwrap()
    );
    assert!(!worktree.exists());
    // Deleting the worktree also drops its setup and the setup's output.
    assert_eq!(core.setup(&worktree), None);
    let (sink, _frames) = tokio::sync::mpsc::unbounded_channel();
    assert!(matches!(
        core.attach(added.setup.unwrap(), sink),
        Err(CoreError::SessionNotRunning(_))
    ));
}

#[test]
fn a_cleanup_that_fails_stops_the_delete_and_reports_its_output() {
    let dir = short_temp_dir();
    let (core, _events, main) =
        core_with_scripts(dir.path(), None, Some("echo cannot-stop-db; exit 4"));
    let worktree = block_on(core.create_worktree(&main, new_worktree("feature")))
        .unwrap()
        .worktree
        .path;

    let deleted = block_on(core.remove_worktree(&worktree, DELETE));

    match deleted {
        Err(CoreError::CleanupFailed {
            worktree: w,
            code,
            output,
        }) => {
            assert_eq!(w, worktree);
            assert_eq!(code, Some(4));
            assert!(output.contains("cannot-stop-db"), "{output:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(worktree.join("a.txt").exists());
    assert!(
        block_on(core.list_worktrees())
            .iter()
            .any(|w| w.path == worktree)
    );
}

#[test]
fn forget_runs_no_cleanup() {
    let dir = short_temp_dir();
    let out = dir.path().join("cleaned");
    let script = format!("touch '{}'", out.display());
    let (core, _events, main) = core_with_scripts(dir.path(), None, Some(&script));
    let worktree = block_on(core.create_worktree(&main, new_worktree("feature")))
        .unwrap()
        .worktree
        .path;

    block_on(core.remove_worktree(&worktree, RemoveOpts::Forget)).unwrap();

    assert!(!out.exists());
    assert!(worktree.exists());
}

#[test]
fn setup_and_cleanup_write_nothing_into_the_repo() {
    let dir = short_temp_dir();
    let (core, mut events, main) = core_with_scripts(dir.path(), Some("true"), Some("true"));
    let worktree = block_on(core.create_worktree(&main, new_worktree("feature")))
        .unwrap()
        .worktree
        .path;
    wait_setup_end(&mut events, &worktree);

    assert_eq!(git(&worktree, &["status", "--porcelain", "--ignored"]), "");
    block_on(core.remove_worktree(&worktree, DELETE)).unwrap();
    assert_eq!(git(&main, &["status", "--porcelain", "--ignored"]), "");
}

// Waits until the fake agent has logged its start, and returns the log.
fn wait_agent_log(dir: &Path) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let log = fs::read_to_string(agent_log(dir)).unwrap_or_default();
        if !log.is_empty() {
            return log;
        }
        assert!(Instant::now() < deadline, "the agent never started");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn an_agent_session_started_with_the_worktree_starts_only_after_setup_exits_0() {
    let dir = short_temp_dir();
    let go = dir.path().join("go");
    let script = format!(
        "while [ ! -e '{}' ]; do sleep 0.05; done; touch setup-done",
        go.display()
    );
    let (core, _events, main) = core_with_scripts(dir.path(), Some(&script), None);
    let worktree = block_on(core.create_worktree(&main, new_worktree("feature")))
        .unwrap()
        .worktree
        .path;

    let started = thread::scope(|scope| {
        let waiting = scope
            .spawn(|| block_on(core.spawn_agent_after_setup(&worktree, claude_in_terminal_mode())));
        thread::sleep(Duration::from_millis(300));
        assert!(!agent_log(dir.path()).exists(), "started during setup");
        assert!(!waiting.is_finished());
        fs::write(&go, "").unwrap();
        waiting.join().unwrap()
    });

    started.unwrap();
    assert_eq!(wait_agent_log(dir.path()), "after-setup\n");
}

#[test]
fn an_agent_session_started_with_the_worktree_does_not_start_when_setup_fails() {
    let dir = short_temp_dir();
    let (core, _events, main) = core_with_scripts(dir.path(), Some("exit 3"), None);
    let worktree = block_on(core.create_worktree(&main, new_worktree("feature")))
        .unwrap()
        .worktree
        .path;

    let started = block_on(core.spawn_agent_after_setup(&worktree, claude_in_terminal_mode()));

    match started {
        Err(CoreError::SetupFailed { worktree: w, code }) => {
            assert_eq!(w, worktree);
            assert_eq!(code, Some(3));
        }
        other => panic!("{other:?}"),
    }
    thread::sleep(Duration::from_millis(200));
    assert!(!agent_log(dir.path()).exists());
}

#[test]
fn without_a_setup_script_an_agent_session_starts_at_once() {
    let dir = short_temp_dir();
    let (core, _events, main) = core_with_scripts(dir.path(), None, None);
    let worktree = block_on(core.create_worktree(&main, new_worktree("feature")))
        .unwrap()
        .worktree
        .path;

    block_on(core.spawn_agent_after_setup(&worktree, claude_in_terminal_mode())).unwrap();

    assert_eq!(wait_agent_log(dir.path()), "before-setup\n");
}

fn add_worktree(core: &Core, main: &Path, branch: &str) -> PathBuf {
    block_on(core.create_worktree(main, new_worktree(branch)))
        .unwrap()
        .worktree
        .path
}

fn tracked(core: &Core, worktree: &Path) -> bool {
    core.sessions(worktree).is_ok()
}

// Whether `check` holds within `limit`. It does not panic, so a test can let a waiting script go
// before it asserts.
fn within(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while !check() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

// Commits in the worktree, as a cleanup script, with the user's git config turned off.
const COMMITTING_CLEANUP: &str = "GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
    git -c user.name=Cleanup -c user.email=cleanup@grove.invalid \
    commit -q --allow-empty -m cleanup";

#[test]
fn an_unconfirmed_delete_stops_when_cleanup_commits_and_keeps_the_commit() {
    let dir = short_temp_dir();
    let (core, _events, main) = core_with_scripts(dir.path(), None, Some(COMMITTING_CLEANUP));
    let worktree = add_worktree(&core, &main, "feature");

    let deleted = block_on(core.remove_worktree(&worktree, DELETE));

    assert!(
        matches!(
            deleted,
            Err(CoreError::DeleteNeedsConfirmation {
                worktree: ref w,
                uncommitted: false,
                unpushed: 1,
            }) if *w == worktree
        ),
        "{deleted:?}"
    );
    assert_eq!(git(&worktree, &["log", "-1", "--format=%s"]), "cleanup\n");
    assert!(tracked(&core, &worktree));
}

#[test]
fn a_confirmed_delete_goes_ahead_when_cleanup_commits() {
    let dir = short_temp_dir();
    let (core, _events, main) = core_with_scripts(dir.path(), None, Some(COMMITTING_CLEANUP));
    let worktree = add_worktree(&core, &main, "feature");

    let opts = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };
    let branch = block_on(core.remove_worktree(&worktree, opts)).unwrap();

    assert_eq!(
        branch,
        BranchOutcome::Kept {
            name: "feature".into(),
            merged: false
        }
    );
    assert!(!worktree.exists());
}

#[test]
fn deleting_the_branch_stops_when_cleanup_commits_to_it() {
    let dir = short_temp_dir();
    let (core, _events, main) = core_with_scripts(dir.path(), None, Some(COMMITTING_CLEANUP));
    let worktree = add_worktree(&core, &main, "feature");

    let opts = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: true,
    };
    let deleted = block_on(core.remove_worktree(&worktree, opts));

    assert!(
        matches!(
            deleted,
            Err(CoreError::BranchNotMerged { ref branch, ref base })
                if branch == "feature" && base == "main"
        ),
        "{deleted:?}"
    );
    assert!(worktree.join("a.txt").exists());
    assert!(tracked(&core, &worktree));
    git(
        &main,
        &["rev-parse", "--verify", "-q", "refs/heads/feature"],
    );
}

#[test]
fn a_worktree_added_at_the_same_path_while_a_delete_finishes_keeps_its_setup() {
    let dir = short_temp_dir();
    let go = dir.path().join("go");
    let script = format!(
        "while [ ! -e '{}' ]; do sleep 0.05; done; touch setup-done",
        go.display()
    );
    let (core, _events, main) = core_with_scripts(dir.path(), Some(&script), None);
    let worktree = add_worktree(&core, &main, "feature");

    let (readded, deleted) = block_on(async {
        let mut delete = pin!(core.remove_worktree(&worktree, DELETE));
        // Polled only until the worktree leaves the list, so what the delete does after that
        // waits until the worktree is added again.
        let finished = poll_fn(|cx| {
            if !tracked(&core, &worktree) {
                return Poll::Ready(None);
            }
            let polled = delete.as_mut().poll(cx).map(Some);
            if polled.is_pending() {
                cx.waker().wake_by_ref();
            }
            polled
        })
        .await;
        // The same branch, so the same path.
        let readded = core.create_worktree(&main, new_worktree("feature")).await;
        let deleted = match finished {
            Some(deleted) => deleted,
            None => delete.await,
        };
        (readded.unwrap(), deleted)
    });

    deleted.unwrap();
    assert_eq!(readded.worktree.path, worktree);
    let running = Setup {
        session: readded.setup.unwrap(),
        state: SetupState::Running,
    };
    assert_eq!(core.setup(&worktree), Some(running));
    let (waited, started) = thread::scope(|scope| {
        let waiting = scope
            .spawn(|| block_on(core.spawn_agent_after_setup(&worktree, claude_in_terminal_mode())));
        thread::sleep(Duration::from_millis(300));
        let waited = !agent_log(dir.path()).exists() && !waiting.is_finished();
        fs::write(&go, "").unwrap();
        (waited, waiting.join().unwrap())
    });
    assert!(waited, "the agent started during setup");
    started.unwrap();
    assert_eq!(wait_agent_log(dir.path()), "after-setup\n");
}

#[test]
fn a_forget_or_delete_of_a_worktree_being_deleted_fails_at_once_and_runs_no_cleanup() {
    let dir = short_temp_dir();
    let (go, log) = (dir.path().join("go"), dir.path().join("cleanups"));
    let script = format!(
        "echo ran >> '{}'; while [ ! -e '{}' ]; do sleep 0.05; done",
        log.display(),
        go.display()
    );
    let (core, _events, main) = core_with_scripts(dir.path(), None, Some(&script));
    let worktree = add_worktree(&core, &main, "feature");
    let confirmed = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };

    let (cleaning, first, at_once, again, forgot, forgot_repo) = thread::scope(|scope| {
        let first = scope.spawn(|| block_on(core.remove_worktree(&worktree, DELETE)));
        let cleaning = within(TIMEOUT, || log.exists());
        let again = scope.spawn(|| block_on(core.remove_worktree(&worktree, confirmed)));
        let at_once = within(Duration::from_secs(1), || again.is_finished());
        let forgot = block_on(core.remove_worktree(&worktree, RemoveOpts::Forget));
        let forgot_repo = block_on(core.remove_worktree(&main, RemoveOpts::ForgetRepo));
        fs::write(&go, "").unwrap();
        let first = first.join().unwrap();
        (
            cleaning,
            first,
            at_once,
            again.join().unwrap(),
            forgot,
            forgot_repo,
        )
    });

    assert!(cleaning, "the first cleanup never started");
    assert!(at_once, "the second delete waited");
    for refused in [&again, &forgot, &forgot_repo] {
        assert!(
            matches!(refused, Err(CoreError::Removing(p)) if *p == worktree),
            "{refused:?}"
        );
    }
    first.unwrap();
    assert_eq!(fs::read_to_string(&log).unwrap(), "ran\n");
    assert!(!worktree.exists());
    // The refused ForgetRepo left no worktree of the repo marked as being removed.
    block_on(core.remove_worktree(&main, RemoveOpts::ForgetRepo)).unwrap();
}

#[test]
fn deletes_of_different_worktrees_run_side_by_side() {
    let dir = short_temp_dir();
    let (go, log) = (dir.path().join("go"), dir.path().join("cleanups"));
    let script = format!(
        "pwd -P >> '{}'; while [ ! -e '{}' ]; do sleep 0.05; done",
        log.display(),
        go.display()
    );
    let (core, _events, main) = core_with_scripts(dir.path(), None, Some(&script));
    let one = add_worktree(&core, &main, "one");
    let two = add_worktree(&core, &main, "two");

    let (side_by_side, first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| block_on(core.remove_worktree(&one, DELETE)));
        let second = scope.spawn(|| block_on(core.remove_worktree(&two, DELETE)));
        // Neither cleanup ends before `go`, so both running means they run at once.
        let side_by_side = within(TIMEOUT, || {
            fs::read_to_string(&log).is_ok_and(|ran| ran.lines().count() == 2)
        });
        fs::write(&go, "").unwrap();
        (side_by_side, first.join().unwrap(), second.join().unwrap())
    });

    assert!(side_by_side, "the cleanups did not run at once");
    first.unwrap();
    second.unwrap();
    assert!(!one.exists() && !two.exists());
}
