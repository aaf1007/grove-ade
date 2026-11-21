//! What comes back when grove starts again. Agent sessions resume their conversation in the mode
//! they were in, shells start fresh in the same worktree, and run sessions come back stopped.
//! An agent session that had ended comes back ended. Each keeps its session id.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::agent::Agents;
use crate::chat::ModelChoice;
use crate::error::with_causes;
use crate::session::{Kind, Probe};
use crate::state::SavedWorkspace;
use crate::{Agent, Core, CoreEvent, Mode, Result, SessionId, Warning};

/// A session in a worktree, as `Core::sessions` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session: SessionId,
    pub kind: SessionKind,
    /// False only for a stopped run session: one that exited, whose output stays attachable, or
    /// one that was running when grove quit, which comes back stopped.
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionKind {
    Agent {
        agent: Agent,
        mode: Mode,
        /// The agent session that spawned it as a subagent. None for one the user started.
        parent: Option<SessionId>,
    },
    Shell,
    /// One of the repo's run sessions, by name.
    Run {
        name: String,
    },
}

/// A session as the state file keeps it. A restart brings back every one of them, and starts a
/// process for an agent session only if it had not ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Relaunch {
    Agent(AgentRecord),
    Shell { id: SessionId },
    Run { id: SessionId, name: String },
}

/// An agent session as the state file keeps it, from its start until the user deletes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentRecord {
    pub(crate) id: SessionId,
    pub(crate) agent: Agent,
    pub(crate) mode: Mode,
    /// None starts a fresh conversation.
    pub(crate) resume_id: Option<String>,
    /// None in a state file from before version 7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<ModelChoice>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<SessionId>,
    /// None until its first user message, and in a state file from before version 9.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    /// None in a state file from before version 9. Such a session counts as created when the
    /// file is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) created_unix_ms: Option<u64>,
    /// Its process had ended, so a restart starts none for it. A state file from before
    /// version 9 kept no ended session.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) ended: bool,
    /// Its workspace. None in a state file from before version 10, for a session whose
    /// worktree had no saved workspace. Such a session starts from the profile layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) workspace: Option<SavedWorkspace>,
}

fn is_false(flag: &bool) -> bool {
    !flag
}

impl Relaunch {
    fn id(&self) -> SessionId {
        match self {
            Relaunch::Agent(AgentRecord { id, .. })
            | Relaunch::Shell { id }
            | Relaunch::Run { id, .. } => *id,
        }
    }
}

/// The stopped run sessions, by session id.
pub(crate) type StoppedRuns = HashMap<SessionId, StoppedRun>;

pub(crate) struct StoppedRun {
    pub(crate) worktree: PathBuf,
    pub(crate) name: String,
    /// True for one a restart brought back, which the next restart brings back too. False for
    /// one that exited while grove ran, which is listed with its output kept while grove runs.
    pub(crate) comes_back_after_restart: bool,
}

/// Every session the state file keeps now, by worktree, oldest first.
pub(crate) fn to_relaunch(
    agents: &Agents,
    sessions: &Probe,
    stopped: &Mutex<StoppedRuns>,
) -> HashMap<PathBuf, Vec<Relaunch>> {
    let mut all: Vec<(PathBuf, Relaunch)> = agents
        .records()
        .into_iter()
        .map(|(worktree, record)| (worktree, Relaunch::Agent(record)))
        .collect();
    for (id, worktree, kind) in sessions.shells_and_runs() {
        let relaunch = match kind {
            Kind::Shell => Relaunch::Shell { id },
            Kind::Run { name, .. } => Relaunch::Run { id, name },
            Kind::Command | Kind::Agent => continue,
        };
        all.push((worktree, relaunch));
    }
    for (&id, run) in lock(stopped).iter() {
        if run.comes_back_after_restart {
            let name = run.name.clone();
            all.push((run.worktree.clone(), Relaunch::Run { id, name }));
        }
    }
    let mut by_worktree: HashMap<PathBuf, Vec<Relaunch>> = HashMap::new();
    for (worktree, relaunch) in all {
        by_worktree.entry(worktree).or_default().push(relaunch);
    }
    for sessions in by_worktree.values_mut() {
        sessions.sort_by_key(Relaunch::id);
    }
    by_worktree
}

impl Core {
    /// The agent, shell and run sessions in `worktree`, oldest first: those running, and the
    /// stopped run sessions, those that exited and those that were running when grove quit.
    pub fn sessions(&self, worktree: &Path) -> Result<Vec<SessionInfo>> {
        self.check_tracked(worktree)?;
        let agents = self.agents.running_in(worktree).into_iter();
        let mut sessions: Vec<SessionInfo> = agents
            .map(|(session, kind)| SessionInfo {
                session,
                kind,
                running: true,
            })
            .collect();
        for (session, w, kind) in self.sessions.probe().shells_and_runs() {
            let kind = match kind {
                Kind::Shell => SessionKind::Shell,
                Kind::Run { name, .. } => SessionKind::Run { name },
                Kind::Command | Kind::Agent => continue,
            };
            if w == worktree {
                sessions.push(SessionInfo {
                    session,
                    kind,
                    running: true,
                });
            }
        }
        for (&session, run) in self.stopped_runs().iter() {
            if run.worktree == worktree {
                sessions.push(SessionInfo {
                    session,
                    kind: SessionKind::Run {
                        name: run.name.clone(),
                    },
                    running: false,
                });
            }
        }
        sessions.sort_by_key(|info| info.session);
        Ok(sessions)
    }

    /// Brings back the sessions the state file names. An agent session that had ended comes back
    /// ended, with no process. Never fails: one that cannot start is left out with a
    /// `SessionNotRestored` warning. Returns the ids that came back.
    pub(crate) fn restore(&self, saved: HashMap<PathBuf, Vec<Relaunch>>) -> HashSet<SessionId> {
        let mut back = HashSet::new();
        // Every saved id is reserved before any session starts, an ended session's too, so
        // `new_id` never hands one out.
        for relaunch in saved.values().flatten() {
            self.sessions.reserve(relaunch.id());
        }
        for (worktree, relaunches) in saved {
            for relaunch in relaunches {
                let id = relaunch.id();
                let started = match relaunch {
                    Relaunch::Agent(record) => self.restore_agent(&worktree, record),
                    // The panel resizes it once it has measured itself.
                    Relaunch::Shell { .. } => self.start_shell(id, &worktree, 80, 24).map(drop),
                    Relaunch::Run { name, .. } => {
                        let run = StoppedRun {
                            worktree: worktree.clone(),
                            name,
                            comes_back_after_restart: true,
                        };
                        self.stopped_runs().insert(id, run);
                        Ok(())
                    }
                };
                match started {
                    Ok(()) => {
                        back.insert(id);
                    }
                    Err(e) => {
                        let warning = Warning::SessionNotRestored {
                            worktree: worktree.clone(),
                            reason: with_causes(&e),
                        };
                        // Having no receivers is fine.
                        let _ = self.events.send(CoreEvent::Warning(warning));
                    }
                }
            }
        }
        back
    }

    pub(crate) fn stopped_runs(&self) -> MutexGuard<'_, StoppedRuns> {
        lock(&self.stopped_runs)
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
