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
        self.warnings
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active
            .clone()
    }
}

impl Warnings {
    pub(crate) fn new(events: broadcast::Sender<CoreEvent>) -> Self {
        Self {
            state: Mutex::default(),
            events,
        }
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
