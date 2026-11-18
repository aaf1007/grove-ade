//! Run sessions: named commands from a repo's profile, each started in one of its worktrees.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::profile::{Profiles, overrides_text, parse_overrides};
use crate::remove::repo_of;
use crate::restore::{StoppedRun, lock};
use crate::session::Kind;
use crate::{
    Agent, Core, CoreError, CoreEvent, ProfileSettings, Result, SessionId, SpawnSpec, WorktreeInfo,
};

/// Each repo's settings, keyed by its main worktree.
pub(crate) type Settings = HashMap<PathBuf, RepoSettings>;

/// What grove saves for a repo in its own state, never in the repo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoSettings {
    /// The name of the profile the repo uses. None uses none.
    pub profile: Option<String>,
    /// The repo's own values. Each one it sets wins over the profile's.
    pub overrides: ProfileSettings,
}

/// Extra args for agent sessions, one list per agent, since their flags differ. They go after
/// grove's own flags and the prompt, so a flag that takes several values cannot swallow the
/// prompt. An arg that would replace one of grove's own flags or overrides is refused.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchArgs {
    pub claude: Option<Vec<String>>,
    pub codex: Option<Vec<String>>,
}

/// A named command from the profile, such as a dev server or the tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSession {
    /// Unique within its list.
    pub name: String,
    /// Run by the user's shell with `-c`, so it can hold pipes and variable assignments.
    pub command: String,
    /// Where it serves, for the port router.
    pub port: Option<u16>,
}

/// A run session to start with `Core::spawn_run`.
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// The name of one of the repo's run sessions.
    pub name: String,
    pub cols: u16,
    pub rows: u16,
}

impl Core {
    /// The settings of the repo whose main worktree is at `repo`.
    pub fn repo_settings(&self, repo: &Path) -> Result<RepoSettings> {
        let tracked = self.tracked();
        check_repo(&tracked, repo)?;
        Ok(self.settings().get(repo).cloned().unwrap_or_default())
    }

    /// Replaces the settings of the repo whose main worktree is at `repo`. Sends one
    /// `RepoSettingsChanged` once they are saved.
    pub async fn set_repo_settings(&self, repo: &Path, settings: RepoSettings) -> Result<()> {
        let overrides = &settings.overrides;
        check_run_sessions(overrides.run_sessions.as_deref().unwrap_or_default())?;
        self.agents.check_launch_args(&overrides.launch_args)?;
        if let Some(name) = &settings.profile
            && self.profiles.get(name).is_none()
        {
            return Err(CoreError::UnknownProfile(name.clone()));
        }
        {
            // Held across the insert, so a repo forgotten meanwhile leaves no settings behind.
            let tracked = self.tracked();
            check_repo(&tracked, repo)?;
            self.settings().insert(repo.to_path_buf(), settings);
        }
        self.save_state().await?;
        let _ = self.events.send(CoreEvent::RepoSettingsChanged {
            repo: repo.to_path_buf(),
            by: None,
        });
        Ok(())
    }

    /// The overrides of the repo whose main worktree is at `repo`, as TOML in a profile file's
    /// schema without `name` and `suggest_for`. Empty when it overrides nothing.
    pub fn repo_overrides_text(&self, repo: &Path) -> Result<String> {
        let overrides = self.repo_settings(repo)?.overrides;
        overrides_text(overrides)
            .map_err(|e| CoreError::io("encode the overrides of", repo)(io::Error::other(e)))
    }

    /// Replaces the overrides of the repo whose main worktree is at `repo` with those `text`
    /// holds, keeping its profile. Comments are not kept. Saves nothing and fails with
    /// BadOverrides, at the line where TOML gives one, when `text` is not valid.
    pub async fn set_repo_overrides_text(&self, repo: &Path, text: &str) -> Result<()> {
        let mut settings = self.repo_settings(repo)?;
        settings.overrides = parse_overrides(text, &self.agents)?;
        self.set_repo_settings(repo, settings).await
    }

    /// The run sessions of the repo whose main worktree is at `repo`: its profile's, or its own
    /// when its overrides set them.
    pub fn run_sessions(&self, repo: &Path) -> Result<Vec<RunSession>> {
        check_repo(&self.tracked(), repo)?;
        Ok(self.settings_of(repo).run_sessions.unwrap_or_default())
    }

