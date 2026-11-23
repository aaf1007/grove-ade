//! Workspaces: each agent session's agent panels, panel-area tabs and whether its terminal is
//! open. A session's workspace starts from its project's profile layout and is kept with the
//! session, so it is saved with it and goes when the session is deleted. A subagent session
//! shows in the workspace of its top parent, the one the user sees.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::Member;
use crate::panel_request::Refused;
use crate::remove::repo_of;
use crate::{Agent, Core, CoreError, CoreEvent, Layout, PanelKind, Result, SessionId};

/// One agent session's layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The agent panels in the chat area, in order: the session's own and those of the subagent
    /// sessions below it.
    pub chat: Vec<AgentPanel>,
    /// The panel area's tabs, in order.
    pub panels: Vec<PanelKind>,
    /// One of `panels`. None shows no tab.
    pub selected: Option<PanelKind>,
    /// Whether the terminal panel is open at the bottom of the panel area. The shell in it
    /// belongs to the session's place, so sessions in one place share it.
    pub terminal: bool,
}

/// An agent panel in the chat area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPanel {
    pub agent: Agent,
    pub session: SessionId,
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
    session: SessionId,
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

/// An agent session's workspace, as its record keeps it.
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
    /// `layout` with what it leaves unset from grove's own: the Files and Diff tabs and the
    /// terminal open. The first tab is selected. It has no agent panel: a layout's `chat` only
    /// names the agent a new session starts with.
    pub(crate) fn from_layout(layout: Layout) -> Workspace {
        let panels = layout
            .panels
            .unwrap_or_else(|| vec![PanelKind::Files, PanelKind::Diff]);
        Workspace {
            chat: Vec::new(),
            selected: panels.first().copied(),
            panels,
            terminal: layout.terminal.unwrap_or(true),
        }
    }

    /// With a shown agent panel for `session` after the last one.
    pub(crate) fn with_panel(mut self, session: SessionId, agent: Agent) -> Workspace {
        self.chat.push(AgentPanel {
            agent,
            session,
            shown: true,
        });
        self
    }

    // With a shown panel, after the last one, for each running session of `members` that `now`
    // has a panel for and this has none.
    fn with_running_panels_of(mut self, now: &Workspace, members: &[Member]) -> Workspace {
        for panel in &now.chat {
            let running = |m: &Member| m.session == panel.session && !m.ended;
            let kept = self.chat.iter().any(|p| p.session == panel.session);
            if members.iter().any(running) && !kept {
                self.chat.push(AgentPanel {
                    shown: true,
                    ..panel.clone()
                });
            }
        }
        self
    }

    // As `Core::workspace` returns it: only the panels of `members`, each with the agent its
    // session runs.
    fn listed(&self, members: &[Member]) -> Workspace {
        let chat = self.chat.iter().filter_map(|panel| {
            let member = members.iter().find(|m| m.session == panel.session)?;
            Some(AgentPanel {
                agent: member.agent,
                ..panel.clone()
            })
        });
        Workspace {
            chat: chat.collect(),
            ..self.clone()
        }
    }
}

impl Core {
    /// The workspace a new agent session in `worktree` starts from: its project's profile
    /// layout with the project's overrides, and no agent panel yet. Fails with `BadProfile`
    /// while the project's profile file is not valid.
    pub fn starting_workspace(&self, worktree: &Path) -> Result<Workspace> {
        Ok(Workspace::from_layout(self.starting_layout(worktree)?))
    }

    /// The workspace agent session `session` shows in: its own, or its top parent's when it is
    /// a subagent. An ended session keeps its workspace. A panel whose session was deleted is
    /// left out.
    pub fn workspace(&self, session: SessionId) -> Result<Workspace> {
        self.agents
            .with_workspace(session, |_, members, saved| saved.workspace.listed(members))
    }

