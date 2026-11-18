//! Setup and cleanup scripts: commands from the repo's profile that grove runs in a new worktree
//! before agents start, and in a worktree before deleting it. They live in grove's profiles and
//! repo settings, never in the repo.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::Duration;

use tokio::sync::watch;

use crate::session::{Kind, kill_session, start_thread};
use crate::shell_env::{Env, spawn_shell_in_own_session, wait_for_exit};
use crate::{AgentSpec, Core, CoreError, CoreEvent, Result, SessionId, SpawnSpec};

/// The setup script grove ran in a worktree, as `Core::setup` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    /// The run session running the script. Its output stays attachable after it ends.
    pub session: SessionId,
    pub state: SetupState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupState {
    Running,
    /// It exited 0.
    Succeeded,
    /// It exited non-zero, or a signal ended it (code None).
    Failed {
        code: Option<i32>,
    },
}

/// Each worktree's setup since the core started, by worktree. Receivers wait for its end.
pub(crate) type Setups = Mutex<HashMap<PathBuf, watch::Sender<Setup>>>;

// The size a setup session starts at. Its panel resizes it on attach.
const SETUP_COLS: u16 = 80;
const SETUP_ROWS: u16 = 24;

// How long a cleanup script may run before grove stops it and the delete fails.
const CLEANUP_LIMIT: Duration = Duration::from_secs(5 * 60);
// How many of a failed cleanup's last lines its error keeps.
const CLEANUP_OUTPUT_LINES: usize = 20;
// How much of a cleanup's latest output grove holds while it runs, for those lines.
const CLEANUP_OUTPUT_TAIL: usize = 64 * 1024;
// How long a failed cleanup's output may take to end after the script exits. A process it left
// running can hold the pipe open.
const CLEANUP_DRAIN: Duration = Duration::from_secs(1);

/// A repo's cleanup script, ready to run off the tokio workers.
pub(crate) struct Cleanup {
    shell: PathBuf,
    env: Env,
    script: String,
    limit: Duration,
}

impl Core {
    /// The setup grove ran in `worktree` since it started. None when it ran none.
    pub fn setup(&self, worktree: &Path) -> Option<Setup> {
        self.setups().get(worktree).map(|setup| *setup.borrow())
    }

    /// Starts the setup script of the repo whose main worktree is `repo` in `worktree`, a
    /// worktree grove just added, as a run session. None when the repo has no setup script.
    pub(crate) fn start_setup(&self, repo: &Path, worktree: &Path) -> Result<Option<SessionId>> {
        let Some(script) = non_blank(self.settings_of(repo).setup) else {
            return Ok(None);
        };
        let session = self.sessions.new_id();
        let exited = self.sessions.keep(session);
        let running = Setup {
            session,
            state: SetupState::Running,
        };
        // Recorded before the session starts, so a removal that comes meanwhile drops it.
        let (setup, _) = watch::channel(running);
        if let Some(old) = self.setups().insert(worktree.to_path_buf(), setup) {
            self.sessions.release(old.borrow().session);
        }
        let command = SpawnSpec {
            command: self.run_shell.clone(),
            args: vec!["-c".into(), script.into()],
            cols: SETUP_COLS,
            rows: SETUP_ROWS,
        };
        if let Err(e) = self.start_session(session, worktree, command, &[], Kind::Command) {
            self.end_setup(worktree, session);
            return Err(e);
        }
        let _ = self.events.send(CoreEvent::Setup {
            worktree: worktree.to_path_buf(),
            setup: running,
        });

        let (setups, events, worktree) = (
            self.setups.clone(),
            self.events.clone(),
            worktree.to_path_buf(),
        );
        self.runtime.spawn(async move {
            // The sender drops without a code only when the core stops.
            let code = exited.await.unwrap_or(None);
            let state = match code {
                Some(0) => SetupState::Succeeded,
                code => SetupState::Failed { code },
            };
            let ended = Setup { session, state };
            // Skip a worktree removed meanwhile, or one whose setup started again.
            let current = lock(&setups)
                .get(&worktree)
                .filter(|setup| setup.borrow().session == session)
                .map(|setup| setup.send_replace(ended));
            if current.is_some() {
                let _ = events.send(CoreEvent::Setup {
                    worktree,
                    setup: ended,
                });
            }
        });
        Ok(Some(session))
    }

