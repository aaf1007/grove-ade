//! Agent session records. An agent session is listed from its start until the user deletes it,
//! its process ended or not, and the state file keeps it. An ended one can be resumed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::time::SystemTime;

use tokio::sync::broadcast;

use super::{Agent, AgentState, Agents, Mode, Switch, Tracked, lock};
use crate::remove::{EXIT_WAIT, wait_for_exits};
use crate::{Core, CoreError, CoreEvent, Result, SessionId};

/// An agent session as `Core::agent_sessions` lists it. It is listed from its start until the
/// user deletes it, its process ended or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionInfo {
    pub session: SessionId,
    pub worktree: PathBuf,
    pub agent: Agent,
    pub mode: Mode,
    /// The agent session that spawned it as a subagent, while that one is listed. None for
    /// one the user started.
    pub parent: Option<SessionId>,
    /// The first line of its first user message, or "New session" before that.
    pub title: String,
    /// When it started, by the wall clock, to the millisecond.
    pub created: SystemTime,
    /// Its process ended. `Core::resume_agent_session` starts a new one.
    pub ended: bool,
    /// None until its first hook, heuristic or turn.
    pub state: Option<AgentState>,
    /// When it entered `state`, by the wall clock. Before its first state, when it started or
    /// a restart brought it back.
    pub since: SystemTime,
}

/// The title of an agent session with no user message yet.
const NEW_SESSION_TITLE: &str = "New session";

// The title a user message gives a session: its first line with text, trimmed.
pub(super) fn title_of(message: &str) -> Option<String> {
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    Some(line.to_owned())
}

impl Agents {
    /// Titles the session after `message`, its first user message, and says the list changed.
    /// A session that has a title keeps it, and a message with no text gives none.
    pub(crate) fn title_from(
        &self,
        session: SessionId,
        message: &str,
        events: &broadcast::Sender<CoreEvent>,
    ) {
        let Some(title) = title_of(message) else {
            return;
        };
        {
            let mut live = lock(&self.live);
            let Some(tracked) = live.get_mut(&session).filter(|t| t.title.is_none()) else {
                return;
            };
            tracked.title = Some(title);
        }
        self.saves.request();
        // Having no receivers is fine.
        let _ = events.send(CoreEvent::AgentSessionsChanged);
    }

    /// The folder of the session's own files under the home folder.
    pub(crate) fn session_folder(&self, session: SessionId) -> PathBuf {
        self.home.join("sessions").join(session.to_string())
    }

    // `session` and every subagent session below it, with the worktree of `session`. Refused
    // while one of them switches mode or resumes.
    fn with_subagents(&self, session: SessionId) -> Result<(Vec<SessionId>, PathBuf)> {
        with_subagents(&lock(&self.live), session)
    }

    // Drops the records `with_subagents` finds, and returns them. Found and dropped under one
    // lock, so none of them starts a subagent after.
    fn drop_with_subagents(&self, session: SessionId) -> Result<Vec<SessionId>> {
        let mut live = lock(&self.live);
        let (dropped, _) = with_subagents(&live, session)?;
        for id in &dropped {
            live.remove(id);
        }
        drop(live);
        lock(&self.last_focused).retain(|_, focused| !dropped.contains(focused));
        Ok(dropped)
    }

    /// Drops the records of the agent sessions in `worktrees`, which are being removed, and
    /// returns them. The removal saves the state.
    pub(crate) fn drop_records_in(&self, worktrees: &[PathBuf]) -> Vec<SessionId> {
        let mut live = lock(&self.live);
        let dropped: Vec<SessionId> = live
            .iter()
            .filter(|(_, tracked)| worktrees.contains(&tracked.worktree))
            .map(|(&id, _)| id)
            .collect();
        for id in &dropped {
            live.remove(id);
        }
        drop(live);
        lock(&self.last_focused).retain(|worktree, _| !worktrees.contains(worktree));
        dropped
    }

    /// Removes the folders of `sessions` under the home folder, with the files each launch
    /// wrote there. One that is gone already is fine. It tries every folder, and fails with
    /// the first that could not be removed. It blocks while it removes them.
    pub(crate) fn remove_session_folders(&self, sessions: &[SessionId]) -> Result<()> {
        let mut removed = Ok(());
        for &session in sessions {
            let folder = self.session_folder(session);
            match fs::remove_dir_all(&folder) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) if removed.is_ok() => {
                    removed = Err(CoreError::io("remove the session folder", &folder)(e));
                }
                Err(_) => {}
            }
        }
        removed
    }
}

