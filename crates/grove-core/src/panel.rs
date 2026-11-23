//! Log and command panels: commands an agent session runs through grove-mcp, each as a session
//! in that agent session's worktree. Only the agent session that opened a panel can read, wait
//! on or close it. A panel outlives its agent session, and does not come back after a restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::panel_request::{self, CommandPanelKind, Request};
use crate::restore::lock;
use crate::session::{Kind, Sessions, kill_session};
use crate::{Core, CoreError, CoreEvent, PanelSpec, Result, SessionId, SpawnSpec};

/// The most panels one agent session holds at once. A panel whose command exited still counts
/// until it is closed, since grove keeps its output for reads.
pub(crate) const MAX_PANELS_PER_SESSION: usize = 8;

/// The open panels, by their session id.
pub(crate) type Panels = HashMap<SessionId, Panel>;

pub(crate) struct Panel {
    owner: SessionId,
    worktree: PathBuf,
    /// None while its command runs. Then its exit code, None for a signal.
    exit: Option<Option<i32>>,
}

impl Core {
    /// Answers a panel request from `session`, which the socket task authenticated.
    pub(crate) fn answer_panel_request(&self, session: SessionId, request: Request) -> Value {
        match request {
            Request::SessionId => panel_request::session_id_reply(session),
            Request::SubagentSpawn {
                agent,
                prompt,
                worktree,
            } => {
                panel_request::spawned_reply(self.spawn_subagent(session, agent, prompt, worktree))
            }
            Request::PanelOpen { kind, title, cmd } => panel_request::reply(
                self.open_panel(session, kind, title, cmd)
                    .map(|panel| json!({ "panel": panel.to_string() })),
            ),
            Request::PanelRead { panel, lines } => {
                panel_request::reply(self.read_panel(session, &panel, lines))
            }
            Request::PanelClose { panel } => {
                panel_request::reply(self.close_panel(session, &panel).map(|()| json!({})))
            }
            Request::WorkspaceGet => panel_request::reply(self.agent_workspace(session)),
            Request::WorkspaceSet { workspace } => panel_request::reply(
                self.set_agent_workspace(session, workspace)
                    .map(|()| json!({})),
            ),
            Request::ProfileGet => panel_request::reply(self.agent_profile(session)),
            Request::ProfileSet { changes } => panel_request::reply(
                self.change_agent_profile(session, changes)
                    .map(|profile| json!({ "profile": profile })),
            ),
            Request::HtmlShow {
                title,
                html,
                path,
                height,
            } => panel_request::reply(
                self.show_inline_page(session, title, html, path, height)
                    .map(|page| json!({ "page": page })),
            ),
        }
    }

    /// The worktree of agent session `caller`, which a request acts on. It takes none from the
    /// caller, so a session can act only on its own.
    pub(crate) fn caller_worktree(&self, caller: SessionId) -> Result<PathBuf> {
        self.agents
            .running_worktree(caller)
            .ok_or(CoreError::SessionNotRunning(caller))
    }

    /// Runs `cmd` with the user's shell as a new panel of agent session `owner`, in its
    /// worktree. Sends `PanelRequested` with `owner` as the parent.
    fn open_panel(
        &self,
        owner: SessionId,
        kind: CommandPanelKind,
        title: String,
        cmd: String,
    ) -> Result<SessionId> {
        let worktree = self
            .agents
            .running_worktree(owner)
            .ok_or(CoreError::NotAnAgentSession(owner))?;
        let id = self.sessions.new_id();
        {
            // Counted and listed under one lock, so two opens at once cannot pass the cap.
            let mut panels = self.panels();
            let open = panels.values().filter(|p| p.owner == owner).count();
            if open >= MAX_PANELS_PER_SESSION {
                return Err(CoreError::TooManyPanels {
                    session: owner,
                    limit: MAX_PANELS_PER_SESSION,
                });
            }
            // Listed before it starts, so a removal of the worktree from now on drops it.
            let panel = Panel {
                owner,
                worktree: worktree.clone(),
                exit: None,
            };
            panels.insert(id, panel);
        }
        let exited = self.sessions.keep(id);
        let output_ended = self.sessions.output_ended(id);
        // The panel resizes it once it has measured itself.
        let command = SpawnSpec {
            command: self.run_shell.clone(),
            args: vec!["-c".into(), cmd.into()],
            cols: 80,
            rows: 24,
        };
        // No variables of the agent session's, its token above all.
        if let Err(e) = self.start_session(id, &worktree, command, &[], Kind::Command) {
            self.panels().remove(&id);
            self.sessions.release(id);
            return Err(e);
        }
        self.record_exit(id, exited, output_ended);
        let panels = self.panels();
        // Held while it sends, so a removal that already dropped the panel and sent
        // `PanelClosed` is never followed by this.
        if !panels.contains_key(&id) {
            return Err(CoreError::Removing(worktree));
        }
        let panel = match kind {
            CommandPanelKind::Log => PanelSpec::Log {
                session: id,
                worktree,
                title,
            },
            CommandPanelKind::Command => PanelSpec::Command {
                session: id,
                worktree,
                title,
            },
        };
        // Having no receivers is fine.
        let _ = self.events.send(CoreEvent::PanelRequested {
            parent: Some(owner),
            panel,
        });
        Ok(id)
    }