    /// Saves `workspace` as the one `session` shows in and sends one `WorkspaceChanged` with
    /// `by`. `by` is the agent session that changed it, None for the user. Undo restores the
    /// workspace from before the last change an agent made. Refused when the selected tab is
    /// not one of the tabs, or when an agent panel names a session twice or one that is neither
    /// the workspace's own session nor a subagent below it. A running session `workspace` has
    /// no panel for, such as a subagent started since the caller read it, keeps its panel,
    /// shown after the last one.
    pub async fn set_workspace(
        &self,
        session: SessionId,
        workspace: Workspace,
        by: Option<SessionId>,
    ) -> Result<()> {
        let owner = self
            .agents
            .with_workspace(session, |owner, members, saved| {
                check_workspace(&workspace, members)?;
                let workspace = workspace.with_running_panels_of(&saved.workspace, members);
                saved.replace(workspace, by);
                Ok::<_, CoreError>(owner)
            })??;
        self.changed(owner, by).await
    }

    /// Restores the workspace `session` shows in from before the last change an agent made to
    /// it, even if the user changed it since, and sends `WorkspaceChanged` with no `by`. It can
    /// be undone once. A running session the restored workspace has no panel for, such as a
    /// subagent started since, keeps its panel, shown after the last one. A panel whose session
    /// was deleted since is left out.
    pub async fn undo_workspace(&self, session: SessionId) -> Result<()> {
        let owner = self
            .agents
            .with_workspace(session, |owner, members, saved| {
                let nothing = CoreError::NothingToUndo(owner);
                let mut restored = saved.before_agent_change.take().ok_or(nothing)?;
                let member =
                    |panel: &AgentPanel| members.iter().any(|m| m.session == panel.session);
                restored.chat.retain(member);
                saved.workspace = restored.with_running_panels_of(&saved.workspace, members);
                Ok::<_, CoreError>(owner)
            })??;
        self.changed(owner, None).await
    }

    async fn changed(&self, owner: SessionId, by: Option<SessionId>) -> Result<()> {
        self.save_state().await?;
        // Having no receivers is fine.
        let _ = self
            .events
            .send(CoreEvent::WorkspaceChanged { session: owner, by });
        Ok(())
    }

    /// Shows agent session `session` in an agent panel of the workspace it shows in: the one it
    /// has there, shown even if it was hidden, or else a new shown one after the last. A new
    /// subagent gets its panel beside its top parent's this way, and a resumed session its own
    /// back. Asks for a save and sends one `WorkspaceChanged` with no `by`.
    pub(crate) fn show_agent_panel(&self, session: SessionId, agent: Agent) {
        let shown = self.agents.with_workspace(session, |owner, _, saved| {
            let chat = &mut saved.workspace.chat;
            match chat.iter_mut().find(|panel| panel.session == session) {
                Some(panel) => panel.shown = true,
                None => chat.push(AgentPanel {
                    agent,
                    session,
                    shown: true,
                }),
            }
            owner
        });
        // It fails only when the session was deleted meanwhile, and its panel went with it.
        if let Ok(owner) = shown {
            self.saves.request();
            self.workspace_changed(owner);
        }
    }

    /// Sends `WorkspaceChanged` with no `by` for the workspace of `owner`.
    pub(crate) fn workspace_changed(&self, owner: SessionId) {
        // Having no receivers is fine.
        let _ = self.events.send(CoreEvent::WorkspaceChanged {
            session: owner,
            by: None,
        });
    }

    /// The workspace agent session `caller` shows in, as grove-mcp gives it.
    pub(crate) fn agent_workspace(&self, caller: SessionId) -> Result<Value> {
        Ok(json!(WorkspaceJson::from(self.workspace(caller)?)))
    }