// `session`, then every session below it, each once, so a chain of parents that loops still
// ends. With them, the worktree of `session`.
fn with_subagents(
    live: &HashMap<SessionId, Tracked>,
    session: SessionId,
) -> Result<(Vec<SessionId>, PathBuf)> {
    let worktree = live
        .get(&session)
        .map(|tracked| tracked.worktree.clone())
        .ok_or(CoreError::NotAnAgentSession(session))?;
    let mut found = vec![session];
    let mut at = 0;
    while let Some(&parent) = found.get(at) {
        for (&subagent, tracked) in live {
            if tracked.parent == Some(parent) && !found.contains(&subagent) {
                found.push(subagent);
            }
        }
        at += 1;
    }
    let switching = |id: &&SessionId| live.get(*id).is_some_and(|tracked| tracked.switching);
    match found.iter().find(switching) {
        Some(&switching) => Err(CoreError::SwitchingMode(switching)),
        None => Ok((found, worktree)),
    }
}

impl Core {
    /// Every agent session of every tracked worktree, oldest first, ended ones too. Removing a
    /// worktree drops the records of its sessions, so none is listed for a worktree that is
    /// gone.
    pub fn agent_sessions(&self) -> Vec<AgentSessionInfo> {
        let live = lock(&self.agents.live);
        let mut listed: Vec<AgentSessionInfo> = live
            .iter()
            .map(|(&session, tracked)| AgentSessionInfo {
                session,
                worktree: tracked.worktree.clone(),
                agent: tracked.agent,
                mode: tracked.mode,
                // Its parent's record can be gone, as when the parent's worktree was removed.
                parent: tracked.parent.filter(|parent| live.contains_key(parent)),
                title: tracked
                    .title
                    .clone()
                    .unwrap_or_else(|| NEW_SESSION_TITLE.to_owned()),
                created: tracked.created,
                ended: tracked.ended,
                state: tracked.state.map(|(state, _)| state),
                since: tracked.entered_wall_clock,
            })
            .collect();
        listed.sort_by_key(|info| info.session);
        listed
    }