    /// Starts an agent session in `worktree` once its setup exits 0, or at once when no setup
    /// runs there. When setup failed it starts nothing and fails with `SetupFailed`.
    /// `spawn_agent` itself never waits for setup.
    pub async fn spawn_agent_after_setup(
        &self,
        worktree: &Path,
        spec: AgentSpec,
    ) -> Result<SessionId> {
        let setup = self.setups().get(worktree).map(watch::Sender::subscribe);
        if let Some(mut setup) = setup
            // Fails only when the worktree is removed meanwhile. `spawn_agent` then refuses it.
            && let Ok(ended) = setup.wait_for(|s| s.state != SetupState::Running).await
            && let SetupState::Failed { code } = ended.state
        {
            return Err(CoreError::SetupFailed {
                worktree: worktree.to_path_buf(),
                code,
            });
        }
        self.spawn_agent(worktree, spec)
    }

    /// Forgets `session`, the setup of `worktree`, with its kept output. A newer setup there, of
    /// a worktree added at the same path since, stays.
    pub(crate) fn end_setup(&self, worktree: &Path, session: SessionId) {
        let mut setups = self.setups();
        if setups
            .get(worktree)
            .is_some_and(|setup| setup.borrow().session == session)
        {
            setups.remove(worktree);
            drop(setups);
            self.sessions.release(session);
        }
    }

    /// The cleanup script of the repo whose main worktree is `repo`. None when it has none.
    pub(crate) fn cleanup(&self, repo: &Path) -> Option<Cleanup> {
        Some(Cleanup {
            shell: self.run_shell.clone(),
            env: self.session_env.clone(),
            script: non_blank(self.settings_of(repo).cleanup)?,
            limit: CLEANUP_LIMIT,
        })
    }

    fn setups(&self) -> MutexGuard<'_, HashMap<PathBuf, watch::Sender<Setup>>> {
        lock(&self.setups)
    }
}

impl Cleanup {
    /// Runs the script in `worktree` with the user's shell and the session environment, with no
    /// terminal and no input. Fails unless it exits 0 within its limit. Past the limit, grove
    /// kills the script and everything it started, except a process that left its session
    /// (setsid). It blocks while the script runs.
    pub(crate) fn run(&self, worktree: &Path) -> Result<()> {
        let failed_to = |action| CoreError::io(action, worktree);
        let (mut reader, writer) =
            io::pipe().map_err(failed_to("open a pipe for the cleanup script in"))?;
        // Its own session, so stopping it stops what it started too.
        let leader = spawn_shell_in_own_session(
            &self.shell,
            &["-c", &self.script],
            &self.env,
            Some(worktree),
            &writer,
            Some(&writer),
        )
        .map_err(failed_to("run the cleanup script in"))?;
        // Dropped at once, so only the script holds the pipe's write end. The reader then sees
        // the end of output once the script and everything it started exit.
        drop(writer);

        let output = Arc::new(Mutex::new(Vec::new()));
        let (drained, output_ended) = mpsc::channel();
        start_thread("grove-cleanup-output", {
            let output = output.clone();
            move || {
                let mut buf = [0; 8192];
                while let Ok(n @ 1..) = reader.read(&mut buf) {
                    hold(&mut lock(&output), &buf[..n]);
                }
                let _ = drained.send(());
            }
        })?;
        let (exited, exit) = mpsc::channel();
        start_thread("grove-cleanup-wait", move || {
            let _ = exited.send(wait_for_exit(leader));
        })?;

        let code = match exit.recv_timeout(self.limit) {
            Ok(status) => status
                .map_err(failed_to("wait for the cleanup script in"))?
                .code(),
            Err(_) => {
                kill_session(leader);
                let _ = exit.recv();
                None
            }
        };
        if code == Some(0) {
            return Ok(());
        }
        let _ = output_ended.recv_timeout(CLEANUP_DRAIN);
        Err(CoreError::CleanupFailed {
            worktree: worktree.to_path_buf(),
            code,
            output: last_lines(&lock(&output), CLEANUP_OUTPUT_LINES),
        })
    }
}

