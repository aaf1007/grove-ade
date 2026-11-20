//! Workspaces: each worktree's agent panels, panel-area tabs and bottom terminal. A worktree
//! starts from its repo's profile layout, and each change is saved in the state file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::panel_request::Refused;
use crate::remove::repo_of;
use crate::{
    Agent, Core, CoreError, CoreEvent, Layout, PanelKind, Result, SessionId, WorktreeInfo,
};

/// One worktree's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The agent panels in the chat area, in order.
    pub chat: Vec<AgentPanel>,
    /// The panel area's tabs, in order.
    pub panels: Vec<PanelKind>,
    /// One of `panels`. None shows no tab.
    pub selected: Option<PanelKind>,
    /// Whether the terminal panel is open at the bottom of the panel area.
    pub terminal: bool,
}

/// An agent panel in the chat area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPanel {
    pub agent: Agent,
    /// None until the UI starts its agent session.
    pub session: Option<SessionId>,
    /// A hidden panel's session keeps running.
    pub shown: bool,
}

/// A workspace as agent sessions read and write it through grove-mcp: only grove's own panels,
/// and no field grove does not know.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceJson {
    chat: Vec<AgentPanelJson>,
    panels: Vec<PanelKind>,
    selected: Option<PanelKind>,
    terminal: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentPanelJson {
    agent: Agent,
    session: Option<SessionId>,
    shown: bool,
}

impl From<Workspace> for WorkspaceJson {
    fn from(workspace: Workspace) -> Self {
        WorkspaceJson {
            chat: workspace
                .chat
                .into_iter()
                .map(|panel| AgentPanelJson {
                    agent: panel.agent,
                    session: panel.session,
                    shown: panel.shown,
                })
                .collect(),
            panels: workspace.panels,
            selected: workspace.selected,
            terminal: workspace.terminal,
        }
    }
}

impl From<WorkspaceJson> for Workspace {
    fn from(workspace: WorkspaceJson) -> Self {
        Workspace {
            chat: workspace
                .chat
                .into_iter()
                .map(|panel| AgentPanel {
                    agent: panel.agent,
                    session: panel.session,
                    shown: panel.shown,
                })
                .collect(),
            panels: workspace.panels,
            selected: workspace.selected,
            terminal: workspace.terminal,
        }
    }
}

/// The saved workspaces, by worktree. A worktree with none uses its profile layout.
pub(crate) type Workspaces = HashMap<PathBuf, Saved>;

pub(crate) struct Saved {
    pub(crate) workspace: Workspace,
    // The workspace from before the last change an agent made, until it is undone. Kept in
    // memory only.
    before_agent_change: Option<Workspace>,
}

impl Saved {
    pub(crate) fn restored(workspace: Workspace) -> Saved {
        Saved {
            workspace,
            before_agent_change: None,
        }
    }
}

impl Workspace {
    // `layout` with what it leaves unset from grove's own: one Claude panel, the Files and Diff
    // tabs and the terminal open. The first tab is selected.
    fn from_layout(layout: Layout) -> Workspace {
        let chat = layout.chat.unwrap_or_else(|| vec![Agent::Claude]);
        let panels = layout
            .panels
            .unwrap_or_else(|| vec![PanelKind::Files, PanelKind::Diff]);
        Workspace {
            chat: chat
                .into_iter()
                .map(|agent| AgentPanel {
                    agent,
                    session: None,
                    shown: true,
                })
                .collect(),
            selected: panels.first().copied(),
            panels,
            terminal: layout.terminal.unwrap_or(true),
        }
    }
}

impl Core {
    /// The workspace of `worktree`: the saved one, or its repo's profile layout with the repo's
    /// overrides. A panel whose session no longer runs in `worktree` is left out.
    pub fn workspace(&self, worktree: &Path) -> Result<Workspace> {
        Ok(self.listed_workspace(worktree)?.0)
    }

    // `workspace`, and the sessions whose panels it left out.
    fn listed_workspace(&self, worktree: &Path) -> Result<(Workspace, Vec<SessionId>)> {
        let layout = self.layout_of(worktree)?;
        let mut workspace = match self.workspaces().get(worktree) {
            Some(saved) => saved.workspace.clone(),
            None => layout,
        };
        // Read after the workspace, so a session placed in between is not taken for ended.
        let running = self.running_in(worktree);
        let ended: Vec<SessionId> = workspace
            .chat
            .iter()
            .filter_map(|panel| panel.session)
            .filter(|session| !running.contains(session))
            .collect();
        workspace.chat.retain(|panel| is_listed(panel, &ended));
        Ok((workspace, ended))
    }