    /// Replaces the workspace agent session `caller` shows in with `workspace`, as grove-mcp
    /// sends it, with `caller` as `by`. Refuses one that drops the panel of an agent session
    /// running in it, or names a session that is not part of it.
    pub(crate) fn set_agent_workspace(
        &self,
        caller: SessionId,
        workspace: Value,
    ) -> std::result::Result<(), Refused> {
        let workspace: WorkspaceJson =
            serde_json::from_value(workspace).map_err(|e| Refused::BadWorkspace(e.to_string()))?;
        let workspace = Workspace::from(workspace);
        let owner = self
            .agents
            .with_workspace(caller, |owner, members, saved| {
                let now = saved.workspace.listed(members);
                check_sessions(&now, &workspace, members).map_err(Refused::BadWorkspace)?;
                check_workspace(&workspace, members)?;
                saved.replace(workspace, Some(caller));
                Ok::<_, Refused>(owner)
            })??;
        // The panel request thread is no runtime worker, so it may block on one.
        self.runtime.block_on(self.changed(owner, Some(caller)))?;
        Ok(())
    }

    // The layout a new agent session in `worktree` starts from: its repo's profile layout with
    // the repo's overrides. Fails with `BadProfile` while the repo's profile file is not valid,
    // so no session starts from a layout its profile did not give.
    pub(crate) fn starting_layout(&self, worktree: &Path) -> Result<Layout> {
        let repo = repo_of(&self.tracked(), worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?;
        Ok(self.valid_settings_of(&repo)?.layout)
    }

    // `starting_layout` for a session that already exists, which a profile that is not valid
    // must not keep from coming back: such a profile counts as empty.
    pub(crate) fn layout_of(&self, worktree: &Path) -> Result<Layout> {
        let repo = repo_of(&self.tracked(), worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?;
        Ok(self.settings_of(&repo).layout)
    }
}

impl Saved {
    // Only the last agent change can be undone.
    fn replace(&mut self, workspace: Workspace, by: Option<SessionId>) {
        let before = std::mem::replace(&mut self.workspace, workspace);
        if by.is_some() {
            self.before_agent_change = Some(before);
        }
    }
}

// What every saved workspace keeps to: its selected tab is one of its tabs, and each agent panel
// names one of `members`, once.
fn check_workspace(workspace: &Workspace, members: &[Member]) -> Result<()> {
    let invalid = |reason| Err(CoreError::InvalidWorkspace { reason });
    if workspace
        .selected
        .is_some_and(|tab| !workspace.panels.contains(&tab))
    {
        return invalid("its selected tab is not one of its tabs");
    }
    for (at, panel) in workspace.chat.iter().enumerate() {
        if !members.iter().any(|m| m.session == panel.session) {
            return invalid("an agent panel names a session that is not part of it");
        }
        if workspace.chat[..at]
            .iter()
            .any(|p| p.session == panel.session)
        {
            return invalid("two agent panels name the same session");
        }
    }
    Ok(())
}

// What an agent's change keeps to, said so the agent can fix it: each panel of `new` names one of
// `members`, once and with its own agent, and each running session `now` lists keeps its panel.
fn check_sessions(
    now: &Workspace,
    new: &Workspace,
    members: &[Member],
) -> std::result::Result<(), String> {
    let mut kept = Vec::new();
    for panel in &new.chat {
        let session = panel.session;
        if kept.contains(&session) {
            return Err(format!("it names agent session {session} twice"));
        }
        kept.push(session);
        match members.iter().find(|m| m.session == session) {
            None => {
                return Err(format!(
                    "agent session {session} is not this workspace's session or a subagent of it"
                ));
            }
            Some(member) if member.agent != panel.agent => {
                return Err(format!(
                    "agent session {session} runs {}, not {}",
                    member.agent.name(),
                    panel.agent.name()
                ));
            }
            Some(_) => {}
        }
    }
    let running = |session: &SessionId| members.iter().any(|m| m.session == *session && !m.ended);
    let dropped = now
        .chat
        .iter()
        .map(|p| p.session)
        .find(|s| running(s) && !kept.contains(s));
    match dropped {
        Some(session) => Err(format!(
            "it drops the panel of agent session {session}, which is running"
        )),
        None => Ok(()),
    }
}
