//! Usage readings: each agent's plan limits, as its chat-mode transport reads them through the
//! agent's own interface. grove never calls a provider itself and never sees a credential.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::sync::broadcast;

use crate::{Agent, CoreEvent};

/// One window of an agent's plan limits, such as the 5-hour one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UsageWindow {
    /// The name the UI shows, such as "5-hour" or "Weekly".
    pub name: String,
    /// How much of the window is used, 0 to 100.
    pub percent_used: u8,
    /// When the window starts over. None when the agent does not say.
    pub resets_at_unix_ms: Option<u64>,
}

/// What an agent's account is limited by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanLimits {
    /// The account has none, as with an API key.
    NoPlanLimits,
    /// The plan's limit windows, in the agent's order.
    Windows(Vec<UsageWindow>),
}

/// An agent's plan limits as of its last good read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageReading {
    pub limits: PlanLimits,
    /// When the agent gave `limits`.
    pub read_at_unix_ms: u64,
    /// A later read failed, so `limits` may be out of date.
    pub latest_read_failed: bool,
}

/// What one read of an agent's plan limits gave: a `usage` line of the sidecar, as PROTOCOL.md
/// lists it, or what codex.rs reads from the app server.
#[derive(Debug, Deserialize)]
#[serde(tag = "reading", rename_all = "snake_case")]
pub(crate) enum UsageRead {
    Windows {
        windows: Vec<UsageWindow>,
    },
    NoPlanLimits,
    /// The read failed, or it held no window.
    Failed,
    /// The agent has no way to read its plan limits, or not the one grove knows.
    Unsupported,
}

/// Each agent's latest usage reading.
#[derive(Default)]
pub(crate) struct UsageReadings(Mutex<HashMap<Agent, UsageReading>>);

impl UsageReadings {
    pub(crate) fn of(&self, agent: Agent) -> Option<UsageReading> {
        self.lock().get(&agent).cloned()
    }

    /// Keeps what `read` gave as the agent's reading, or drops the reading when the agent
    /// cannot give one, and tells the UI.
    pub(crate) fn apply(
        &self,
        agent: Agent,
        read: UsageRead,
        events: &broadcast::Sender<CoreEvent>,
    ) {
        let mut readings = self.lock();
        let reading = match read {
            UsageRead::Windows { windows } => Some(fresh(PlanLimits::Windows(windows))),
            UsageRead::NoPlanLimits => Some(fresh(PlanLimits::NoPlanLimits)),
            // With no good reading before it, a failed read leaves nothing to show.
            UsageRead::Failed => match readings.get(&agent) {
                Some(last_good) => Some(UsageReading {
                    latest_read_failed: true,
                    ..last_good.clone()
                }),
                None => return,
            },
            UsageRead::Unsupported => None,
        };
        match &reading {
            Some(reading) => readings.insert(agent, reading.clone()),
            None => readings.remove(&agent),
        };
        // Sent under the lock, so readings reach the UI in the order they were kept. Having no
        // receivers is fine.
        let _ = events.send(CoreEvent::UsageReading { agent, reading });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Agent, UsageReading>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A reading the agent just gave.
fn fresh(limits: PlanLimits) -> UsageReading {
    UsageReading {
        limits,
        read_at_unix_ms: now_unix_ms(),
        latest_read_failed: false,
    }
}

/// The time now, in milliseconds since the Unix epoch. 0 only on a clock set before it.
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| u64::try_from(since_epoch.as_millis()).ok())
        .unwrap_or(0)
}