    /// Refuses an agent panel past the last one of `worktree`, as `workspace` returns them.
    /// Returns the sessions whose panels it left out, for `place_agent` to count by the same.
    pub(crate) fn check_agent_panel(
        &self,
        worktree: &Path,
        panel: Option<usize>,
    ) -> Result<Vec<SessionId>> {
        let (workspace, ended) = self.listed_workspace(worktree)?;
        if panel.is_some_and(|at| at >= workspace.chat.len()) {
            return Err(CoreError::InvalidWorkspace {
                reason: "its agent panel is past the last one",
            });
        }
        Ok(ended)
    }

    /// Puts the new agent session `session` in agent panel `panel` of `worktree`, counted as
    /// `workspace` returned them when `check_agent_panel` left out the panels of `ended`. So the
    /// index means what it meant then: a session that ended since still counts, and so does a
    /// panel another start filled since. With no `panel`, or one that is gone or has a session,
    /// it goes in a new shown panel after the last one. The other panels stay as they are, those
    /// of ended sessions too. Asks for a save and sends one `WorkspaceChanged` with no `by`.
    pub(crate) fn place_agent(
        &self,
        worktree: &Path,
        session: SessionId,
        agent: Agent,
        panel: Option<usize>,
        ended: &[SessionId],
    ) {
        // Fails only when the worktree was forgotten meanwhile, which ends its sessions.
        let Ok(layout) = self.layout_of(worktree) else {
            return;
        };
        {
            // Held across the change, as in `set_workspace`.
            let tracked = self.tracked();
            if check_tracked(&tracked, worktree).is_err() {
                return;
            }
            let mut workspaces = self.workspaces();
            let saved = workspaces
                .entry(worktree.to_path_buf())
                .or_insert_with(|| Saved::restored(layout));
            let chat = &mut saved.workspace.chat;
            let placed = AgentPanel {
                agent,
                session: Some(session),
                shown: true,
            };
            let mut listed = chat.iter_mut().filter(|p| is_listed(p, ended));
            match panel.and_then(|at| listed.nth(at)) {
                Some(empty) if empty.session.is_none() => *empty = placed,
                _ => chat.push(placed),
            }
        }
        self.saves.request();
        // Having no receivers is fine.
        let _ = self.events.send(CoreEvent::WorkspaceChanged {
            worktree: worktree.to_path_buf(),
            by: None,
        });
    }

    /// Saves `workspace` for `worktree` and sends one `WorkspaceChanged` with `by`. `by` is the
    /// agent session that changed it, None for the user. Undo restores the workspace from before
    /// the last change an agent made.
    pub async fn set_workspace(
        &self,
        worktree: &Path,
        workspace: Workspace,
        by: Option<SessionId>,
    ) -> Result<()> {
        if workspace
            .selected
            .is_some_and(|tab| !workspace.panels.contains(&tab))
        {
            return Err(CoreError::InvalidWorkspace {
                reason: "its selected tab is not one of its tabs",
            });
        }
        let layout = self.layout_of(worktree)?;
        {
            // Held across the insert, so a worktree forgotten meanwhile leaves no workspace.
            let tracked = self.tracked();
            check_tracked(&tracked, worktree)?;
            let mut workspaces = self.workspaces();
            let (before, kept) = match workspaces.remove(worktree) {
                Some(saved) => (saved.workspace, saved.before_agent_change),
                None => (layout, None),
            };
            let saved = Saved {
                workspace,
                // Only the last agent change can be undone.
                before_agent_change: if by.is_some() { Some(before) } else { kept },
            };
            workspaces.insert(worktree.to_path_buf(), saved);
        }
        self.changed(worktree, by).await
    }

    /// Restores the workspace of `worktree` from before the last change an agent made, even if
    /// the user changed it since, and sends `WorkspaceChanged` with no `by`. It can be undone
    /// once. A running session the restored workspace has no panel for, such as one placed since,
    /// keeps one: the panel at its place if that one is empty, or else a new one after the last.
    pub async fn undo_workspace(&self, worktree: &Path) -> Result<()> {
        {
            let tracked = self.tracked();
            check_tracked(&tracked, worktree)?;
            let mut workspaces = self.workspaces();
            let nothing = || CoreError::NothingToUndo(worktree.to_path_buf());
            let saved = workspaces.get_mut(worktree).ok_or_else(nothing)?;
            let mut restored = saved.before_agent_change.take().ok_or_else(nothing)?;
            // Only leaf locks, so taking them here cannot deadlock.
            let running = self.running_in(worktree);
            for (at, panel) in saved.workspace.chat.iter().enumerate() {
                let Some(session) = panel.session.filter(|s| running.contains(s)) else {
                    continue;
                };
                if restored.chat.iter().any(|p| p.session == Some(session)) {
                    continue;
                }
                let kept = AgentPanel {
                    agent: panel.agent,
                    session: Some(session),
                    shown: true,
                };
                match restored.chat.get_mut(at) {
                    Some(empty) if empty.session.is_none() => *empty = kept,
                    _ => restored.chat.push(kept),
                }
            }
            saved.workspace = restored;
        }
        self.changed(worktree, None).await
    }