    /// Deletes agent session `session` and every subagent session below it: ends their
    /// processes, then drops their records, their agent panels and their folders under the
    /// home folder. Sends `AgentSessionsChanged`, and `WorkspaceChanged` with no `by` for each
    /// worktree that lost a panel. When a process does not exit in time it fails with
    /// `SessionsStillRunning` and drops nothing, so the call can be made again. When the state
    /// cannot be saved it fails after it dropped them all, as removing a worktree does, and
    /// the next save writes the state without them.
    pub async fn delete_agent_session(&self, session: SessionId) -> Result<()> {
        // Subscribed before any is killed, so no exit is missed.
        let exits = self.events.subscribe();
        let (to_delete, worktree) = self.agents.with_subagents(session)?;
        let killed: HashSet<SessionId> = to_delete
            .into_iter()
            // It fails only for one with no process, which has nothing to wait for.
            .filter(|&session| self.kill(session).is_ok())
            .collect();
        let running = self.sessions.is_running();
        // The caller's runtime may have no timer, so wait on the core's.
        let exited = self
            .runtime
            .spawn(wait_for_exits(exits, killed, running, EXIT_WAIT))
            .await;
        if !matches!(exited, Ok(true)) {
            return Err(CoreError::SessionsStillRunning(worktree));
        }
        let deleted = self.agents.drop_with_subagents(session)?;
        // One that started while the others ended, such as a subagent asked for just before.
        for &session in &deleted {
            let _ = self.kill(session);
        }
        let lost_a_panel = self.drop_agent_panels(&deleted);
        let saved = self.save_state().await;
        // The records are gone whether the save worked or not. Having no receivers is fine.
        let _ = self.events.send(CoreEvent::AgentSessionsChanged);
        for worktree in lost_a_panel {
            let _ = self
                .events
                .send(CoreEvent::WorkspaceChanged { worktree, by: None });
        }
        // The records are gone, so nothing else would remove their folders later.
        let agents = self.agents.clone();
        let home = self.agents.session_folder(session);
        let removed = self
            .runtime
            .spawn_blocking(move || agents.remove_session_folders(&deleted))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("remove the session folder", &home)(io::Error::other(e)));
        saved?;
        removed?
    }

    /// Starts ended agent session `session` again: a new process under the same session id,
    /// in the mode it had, that resumes its conversation as a restart does. It becomes `Idle`
    /// and shows in the agent panel it had, or in a new one after the last when the workspace
    /// no longer has that panel. Sends `AgentSessionsChanged` and `WorkspaceChanged` with no
    /// `by`. A session that still runs stays as it is.
    pub fn resume_agent_session(&self, session: SessionId) -> Result<()> {
        let (agent, mode, worktree) = {
            let mut live = lock(&self.agents.live);
            let tracked = live
                .get_mut(&session)
                .ok_or(CoreError::NotAnAgentSession(session))?;
            if tracked.switching {
                return Err(CoreError::SwitchingMode(session));
            }
            if !tracked.ended {
                return Ok(());
            }
            // A second resume or a mode switch is refused from here on, so one process starts.
            tracked.switching = true;
            // It was `Exited` or `Failed`. Set before its new process starts, so this never
            // replaces a state that process reports, or the state of its exit.
            tracked.set(session, AgentState::Idle, tracked.source(), &self.events);
            (tracked.agent, tracked.mode, tracked.worktree.clone())
        };
        // Leaves the session ended and `Exited` however this fails.
        let mut resume = Switch {
            agents: &self.agents,
            events: &self.events,
            session,
            started: false,
        };
        self.resume_in(session, mode)?;
        resume.started = true;
        drop(resume);
        self.show_agent_panel(&worktree, session, agent);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::super::AgentStart;
    use super::*;
    use crate::CoreConfig;

    #[test]
    fn a_message_titles_a_session_after_its_first_line_with_text() {
        assert_eq!(
            title_of("\n  fix the bug \nthen test"),
            Some("fix the bug".into())
        );
        assert_eq!(title_of(" \n\t\n"), None);
    }

    #[test]
    fn a_session_folder_that_cannot_be_removed_does_not_keep_the_others() {
        let home = tempfile::tempdir().unwrap();
        let (core, _events) = Core::start(CoreConfig::new(home.path().to_path_buf())).unwrap();
        let (stuck, removable) = (SessionId(1), SessionId(2));
        for session in [stuck, removable] {
            let folder = core.agents.session_folder(session);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("mcp.json"), "{}").unwrap();
        }
        // A folder that gives up no file cannot be emptied.
        let stuck_folder = core.agents.session_folder(stuck);
        fs::set_permissions(&stuck_folder, fs::Permissions::from_mode(0o555)).unwrap();

        let removed = core.agents.remove_session_folders(&[stuck, removable]);

        fs::set_permissions(&stuck_folder, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(removed, Err(CoreError::Io { .. })), "{removed:?}");
        assert!(stuck_folder.exists());
        assert!(!core.agents.session_folder(removable).exists());
    }

    #[test]
    fn a_subagent_whose_parent_is_no_longer_listed_is_listed_with_no_parent() {
        let home = tempfile::tempdir().unwrap();
        let (core, _events) = Core::start(CoreConfig::new(home.path().to_path_buf())).unwrap();
        for (id, parent) in [(1, None), (2, Some(1)), (3, Some(9))] {
            let mut tracked = Tracked::new(Agent::Claude, home.path(), Mode::Chat, (80, 24), None);
            tracked.parent = parent.map(SessionId);
            lock(&core.agents.live).insert(SessionId(id), tracked);
        }

        let parents: Vec<_> = core
            .agent_sessions()
            .into_iter()
            .map(|info| (info.session, info.parent))
            .collect();

        assert_eq!(
            parents,
            [
                (SessionId(1), None),
                (SessionId(2), Some(SessionId(1))),
                (SessionId(3), None),
            ]
        );
    }

    #[test]
    fn a_session_deleted_while_its_process_starts_does_not_keep_that_process() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let git = Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(git.success());
        // Stands in for Claude Code, and runs until it is killed.
        let claude = dir.path().join("claude");
        fs::write(&claude, "#!/bin/sh\nexec sleep 600\n").unwrap();
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = CoreConfig::new(dir.path().join("home"));
        cfg.claude_command = claude;
        let (core, _events) = Core::start(cfg).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(core.add_repo(&repo)).unwrap();
        let worktree = runtime.block_on(core.list_worktrees())[0].path.clone();
        // As when `delete_agent_session` dropped its record between the launch and the start.
        let session = SessionId(7);
        let start = AgentStart {
            agent: Agent::Claude,
            mode: Mode::Terminal,
            resume_id: "0b0e5a6c-6a57-4f0c-9d0a-2f7f1f3f8a11",
            resume: true,
            prompt: None,
            model: None,
            cols: 80,
            rows: 24,
        };

        let started = core.start_agent(session, &worktree, start);

        assert!(
            matches!(started, Err(CoreError::NotAnAgentSession(s)) if s == session),
            "{:?}",
            started.map(|_| ())
        );
        let running = core.sessions.is_running();
        let deadline = Instant::now() + Duration::from_secs(10);
        while running(session) {
            assert!(Instant::now() < deadline, "its process still runs");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
