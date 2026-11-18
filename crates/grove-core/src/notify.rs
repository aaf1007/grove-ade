//! Which agent state changes should notify the user: an entry into a state the session's repo
//! lists under `notify` in its profile settings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast::{self, error::RecvError};

use crate::agent::Agents;
use crate::profile::Profiles;
use crate::remove::repo_of;
use crate::restore::lock;
use crate::run::{self, resolved};
use crate::{AgentState, AgentStateKind, CoreEvent, SessionId, WorktreeInfo};

// The list of a repo whose settings set none.
const DEFAULT_STATES: [AgentStateKind; 2] = [AgentStateKind::NeedsInput, AgentStateKind::Failed];

/// What `follow` reads to find a session's repo settings.
pub(crate) struct Sources {
    pub(crate) agents: Arc<Agents>,
    pub(crate) worktrees: Arc<Mutex<Vec<WorktreeInfo>>>,
    pub(crate) repo_settings: Arc<Mutex<run::Settings>>,
    pub(crate) profiles: Arc<Profiles>,
}

/// Sends `Notify` for each agent state change that should notify. It only takes short locks,
/// so a burst of changes never holds up a tokio worker.
pub(crate) async fn follow(
    sources: Sources,
    events: broadcast::Sender<CoreEvent>,
    mut received: broadcast::Receiver<CoreEvent>,
) {
    let mut entries = Entries::default();
    loop {
        let event = received.recv().await;
        if matches!(event, Err(RecvError::Closed)) {
            return;
        }
        let Some((session, state)) = entries.entry(event) else {
            continue;
        };
        if let Some(worktree) = sources.notifying_worktree(session, state) {
            let _ = events.send(CoreEvent::Notify {
                session,
                worktree,
                state,
            });
        }
    }
}

impl Sources {
    // The worktree of `session` when its repo lists `state`. None when it does not, or when the
    // session or its worktree is gone.
    fn notifying_worktree(&self, session: SessionId, state: AgentState) -> Option<PathBuf> {
        let worktree = self.agents.worktree_of(session)?;
        let repo = repo_of(&lock(&self.worktrees), &worktree)?;
        let saved = lock(&self.repo_settings).get(&repo).cloned();
        let listed = resolved(saved, &self.profiles).notify.states;
        notifies(listed.as_deref(), state).then_some(worktree)
    }
}

// Whether a repo whose notify list is `listed` notifies on `state`.
fn notifies(listed: Option<&[AgentStateKind]>, state: AgentState) -> bool {
    listed.unwrap_or(&DEFAULT_STATES).contains(&kind_of(state))
}

fn kind_of(state: AgentState) -> AgentStateKind {
    match state {
        AgentState::Working => AgentStateKind::Working,
        AgentState::NeedsInput { .. } => AgentStateKind::NeedsInput,
        AgentState::Failed => AgentStateKind::Failed,
        AgentState::Done => AgentStateKind::Done,
        AgentState::Idle => AgentStateKind::Idle,
        AgentState::Exited => AgentStateKind::Exited,
    }
}

// Each agent session's last state. The core also sends a state again when only its source
// changes, and that is no new entry.
#[derive(Default)]
struct Entries(HashMap<SessionId, AgentState>);

impl Entries {
    // The session and state when `event` is an entry into a state.
    fn entry(&mut self, event: Result<CoreEvent, RecvError>) -> Option<(SessionId, AgentState)> {
        match event {
            Ok(CoreEvent::AgentState { session, state, .. }) => {
                self.is_entry(session, state).then_some((session, state))
            }
            // Missed changes make the last states stale. Forgetting them may repeat a
            // notification, but never drops a new one. A missed change itself is not notified late.
            Err(RecvError::Lagged(_)) => {
                self.0.clear();
                None
            }
            _ => None,
        }
    }

    fn is_entry(&mut self, session: SessionId, state: AgentState) -> bool {
        // An exited session changes no more, so it is dropped.
        let last = if state == AgentState::Exited {
            self.0.remove(&session)
        } else {
            self.0.insert(session, state)
        };
        last != Some(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InputKind;

    const APPROVAL: AgentState = AgentState::NeedsInput {
        kind: InputKind::Approval,
    };

    #[test]
    fn with_no_list_only_needs_input_and_failed_notify() {
        assert!(notifies(None, APPROVAL));
        assert!(notifies(None, AgentState::Failed));
        for state in [
            AgentState::Working,
            AgentState::Done,
            AgentState::Idle,
            AgentState::Exited,
        ] {
            assert!(!notifies(None, state), "{state:?}");
        }
    }

    #[test]
    fn a_list_replaces_the_default() {
        let listed = [AgentStateKind::Done];
        assert!(notifies(Some(&listed), AgentState::Done));
        assert!(!notifies(Some(&listed), AgentState::Failed));
        assert!(!notifies(Some(&[]), APPROVAL));
    }

    #[test]
    fn a_state_counts_as_an_entry_once_until_it_changes() {
        let mut entries = Entries::default();
        let (one, two) = (SessionId(1), SessionId(2));
        assert!(entries.is_entry(one, APPROVAL));
        assert!(!entries.is_entry(one, APPROVAL));
        assert!(entries.is_entry(two, APPROVAL));
        let question = AgentState::NeedsInput {
            kind: InputKind::Question,
        };
        assert!(entries.is_entry(one, question));
        assert!(entries.is_entry(one, AgentState::Working));
        assert!(entries.is_entry(one, APPROVAL));
    }

    #[test]
    fn an_exited_session_is_forgotten() {
        let mut entries = Entries::default();
        assert!(entries.is_entry(SessionId(1), AgentState::Working));
        assert!(entries.is_entry(SessionId(1), AgentState::Exited));
        assert!(entries.0.is_empty());
    }

    #[test]
    fn after_a_lag_the_same_state_counts_as_a_new_entry() {
        let changed = |state| {
            Ok(CoreEvent::AgentState {
                session: SessionId(1),
                state,
                source: crate::StateSource::Hook,
            })
        };
        let mut entries = Entries::default();
        assert_eq!(
            entries.entry(changed(APPROVAL)),
            Some((SessionId(1), APPROVAL))
        );
        // The Working between the two approvals was lost.
        assert_eq!(entries.entry(Err(RecvError::Lagged(1))), None);
        assert_eq!(
            entries.entry(changed(APPROVAL)),
            Some((SessionId(1), APPROVAL))
        );
    }
}
