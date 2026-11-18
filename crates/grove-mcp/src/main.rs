//! grove's MCP server. An agent session launches it on stdio from the MCP config grove passes,
//! with the session's id and token, and its tools ask the core to act for that session.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use grove_mcp::tools::{self, AgentName, PanelKind, PanelState};
use grove_mcp::{CoreClient, RequestError};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SubagentSpawnArguments {
    /// The agent the subagent runs.
    agent: AgentName,
    /// The subagent's first message.
    prompt: String,
    /// The absolute path of a worktree grove tracks. Defaults to the calling session's own.
    worktree: Option<PathBuf>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PanelOpenArguments {
    /// "log" for output the user only reads, such as the tests. "command" for a terminal the
    /// user can type into, such as a dev server.
    kind: PanelKind,
    /// The panel's tab title.
    title: String,
    /// The command, run with the user's shell in this session's worktree.
    cmd: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PanelReadArguments {
    /// The panel's id, as panel_open returned it.
    id: String,
    /// How many of the last lines to read. Defaults to 50.
    lines: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PanelWaitArguments {
    /// The panel's id, as panel_open returned it.
    id: String,
    /// The state to wait for: "exited", when its command has ended.
    state: PanelState,
    /// How long to wait before failing. Defaults to 60, at most 600.
    timeout_seconds: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PanelCloseArguments {
    /// The panel's id, as panel_open returned it.
    id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WorkspaceSetArguments {
    /// The whole new workspace, in the shape workspace_get returns: {"chat": [{"agent":
    /// "claude"|"codex", "session": <id> or null, "shown": true|false}], "panels": [tabs from
    /// "files", "diff", "browser", "logs", "profile"], "selected": one of the panels or null,
    /// "terminal": true|false}. Keep the panel of every session that runs, with its id.
    workspace: Map<String, Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProfileSetArguments {
    /// The settings to change, in the shape of profile_get's "settings", such as {"agent":
    /// {"model": "sonnet"}} or {"layout": {"panels": ["files", "diff", "logs"]}}. Each setting
    /// given replaces the profile's, and a list replaces the whole list. The rest stay.
    changes: Map<String, Value>,
}

#[derive(Clone)]
struct GroveTools {
    core: Arc<CoreClient>,
}

#[tool_router]
impl GroveTools {
    #[tool(
        description = "Start a subagent: a new Claude Code or Codex session that grove runs \
                       beside this one, in chat mode, in its own panel the user can watch and \
                       type into. Returns the new session's id."
    )]
    async fn subagent_spawn(
        &self,
        Parameters(arguments): Parameters<SubagentSpawnArguments>,
    ) -> CallToolResult {
        self.call(move |core| {
            let worktree = arguments.worktree.as_deref();
            tools::subagent_spawn(core, arguments.agent, &arguments.prompt, worktree)
        })
        .await
    }

    #[tool(
        description = "Open a panel in grove that runs a command with the user's shell in this \
                       session's worktree, as a tab the user can watch: a log, or a terminal the \
                       user can type into. Returns the panel's id. A session holds at most 8 \
                       panels, exited or not, until it closes them."
    )]
    async fn panel_open(
        &self,
        Parameters(arguments): Parameters<PanelOpenArguments>,
    ) -> CallToolResult {
        self.call(move |core| {
            tools::panel_open(core, arguments.kind, &arguments.title, &arguments.cmd)
        })
        .await
    }

    #[tool(
        description = "Read the last lines of one of this session's panels, after a line that \
                       says whether its command still runs or how it exited."
    )]
    async fn panel_read(
        &self,
        Parameters(arguments): Parameters<PanelReadArguments>,
    ) -> CallToolResult {
        self.call(move |core| tools::panel_read(core, &arguments.id, arguments.lines))
            .await
    }

    #[tool(
        description = "Wait until one of this session's panels reaches a state: \"exited\", once \
                       its command, and anything it left writing to the panel, have ended. Fails \
                       if it has not after timeout_seconds."
    )]
    async fn panel_wait(
        &self,
        Parameters(arguments): Parameters<PanelWaitArguments>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        // Cancelled when the client cancels the call, and when the client leaves. Nobody hears
        // the answer then, and grove-mcp could not end before the wait does.
        let cancelled = context.ct.clone();
        self.call(move |core| {
            tools::panel_wait(
                core,
                &arguments.id,
                arguments.state,
                arguments.timeout_seconds,
                || cancelled.is_cancelled(),
            )
        })
        .await
    }

    #[tool(description = "Close one of this session's panels and kill every process in it.")]
    async fn panel_close(
        &self,
        Parameters(arguments): Parameters<PanelCloseArguments>,
    ) -> CallToolResult {
        self.call(move |core| tools::panel_close(core, &arguments.id))
            .await
    }

    #[tool(
        description = "Read the workspace of this session's worktree as JSON: the agent panels \
                       in the chat area, the panel area's tabs, the selected tab and whether the \
                       terminal is open."
    )]
    async fn workspace_get(&self) -> CallToolResult {
        self.call(tools::workspace_get).await
    }

    #[tool(
        description = "Replace the workspace of this session's worktree, such as to open the Logs \
                       tab. Read it with workspace_get first and send it back changed. The user \
                       sees a notice naming this session and can undo it."
    )]
    async fn workspace_set(
        &self,
        Parameters(arguments): Parameters<WorkspaceSetArguments>,
    ) -> CallToolResult {
        self.call(move |core| tools::workspace_set(core, Value::Object(arguments.workspace)))
            .await
    }

    #[tool(
        description = "Read the profile this session's repo uses, as JSON: its name and its \
                       settings, such as the default agent and model, the layout new worktrees \
                       start with, setup, cleanup and run sessions."
    )]
    async fn profile_get(&self) -> CallToolResult {
        self.call(tools::profile_get).await
    }

    #[tool(
        description = "Change settings of the profile this session's repo uses, such as the \
                       default model. It changes the profile file, so every repo that uses the \
                       profile gets the change. The user sees a notice naming this session."
    )]
    async fn profile_set(
        &self,
        Parameters(arguments): Parameters<ProfileSetArguments>,
    ) -> CallToolResult {
        self.call(move |core| tools::profile_set(core, Value::Object(arguments.changes)))
            .await
    }
}

impl GroveTools {
    // The socket client blocks, and so does the wait between polls, so a tool runs off the
    // async thread.
    async fn call(
        &self,
        tool: impl FnOnce(&CoreClient) -> Result<String, String> + Send + 'static,
    ) -> CallToolResult {
        let core = self.core.clone();
        let text = tokio::task::spawn_blocking(move || tool(&core))
            .await
            .unwrap_or_else(|join| Err(RequestError::Io(std::io::Error::other(join)).to_string()));
        match text {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(text) => CallToolResult::error(vec![ContentBlock::text(text)]),
        }
    }
}

#[tool_handler(name = "grove")]
impl ServerHandler for GroveTools {}

fn main() -> anyhow::Result<()> {
    let core = CoreClient::from_env()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("start the runtime")?;
    runtime.block_on(async {
        let grove = GroveTools {
            core: Arc::new(core),
        };
        let server = grove
            .serve(rmcp::transport::stdio())
            .await
            .context("start the MCP server")?;
        server.waiting().await.context("serve")?;
        Ok(())
    })
}