// Adds `read` to a running cleanup's `held` output, which keeps only its latest bytes.
fn hold(held: &mut Vec<u8>, read: &[u8]) {
    held.extend_from_slice(read);
    // Trimmed in batches, so a chatty script costs no copy on every read.
    if held.len() > 2 * CLEANUP_OUTPUT_TAIL {
        held.drain(..held.len() - CLEANUP_OUTPUT_TAIL);
    }
}

// The last `count` lines of `bytes` as text, without trailing whitespace.
fn last_lines(bytes: &[u8], count: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

// A blank script counts as none, so a repo turns its profile's script off with "".
fn non_blank(script: Option<String>) -> Option<String> {
    script.filter(|s| !s.trim().is_empty())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn cleanup(script: &str, limit: Duration) -> Cleanup {
        Cleanup {
            shell: "/bin/sh".into(),
            env: Env::new(),
            script: script.into(),
            limit,
        }
    }

    #[test]
    fn a_cleanup_past_its_limit_is_stopped_with_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let script = "sleep 30 & echo started; wait";
        let started = Instant::now();

        let ran = cleanup(script, Duration::from_millis(200)).run(dir.path());

        // The background sleep holds the output pipe, so this also shows it was stopped.
        assert!(started.elapsed() < Duration::from_secs(5), "{ran:?}");
        match ran {
            Err(CoreError::CleanupFailed { code, output, .. }) => {
                assert_eq!(code, None);
                assert_eq!(output, "started");
            }
            other => panic!("{other:?}"),
        }
    }

    // Whether `pid` ends within a few seconds.
    fn ends(pid: libc::pid_t) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if unsafe { libc::kill(pid, 0) } == -1 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_cleanup_past_its_limit_is_stopped_with_jobs_in_their_own_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        // Job control gives the background job its own process group.
        let script = format!(
            "set -m; sleep 30 & echo $! > '{}'; wait",
            pid_file.display()
        );

        let ran = cleanup(&script, Duration::from_millis(500)).run(dir.path());

        assert!(
            matches!(ran, Err(CoreError::CleanupFailed { code: None, .. })),
            "{ran:?}"
        );
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let ended = ends(pid);
        if !ended {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(ended, "the background job outlived the cleanup");
    }

    #[test]
    fn a_running_cleanup_holds_only_its_latest_output() {
        let mut all = Vec::new();
        let mut held = Vec::new();

        for i in 0..100_000 {
            let read = format!("line{i}\n");
            all.extend_from_slice(read.as_bytes());
            hold(&mut held, read.as_bytes());
            assert!(
                held.len() <= 2 * CLEANUP_OUTPUT_TAIL,
                "{} bytes",
                held.len()
            );
        }

        assert!(held.len() >= CLEANUP_OUTPUT_TAIL);
        assert!(all.ends_with(&held));
    }

    #[test]
    fn a_failed_cleanup_keeps_its_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let script = "for i in $(seq 1 30); do echo line$i; done; echo oops >&2; exit 1";

        let ran = cleanup(script, Duration::from_secs(10)).run(dir.path());

        let Err(CoreError::CleanupFailed { output, .. }) = ran else {
            panic!("{ran:?}");
        };
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), CLEANUP_OUTPUT_LINES);
        assert_eq!(lines.first(), Some(&"line12"));
        assert_eq!(lines.last(), Some(&"oops"));
    }

    // A worktree with its own `bin/sh`. The cleanups below create `ran` in it with a builtin.
    fn worktree_with_its_own_shell() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", dir.path().join("bin/sh")).unwrap();
        dir
    }

    #[test]
    fn a_relative_shell_path_is_found_in_the_worktree() {
        let dir = worktree_with_its_own_shell();
        let mut cleanup = cleanup(": > ran", Duration::from_secs(10));
        cleanup.shell = "bin/sh".into();

        let ran = cleanup.run(dir.path());

        assert!(ran.is_ok(), "{ran:?}");
        assert!(dir.path().join("ran").exists());
    }

    #[test]
    fn a_relative_path_entry_is_searched_in_the_worktree() {
        let dir = worktree_with_its_own_shell();
        let mut cleanup = cleanup(": > ran", Duration::from_secs(10));
        cleanup.shell = "sh".into();
        cleanup.env.insert("PATH".into(), "bin".into());

        let ran = cleanup.run(dir.path());

        assert!(ran.is_ok(), "{ran:?}");
        assert!(dir.path().join("ran").exists());
    }
}