    async fn changed(&self, worktree: &Path, by: Option<SessionId>) -> Result<()> {
        self.save_state().await?;
        let _ = self.events.send(CoreEvent::WorkspaceChanged {
            worktree: worktree.to_path_buf(),
            by,
        });
        Ok(())
    }

    /// The workspace of agent session `caller`'s worktree, as grove-mcp gives it.
    pub(crate) fn agent_workspace(&self, caller: SessionId) -> Result<Value> {
        let worktree = self.caller_worktree(caller)?;
        Ok(json!(WorkspaceJson::from(self.workspace(&worktree)?)))
    }

    /// Replaces the workspace of agent session `caller`'s worktree with `workspace`, as grove-mcp
    /// sends it, with `caller` as `by`. Refuses one that drops the panel of an agent session
    /// running there, or names a session that does not run there.
    pub(crate) fn set_agent_workspace(
        &self,
        caller: SessionId,
        workspace: Value,
    ) -> std::result::Result<(), Refused> {
        let worktree = self.caller_worktree(caller)?;
        let workspace: WorkspaceJson =
            serde_json::from_value(workspace).map_err(|e| Refused::BadWorkspace(e.to_string()))?;
        let workspace = Workspace::from(workspace);
        check_sessions(&self.workspace(&worktree)?, &workspace).map_err(Refused::BadWorkspace)?;
        // The panel request thread is no runtime worker, so it may block on one.
        self.runtime
            .block_on(self.set_workspace(&worktree, workspace, Some(caller)))?;
        Ok(())
    }

    // The workspace `worktree` starts with: its repo's profile layout with the repo's overrides.
    fn layout_of(&self, worktree: &Path) -> Result<Workspace> {
        let repo = repo_of(&self.tracked(), worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?;
        Ok(Workspace::from_layout(self.settings_of(&repo).layout))
    }

    // The sessions whose panels `workspace` lists: those with a process in `worktree`, and its
    // agent sessions not ended, as `sessions` lists them. An agent session switching mode has
    // no process for a moment and still counts.
    fn running_in(&self, worktree: &Path) -> Vec<SessionId> {
        let mut running = self.sessions.in_worktrees(&[worktree.to_path_buf()]);
        running.extend(
            self.agents
                .running_in(worktree)
                .into_iter()
                .map(|(id, _)| id),
        );
        running
    }

    pub(crate) fn workspaces(&self) -> MutexGuard<'_, Workspaces> {
        self.workspaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

// Whether `workspace` returns the panel: its session, if any, is not in `ended`.
fn is_listed(panel: &AgentPanel, ended: &[SessionId]) -> bool {
    panel.session.is_none_or(|s| !ended.contains(&s))
}

// Each agent session `now` lists keeps its panel in `new`, once and with its own agent, and `new`
// names no other session. Panels with no session come and go freely.
fn check_sessions(now: &Workspace, new: &Workspace) -> std::result::Result<(), String> {
    let mut kept = Vec::new();
    for panel in &new.chat {
        let Some(session) = panel.session else {
            continue;
        };
        if kept.contains(&session) {
            return Err(format!("it names agent session {session} twice"));
        }
        kept.push(session);
        match now.chat.iter().find(|p| p.session == Some(session)) {
            None => {
                return Err(format!(
                    "agent session {session} is not running in this worktree"
                ));
            }
            Some(running) if running.agent != panel.agent => {
                return Err(format!(
                    "agent session {session} runs {}, not {}",
                    running.agent.name(),
                    panel.agent.name()
                ));
            }
            Some(_) => {}
        }
    }
    let dropped = now
        .chat
        .iter()
        .filter_map(|p| p.session)
        .find(|s| !kept.contains(s));
    match dropped {
        Some(session) => Err(format!(
            "it drops the panel of agent session {session}, which is running"
        )),
        None => Ok(()),
    }
}

fn check_tracked(tracked: &[WorktreeInfo], worktree: &Path) -> Result<()> {
    if tracked.iter().any(|w| w.path == worktree) {
        Ok(())
    } else {
        Err(CoreError::UnknownWorktree(worktree.to_path_buf()))
    }
}
