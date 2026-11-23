use std::path::Path;
use std::sync::{Mutex, PoisonError};

use tokio::sync::broadcast;

use crate::{Core, CoreEvent, SessionId, Warning};

/// An active warning. Its id names this occurrence for the life of the core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveWarning {
    pub id: usize,
    pub warning: Warning,
}

#[derive(Default)]
struct State {
    next_id: usize,
    active: Vec<ActiveWarning>,
}

pub(crate) struct Warnings {
    state: Mutex<State>,
    events: broadcast::Sender<CoreEvent>,
}

impl Core {
    /// Warnings that still apply, oldest first. Dismissing a bar affects only its view.
    pub fn warnings(&self) -> Vec<ActiveWarning> {
        self.warnings.active()
    }
}

impl Warnings {
    pub(crate) fn new(events: broadcast::Sender<CoreEvent>) -> Self {
        Self {
            state: Mutex::default(),
            events,
        }
    }

    pub(crate) fn active(&self) -> Vec<ActiveWarning> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active
            .clone()
    }

    pub(crate) fn raise(&self, warning: Warning) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .active
            .iter()
            .any(|entry| match (&entry.warning, &warning) {
                (
                    Warning::WatcherNotStarted { worktree: a, .. },
                    Warning::WatcherNotStarted { worktree: b, .. },
                ) => a == b,
                (
                    Warning::CodexHooksNotTrusted { session: a, .. },
                    Warning::CodexHooksNotTrusted { session: b, .. },
                ) => a == b,
                // Each failed restore reports a separate session, even when the reasons match.
                _ => false,
            })
        {
            return;
        }
        let id = state.next_id;
        state.next_id += 1;
        state.active.push(ActiveWarning {
            id,
            warning: warning.clone(),
        });
        let _ = self.events.send(CoreEvent::Warning(warning));
    }

    pub(crate) fn clear_watcher(&self, path: &Path) {
        self.clear(|warning| matches!(warning, Warning::WatcherNotStarted { worktree, .. } if worktree == path));
    }

    pub(crate) fn clear_hooks(&self, id: SessionId) {
        self.clear(|warning| matches!(warning, Warning::CodexHooksNotTrusted { session, .. } if *session == id));
    }

    fn clear(&self, matches: impl Fn(&Warning) -> bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = state
            .active
            .iter()
            .position(|entry| matches(&entry.warning))
        {
            let entry = state.active.remove(index);
            let _ = self.events.send(CoreEvent::WarningCleared { id: entry.id });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hooks_warning_raised_twice_for_one_session_is_listed_once() {
        let (events, mut sent) = broadcast::channel(8);
        let warnings = Warnings::new(events);
        let hooks = |session| Warning::CodexHooksNotTrusted {
            session: SessionId(session),
            worktree: "/w".into(),
        };

        warnings.raise(hooks(3));
        warnings.raise(hooks(3));
        warnings.raise(hooks(4));

        let listed: Vec<_> = warnings.active().into_iter().map(|e| e.warning).collect();
        assert_eq!(listed, [hooks(3), hooks(4)]);
        let raised = std::iter::from_fn(|| sent.try_recv().ok()).count();
        assert_eq!(raised, 2);
    }
}