    /// Starts one of the repo's run sessions in `worktree`, a tracked worktree of that repo. A
    /// stopped run session of that name there, one that exited or one a restart brought back,
    /// gives way to it. Once it exits, `sessions` lists it as stopped and `attach` returns its
    /// final screen, until another of its name starts there or its worktree goes.
    pub fn spawn_run(&self, worktree: &Path, spec: RunSpec) -> Result<SessionId> {
        let repo = repo_of(&self.tracked(), worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?;
        let run = self
            .settings_of(&repo)
            .run_sessions
            .unwrap_or_default()
            .into_iter()
            .find(|r| r.name == spec.name)
            .ok_or(CoreError::UnknownRunSession {
                repo,
                name: spec.name,
            })?;
        let command = SpawnSpec {
            command: self.run_shell.clone(),
            args: vec!["-c".into(), run.command.into()],
            cols: spec.cols,
            rows: spec.rows,
        };
        let kind = Kind::Run {
            name: run.name.clone(),
            port: run.port,
        };
        let id = self.sessions.new_id();
        let exited = self.sessions.keep(id);
        let session = self
            .start_session(id, worktree, command, &[], kind)
            .inspect_err(|_| self.sessions.release(id))?;
        self.drop_stopped_runs(|w, name| w == worktree && name == run.name);
        self.saves.request();
        self.list_once_stopped(session, worktree, run.name, exited);
        Ok(session)
    }

    /// Forgets the stopped run sessions that `which` picks by worktree and name, with their kept
    /// output.
    pub(crate) fn drop_stopped_runs(&self, which: impl Fn(&Path, &str) -> bool) {
        let dropped: Vec<SessionId> = {
            let mut stopped = self.stopped_runs();
            let picked: Vec<SessionId> = stopped
                .iter()
                .filter(|(_, run)| which(&run.worktree, &run.name))
                .map(|(&id, _)| id)
                .collect();
            for id in &picked {
                stopped.remove(id);
            }
            picked
        };
        for id in dropped {
            self.sessions.release(id);
        }
    }

    // Once the run session exits, it is listed as stopped with its output kept, in place of any
    // older stopped one of its name there. One whose exit is recorded after a newer one of its
    // name stopped there, or whose worktree is being removed or is gone, is released instead.
    // The worktree check comes after the insert, so a removal either finds the entry or is seen
    // here.
    fn list_once_stopped(
        &self,
        session: SessionId,
        worktree: &Path,
        name: String,
        exited: oneshot::Receiver<Option<i32>>,
    ) {
        let (stopped_runs, removing, tracked, release, saves) = (
            Arc::clone(&self.stopped_runs),
            Arc::clone(&self.removing),
            Arc::clone(&self.worktrees),
            self.sessions.releaser(),
            self.saves.clone(),
        );
        let worktree = worktree.to_path_buf();
        self.runtime.spawn(async move {
            // The sender drops without a code only when the core stops.
            if exited.await.is_err() {
                return;
            }
            let older: Vec<SessionId> = {
                let mut stopped = lock(&stopped_runs);
                let same_name: Vec<SessionId> = stopped
                    .iter()
                    .filter(|(_, run)| run.worktree == worktree && run.name == name)
                    .map(|(&id, _)| id)
                    .collect();
                // Ids only grow, so a larger one started later.
                if same_name.iter().any(|&id| id > session) {
                    drop(stopped);
                    release(session);
                    return;
                }
                for id in &same_name {
                    stopped.remove(id);
                }
                let run = StoppedRun {
                    worktree: worktree.clone(),
                    name,
                    comes_back_after_restart: false,
                };
                stopped.insert(session, run);
                same_name
            };
            for id in older {
                release(id);
            }
            let removed = lock(&removing).contains(&worktree)
                || !lock(&tracked).iter().any(|w| w.path == worktree);
            if removed {
                lock(&stopped_runs).remove(&session);
                release(session);
            }
            saves.request();
        });
    }

    /// The extra args of `worktree`'s repo for `agent`.
    pub(crate) fn launch_args(&self, worktree: &Path, agent: Agent) -> Vec<String> {
        let Some(repo) = repo_of(&self.tracked(), worktree) else {
            return Vec::new();
        };
        let args = self.settings_of(&repo).launch_args;
        match agent {
            Agent::Claude => args.claude,
            Agent::Codex => args.codex,
        }
        .unwrap_or_default()
    }

    // What the repo whose main worktree is at `repo` runs with, as `resolved` gives it.
    pub(crate) fn settings_of(&self, repo: &Path) -> ProfileSettings {
        let saved = self.settings().get(repo).cloned();
        resolved(saved, &self.profiles)
    }
}

// What a repo with `saved` settings runs with: its profile with its overrides applied. A
// profile that is gone or not valid counts as empty, and so does a repo with none saved.
pub(crate) fn resolved(saved: Option<RepoSettings>, profiles: &Profiles) -> ProfileSettings {
    let Some(settings) = saved else {
        return ProfileSettings::default();
    };
    let profile = settings.profile.and_then(|name| profiles.get(&name));
    settings
        .overrides
        .applied_to(profile.map(|p| p.settings).unwrap_or_default())
}

// Each run session needs a name, unique in its list, and a command.
pub(crate) fn check_run_sessions(runs: &[RunSession]) -> Result<()> {
    for (i, run) in runs.iter().enumerate() {
        let reason = if run.name.trim().is_empty() {
            "has no name"
        } else if run.command.trim().is_empty() {
            "has no command"
        } else if runs[..i].iter().any(|r| r.name == run.name) {
            "is saved twice"
        } else {
            continue;
        };
        return Err(CoreError::InvalidRunSession {
            name: run.name.clone(),
            reason,
        });
    }
    Ok(())
}

fn check_repo(tracked: &[WorktreeInfo], repo: &Path) -> Result<()> {
    if tracked.iter().any(|w| w.is_main && w.path == repo) {
        Ok(())
    } else {
        Err(CoreError::UnknownRepo(repo.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::{CoreConfig, SessionKind};

    // A core whose repo saves one run session, "tests", running `command`. Run sessions start in
    // `shell`, or the user's shell when it is None.
    fn core_with_tests_run(
        dir: &Path,
        shell: Option<PathBuf>,
        command: &str,
    ) -> (Core, PathBuf, tokio::runtime::Runtime) {
        let mut cfg = CoreConfig::new(dir.join("home"));
        cfg.login_shell = shell;
        let (core, _events) = Core::start(cfg).unwrap();
        let repo = dir.join("repo");
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap();
        assert!(init.success());
        let tests = RunSession {
            name: "tests".into(),
            command: command.into(),
            port: None,
        };
        let settings = RepoSettings {
            overrides: ProfileSettings {
                run_sessions: Some(vec![tests]),
                ..ProfileSettings::default()
            },
            ..RepoSettings::default()
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let worktree = runtime.block_on(async {
            core.add_repo(&repo).await.unwrap();
            let worktree = core.list_worktrees().await[0].path.clone();
            core.set_repo_settings(&worktree, settings).await.unwrap();
            worktree
        });
        (core, worktree, runtime)
    }

    // macOS caps a socket path at 104 bytes, so the home folder stays under /tmp.
    fn short_temp_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("grove")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn tests_spec() -> RunSpec {
        RunSpec {
            name: "tests".into(),
            cols: 80,
            rows: 24,
        }
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    // The worktree's run sessions as `sessions` lists them, with whether each runs.
    fn listed_runs(core: &Core, worktree: &Path) -> Vec<(SessionId, bool)> {
        let sessions = core.sessions(worktree).unwrap();
        sessions
            .into_iter()
            .filter(|info| matches!(info.kind, SessionKind::Run { .. }))
            .map(|info| (info.session, info.running))
            .collect()
    }

    fn screen(core: &Core, session: SessionId) -> String {
        let (sink, _frames) = unbounded_channel();
        String::from_utf8_lossy(&core.attach(session, sink).unwrap().bytes).into_owned()
    }

    #[test]
    fn a_run_session_that_fails_to_start_is_not_kept() {
        let dir = short_temp_dir();
        // Run sessions start in this shell, which does not exist.
        let shell = Some(dir.path().join("no-such-shell"));
        let (core, worktree, _runtime) = core_with_tests_run(dir.path(), shell, "true");

        assert!(core.spawn_run(&worktree, tests_spec()).is_err());
        assert_eq!(core.sessions.kept_count(), 0);
    }

    #[test]
    fn an_older_run_sessions_late_exit_leaves_the_newer_one_that_stopped() {
        let dir = short_temp_dir();
        let (core, worktree, runtime) = core_with_tests_run(dir.path(), None, "echo newer");
        // The older one starts as spawn_run starts it, but the test decides when its exit is
        // recorded.
        let older = core.sessions.new_id();
        let exited = core.sessions.keep(older);
        let command = SpawnSpec {
            command: core.run_shell.clone(),
            args: vec!["-c".into(), "echo older".into()],
            cols: 80,
            rows: 24,
        };
        let kind = Kind::Run {
            name: "tests".into(),
            port: None,
        };
        core.start_session(older, &worktree, command, &[], kind)
            .unwrap();
        let (record_exit, exit_recorded) = oneshot::channel();
        core.list_once_stopped(older, &worktree, "tests".into(), exit_recorded);
        let code = runtime.block_on(exited).unwrap();

        let newer = core.spawn_run(&worktree, tests_spec()).unwrap();
        wait_until("the newer one is listed stopped", || {
            listed_runs(&core, &worktree) == [(newer, false)]
        });
        record_exit.send(code).unwrap();
        wait_until("one run session is kept", || {
            core.sessions.kept_count() == 1
        });

        assert_eq!(listed_runs(&core, &worktree), [(newer, false)]);
        wait_until("the newer one's output shows", || {
            screen(&core, newer).contains("newer")
        });
    }
}
