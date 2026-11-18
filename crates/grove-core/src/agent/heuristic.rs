//! Status heuristics for when hooks are silent or not running. Hook beats Heuristic: a
//! heuristic acts only on what happened after the last hook event or state change.
//!
//! Until a hook runs, a fallback applies the same two rules to every state: output that
//! follows a write gives `Working`, and the screen decides once output goes quiet.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::time::{self, MissedTickBehavior};

use super::{AgentState, Agents, InputKind, Screen, StateSource, Tracked, lock};
use crate::session::{Activity, Observed, Probe};
use crate::{CoreConfig, CoreEvent, SessionId};

/// The heuristics' timers, from `CoreConfig`.
pub(crate) struct Timers {
    /// Output this soon after a write to a `NeedsInput` session makes it `Working`.
    answer: Duration,
    /// A session this quiet gets a screen check: a `Working` one, or any before hooks run.
    stuck: Duration,
    /// How often the heuristics look at each session.
    tick: Duration,
}

impl Timers {
    pub(crate) fn new(cfg: &CoreConfig) -> Self {
        Self {
            answer: cfg.needs_input_to_working,
            stuck: cfg.stuck_working_after,
            // Often enough to act well within either timer, and at most 10 times a second.
            tick: (cfg.needs_input_to_working.min(cfg.stuck_working_after) / 10)
                .clamp(Duration::from_millis(1), Duration::from_millis(100)),
        }
    }
}

/// Looks at every running agent session once a tick, for as long as the core runs.
pub(crate) async fn run(agents: Arc<Agents>, probe: Probe, events: broadcast::Sender<CoreEvent>) {
    let mut ticks = time::interval(agents.timers.tick);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let running: Vec<SessionId> = lock(&agents.live)
            .iter()
            .filter(|(_, tracked)| !tracked.ended)
            .map(|(&session, _)| session)
            .collect();
        for session in running {
            // Read before taking the agents' lock, so the two locks never nest.
            let Some(Observed { activity, in_front }) = probe.observe(session) else {
                continue;
            };
            let (check, since) = {
                let mut live = lock(&agents.live);
                let Some(tracked) = live.get_mut(&session).filter(|t| !t.ended) else {
                    continue;
                };
                if tracked.answered(&activity, &agents.timers) {
                    tracked.set(
                        session,
                        AgentState::Working,
                        StateSource::Heuristic,
                        &events,
                    );
                    continue;
                }
                // While another program holds the terminal, the screen is not the agent's.
                if !in_front {
                    continue;
                }
                let Some(check) = tracked.due(&activity, Instant::now(), &agents.timers) else {
                    continue;
                };
                (check, tracked.since)
            };
            // Output since the check was due may have started a new turn.
            let Some((screen, _)) = probe
                .screen(session)
                .filter(|(_, output)| *output == activity.output)
            else {
                continue;
            };
            let mut live = lock(&agents.live);
            // A hook or a state change while the screen was read wins.
            let Some(tracked) = live
                .get_mut(&session)
                .filter(|t| !t.ended && t.since == since)
            else {
                continue;
            };
            let state = match (check, agents.adapter(tracked.agent).read_screen(&screen)) {
                (_, Screen::Dialog) => AgentState::NeedsInput {
                    kind: InputKind::Approval,
                },
                (Check::Quiet, Screen::Prompt { error: true }) => AgentState::Failed,
                (Check::Quiet, Screen::Prompt { error: false }) => AgentState::Idle,
                _ => continue,
            };
            tracked.set(session, state, StateSource::Heuristic, &events);
        }
    }
}

/// A screen check a session is due.
#[derive(Clone, Copy)]
enum Check {
    /// A `Working` session, or any session before hooks run, had no hook event and no
    /// output for the stuck time.
    Quiet,
    /// Output stopped for a tick before any hook ran. Only a dialog counts, since a prompt
    /// also shows while the agent works.
    Settled,
}

impl Tracked {
    /// The screen check this session is due, at most one per quiet spell of each kind.
    fn due(&mut self, activity: &Activity, now: Instant, timers: &Timers) -> Option<Check> {
        let quiet = |from: Instant| now.saturating_duration_since(from);
        let quiet_from = activity.output.map_or(self.since, |o| o.max(self.since));
        let working = self.state.map(|(state, _)| state) == Some(AgentState::Working);
        if (working || !self.hooks_seen)
            && quiet(quiet_from) >= timers.stuck
            && self.checked != Some(quiet_from)
        {
            self.checked = Some(quiet_from);
            return Some(Check::Quiet);
        }
        let output = activity.output?;
        if !self.hooks_seen && quiet(output) >= timers.tick && self.settled != Some(output) {
            self.settled = Some(output);
            return Some(Check::Settled);
        }
        None
    }

    /// The user wrote to a `NeedsInput` session, or to any session that is not `Working`
    /// before hooks run, and output followed in time.
    fn answered(&self, activity: &Activity, timers: &Timers) -> bool {
        let applies = match self.state.map(|(state, _)| state) {
            Some(AgentState::NeedsInput { .. }) => true,
            Some(AgentState::Working) => false,
            _ => !self.hooks_seen,
        };
        if !applies {
            return false;
        }
        match (activity.input, activity.reply) {
            (Some(input), Some(reply)) => {
                input > self.since && reply.saturating_duration_since(input) <= timers.answer
            }
            _ => false,
        }
    }
}
