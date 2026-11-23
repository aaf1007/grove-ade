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
        keep_panels(&mut lock(&self.live), |session| !gone.contains(&session))
    }

    /// After a restart, drops the agent panel of each session that did not come back, so a new
    /// session that gets its id never shows in it. A session that has a record keeps its panel,
    /// so one that started while the others came back keeps it too.
    pub(crate) fn drop_panels_of_sessions_not_listed(&self) {
        let mut live = lock(&self.live);
        let listed: HashSet<SessionId> = live.keys().copied().collect();
        keep_panels(&mut live, |session| listed.contains(&session));
    }
}

// Keeps only the agent panels whose session `keep` says to, in every workspace. Returns the
// sessions whose workspace lost one.
fn keep_panels(
    live: &mut HashMap<SessionId, Tracked>,
    keep: impl Fn(SessionId) -> bool,
) -> Vec<SessionId> {
    let mut lost_a_panel = Vec::new();
    for (&owner, tracked) in live.iter_mut() {
        let chat = &mut tracked.workspace.workspace.chat;
        let before = chat.len();
        chat.retain(|panel| keep(panel.session));
        if chat.len() != before {
            lost_a_panel.push(owner);
        }
    }
    lost_a_panel
}

/// Each agent panel with the session whose workspace it shows in, in the order each workspace
/// has them. Read before records are dropped, for `move_panels`.
pub(super) fn placed_panels(live: &HashMap<SessionId, Tracked>) -> Vec<(SessionId, AgentPanel)> {
    let mut placed = Vec::new();
    for (&owner, tracked) in live {
        for panel in &tracked.workspace.workspace.chat {
            if top_parent(live, panel.session) == Some(owner) {
                placed.push((owner, panel.clone()));
            }
        }
    }
    // So the panels that move arrive in the same order each time.
    placed.sort_by_key(|(owner, _)| *owner);
    placed
}

/// After records were dropped from `live`: each workspace keeps only the panels of the sessions
/// that still show in it, and a session whose top parent was dropped shows in the workspace of
/// the top parent it has now, with the panel `placed` says it had. Returns the sessions whose
/// workspace lost or got a panel.
pub(super) fn move_panels(
    live: &mut HashMap<SessionId, Tracked>,
    placed: Vec<(SessionId, AgentPanel)>,
) -> Vec<SessionId> {
    let shows_in: HashMap<SessionId, SessionId> = live
        .keys()
        .filter_map(|&session| Some((session, top_parent(live, session)?)))
        .collect();
    let mut changed = Vec::new();
    for (&owner, tracked) in live.iter_mut() {
        let chat = &mut tracked.workspace.workspace.chat;
        let before = chat.len();
        // A subagent keeps its own panel in its own workspace, for when its parents are gone.
        chat.retain(|panel| panel.session == owner || shows_in.get(&panel.session) == Some(&owner));
        if chat.len() != before {
            changed.push(owner);
        }
    }
    for (shown_in, panel) in placed {
        let Some(owner) = shows_in
            .get(&panel.session)
            .filter(|owner| **owner != shown_in)
        else {
            continue;
        };
        let Some(tracked) = live.get_mut(owner) else {
            continue;
        };
        let chat = &mut tracked.workspace.workspace.chat;
        if chat.iter().all(|other| other.session != panel.session) {
            chat.push(panel);
            if !changed.contains(owner) {
                changed.push(*owner);
            }
        }
    }
    changed
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::broadcast;

    use super::*;
    use crate::state::SaveRequests;
    use crate::{CoreConfig, Mode};

    // The risk this guards: the socket takes requests while sessions are restored, so a
    // subagent can start before the panels of sessions that did not come back are dropped.
    #[test]
    fn a_session_started_while_the_others_were_restored_keeps_its_panel() {
        let home = tempfile::tempdir().unwrap();
        let agents = Agents::new(
            &CoreConfig::new(home.path().to_path_buf()),
            SaveRequests::default(),
            Arc::new(crate::warnings::Warnings::new(broadcast::channel(1).0)),
        );
        let (restored, started_meanwhile, not_back) = (SessionId(1), SessionId(2), SessionId(3));
        let panel = |session| AgentPanel {
            agent: Agent::Claude,
            session,
            shown: true,
        };
        let new = || Tracked::new(Agent::Claude, home.path(), Mode::Chat, (80, 24), None);
        let mut parent = new();
        parent.workspace.workspace.chat =
            vec![panel(restored), panel(not_back), panel(started_meanwhile)];
        let mut subagent = new();
        subagent.parent = Some(restored);
        lock(&agents.live).insert(restored, parent);
        lock(&agents.live).insert(started_meanwhile, subagent);

        agents.drop_panels_of_sessions_not_listed();

        let chat = agents
            .with_workspace(restored, |_, _, saved| saved.workspace.chat.clone())
            .unwrap();
        assert_eq!(chat, [panel(restored), panel(started_meanwhile)]);
    }
}