    // Records the panel's exit code once its command exits and the parser has the last of its
    // output, so a read after that has it all. On Linux a process the command left holding the
    // terminal keeps the panel running until it exits too. macOS cuts it off from the terminal
    // when the command exits.
    fn record_exit(
        &self,
        id: SessionId,
        exited: oneshot::Receiver<Option<i32>>,
        output_ended: oneshot::Receiver<()>,
    ) {
        let panels = Arc::clone(&self.panels);
        self.runtime.spawn(async move {
            // Each sender drops unheard when the panel closed first, or the core stops.
            let (Ok(code), Ok(())) = (exited.await, output_ended.await) else {
                return;
            };
            if let Some(panel) = lock(&panels).get_mut(&id) {
                panel.exit = Some(code);
            }
        });
    }

    /// The last `lines` lines of `caller`'s panel `panel`, and whether its command runs.
    fn read_panel(&self, caller: SessionId, panel: &str, lines: usize) -> Result<Value> {
        let (id, exit) = {
            let panels = self.panels();
            let (id, found) = owned(&panels, caller, panel)?;
            (id, found.exit)
        };
        // A wait asks for none, and leaves the parser to the reader.
        let lines = if lines == 0 {
            Vec::new()
        } else {
            let terminal = self
                .sessions
                .terminal(id)
                // It closed since.
                .ok_or_else(|| unknown(caller, panel))?;
            lock(&terminal).last_lines(lines)
        };
        Ok(json!({
            "lines": lines,
            "running": exit.is_none(),
            "code": exit.flatten(),
        }))
    }

    /// Closes `caller`'s panel `panel`: kills every process in it, drops its output and sends
    /// `PanelClosed`.
    fn close_panel(&self, caller: SessionId, panel: &str) -> Result<()> {
        let id = {
            let mut panels = self.panels();
            let (id, _) = owned(&panels, caller, panel)?;
            panels.remove(&id);
            id
        };
        self.end_panel(id);
        let _ = self.events.send(CoreEvent::PanelClosed { panel: id });
        Ok(())
    }

    // Kills every process still in the panel's process session, even once the command's own
    // process has exited, and drops its output.
    fn end_panel(&self, id: SessionId) {
        if let Some(leader) = self.sessions.release_with_leader(id) {
            // Listing every process reads /proc on Linux, so it stays off the caller's thread.
            self.runtime.spawn_blocking(move || kill_session(leader));
        }
    }

    /// Drops the panels in `worktree`, which is being removed and whose sessions have ended,
    /// kills what their commands left running, and sends `PanelClosed` for each.
    pub(crate) fn drop_panels_in(&self, worktree: &Path) {
        let dropped: Vec<SessionId> = {
            let mut panels = self.panels();
            let ids: Vec<SessionId> = panels
                .iter()
                .filter(|(_, p)| p.worktree == worktree)
                .map(|(&id, _)| id)
                .collect();
            for id in &ids {
                panels.remove(id);
            }
            ids
        };
        for id in dropped {
            self.end_panel(id);
            let _ = self.events.send(CoreEvent::PanelClosed { panel: id });
        }
    }

    fn panels(&self) -> MutexGuard<'_, Panels> {
        lock(&self.panels)
    }
}

/// When grove quits: kills every process in each panel's process session, its command's own
/// when it still runs and what the command left running. Sends no `PanelClosed`.
pub(crate) fn kill_left_on_quit(panels: &Mutex<Panels>, sessions: &Sessions) {
    for id in lock(panels).keys() {
        if let Some(leader) = sessions.release_with_leader(*id) {
            kill_session(leader);
        }
    }
}

// The panel `named` and its id, when `caller` opened it.
fn owned<'a>(panels: &'a Panels, caller: SessionId, named: &str) -> Result<(SessionId, &'a Panel)> {
    let id = named
        .parse()
        .ok()
        .map(SessionId)
        // Only the form the id prints in, so "+3" or "03" names no panel.
        .filter(|id| id.to_string() == named);
    id.and_then(|id| panels.get(&id).map(|panel| (id, panel)))
        .filter(|(_, panel)| panel.owner == caller)
        .ok_or_else(|| unknown(caller, named))
}

fn unknown(caller: SessionId, panel: &str) -> CoreError {
    CoreError::UnknownPanel {
        session: caller,
        panel: panel.to_owned(),
    }
}
