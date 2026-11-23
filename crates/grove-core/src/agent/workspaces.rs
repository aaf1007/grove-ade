//! Where each agent session's workspace is kept: in the session's record, so the two are saved
//! and dropped together. A subagent session shows in the workspace of its top parent.

use std::collections::{HashMap, HashSet};

use super::records::with_those_below;
use super::{Agent, Agents, Tracked, lock};
use crate::workspace::Saved;
use crate::{AgentPanel, CoreError, Result, SessionId};

/// An agent session whose panel a workspace may hold: the session the workspace belongs to, or
/// a subagent session below it.
pub(crate) struct Member {
    pub(crate) session: SessionId,
    pub(crate) agent: Agent,
    /// Its process ended.
    pub(crate) ended: bool,
}

impl Agents {
    /// Runs `change` on the workspace agent session `session` shows in: its own, or its top
    /// parent's when it is a subagent. `change` also gets the session that workspace belongs
    /// to, and its members. The records stay locked meanwhile, so no member starts or goes.
    pub(crate) fn with_workspace<T>(
        &self,
        session: SessionId,
        change: impl FnOnce(SessionId, &[Member], &mut Saved) -> T,
    ) -> Result<T> {
        let mut live = lock(&self.live);
        let owner = top_parent(&live, session).ok_or(CoreError::NotAnAgentSession(session))?;
        let members: Vec<Member> = with_those_below(&live, owner)
            .into_iter()
            .filter_map(|session| {
                let tracked = live.get(&session)?;
                Some(Member {
                    session,
                    agent: tracked.agent,
                    ended: tracked.ended,
                })
            })
            .collect();
        let tracked = live
            .get_mut(&owner)
            .ok_or(CoreError::NotAnAgentSession(session))?;
        Ok(change(owner, &members, &mut tracked.workspace))
    }

    /// Puts each of `panels`, the agent panels subagent sessions had in a state file from
    /// before version 10, in the workspace of its top parent, after the panels already there.
    /// A session that did not come back gets none, and neither does one whose parents are all
    /// gone: it shows in its own workspace.
    pub(crate) fn adopt_panels(&self, panels: Vec<AgentPanel>) {
        for panel in panels {
            let session = panel.session;
            // It fails only for a session that did not come back.
            let _ = self.with_workspace(session, |owner, _, saved| {
                let chat = &mut saved.workspace.chat;
                if owner != session && chat.iter().all(|shown| shown.session != session) {
                    chat.push(panel);
                }
            });
        }
    }

    /// Drops the agent panels of `gone`, sessions whose records were dropped, from every
    /// workspace. Returns the sessions whose workspace lost one.
    pub(crate) fn drop_panels_of(&self, gone: &[SessionId]) -> Vec<SessionId> {
        self.keep_panels(|session| !gone.contains(&session))
    }

    /// After a restart, drops the agent panel of each session that did not come back, so a new
    /// session that gets its id never shows in it.
    pub(crate) fn keep_panels_of(&self, back: &HashSet<SessionId>) {
        self.keep_panels(|session| back.contains(&session));
    }

    // Keeps only the agent panels whose session `keep` says to, in every workspace. Returns the
    // sessions whose workspace lost one.
    fn keep_panels(&self, keep: impl Fn(SessionId) -> bool) -> Vec<SessionId> {
        let mut lost_a_panel = Vec::new();
        for (&owner, tracked) in lock(&self.live).iter_mut() {
            let chat = &mut tracked.workspace.workspace.chat;
            let before = chat.len();
            chat.retain(|panel| keep(panel.session));
            if chat.len() != before {
                lost_a_panel.push(owner);
            }
        }
        lost_a_panel
    }
}

// The session whose workspace `session` shows in: the last one up its chain of parents that is
// still listed. Each session is visited once, so a chain that loops still ends.
fn top_parent(live: &HashMap<SessionId, Tracked>, session: SessionId) -> Option<SessionId> {
    let mut seen = vec![session];
    let mut top = session;
    let mut above = live.get(&session)?.parent;
    while let Some(parent) = above.filter(|parent| !seen.contains(parent)) {
        let Some(tracked) = live.get(&parent) else {
            break;
        };
        seen.push(parent);
        top = parent;
        above = tracked.parent;
    }
    Some(top)
}
