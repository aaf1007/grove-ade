use std::path::PathBuf;
use std::time::Duration;

/// Where the core keeps its files and how it runs. Tests point it at a temp folder.
#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// grove's home folder, readable only by the user.
    pub home: PathBuf,
    /// The Unix socket hooks and MCP servers talk to. macOS caps its path at 104 bytes.
    pub socket: PathBuf,
    /// The command that starts Claude Code.
    pub claude_command: PathBuf,
    /// The command that starts Codex.
    pub codex_command: PathBuf,
    /// The command that starts the Claude sidecar, which runs Claude in chat mode.
    pub claude_sidecar_command: PathBuf,
    /// The command that starts `grove-mcp`, the MCP server every agent session gets. None gives
    /// agent sessions no grove MCP server.
    pub mcp_server: Option<PathBuf>,
    /// The path of `grove-hook`, which agent hooks run.
    pub hook_relay: PathBuf,
    /// The grove-hook that ships with grove, such as the one in the app bundle. At start the
    /// core copies it to `hook_relay` when the two differ. None skips the copy.
    pub hook_relay_source: Option<PathBuf>,
    /// Output that comes sooner than this after a session's last frame waits until this long
    /// after it, and goes out with everything read meanwhile.
    pub output_flush_window: Duration,
    /// Unacked output bytes at which a session's reader pauses.
    pub output_high_watermark: usize,
    /// Unacked output bytes below which a paused reader resumes.
    pub output_low_watermark: usize,
    /// After the user writes to a `NeedsInput` session, output within this window makes it `Working`.
    pub needs_input_to_working: Duration,
    /// A `Working` session with no hook event and no output for this long gets a screen check.
    pub stuck_working_after: Duration,
    /// The shell the core runs once at start, as a login shell, to read the environment every
    /// session gets. None skips the read, so sessions get the minimal environment.
    pub login_shell: Option<PathBuf>,
    /// How long the login shell gets to print its environment.
    pub login_shell_timeout: Duration,
}

impl CoreConfig {
    /// Defaults for the current user, with the home folder at `~/.grovecode/` and the login
    /// shell from `$SHELL`. Returns None when the user's home directory is unknown.
    pub fn for_user() -> Option<Self> {
        let mut cfg = Self::new(std::env::home_dir()?.join(".grovecode"));
        // zsh is the macOS default.
        let shell = std::env::var_os("SHELL").map_or_else(|| "/bin/zsh".into(), PathBuf::from);
        cfg.login_shell = Some(shell);
        Some(cfg)
    }

    /// Defaults with every path inside `home`.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            socket: home.join("grove.sock"),
            claude_command: "claude".into(),
            codex_command: "codex".into(),
            // The app points it at the sidecar inside its bundle.
            claude_sidecar_command: "claude-sidecar".into(),
            // The app sets it when its bundle has one.
            mcp_server: None,
            hook_relay: home.join("bin/grove-hook"),
            hook_relay_source: None,
            output_flush_window: Duration::from_millis(8),
            output_high_watermark: 128 * 1024,
            output_low_watermark: 32 * 1024,
            needs_input_to_working: Duration::from_secs(2),
            stuck_working_after: Duration::from_secs(10),
            login_shell: None,
            login_shell_timeout: Duration::from_secs(5),
            home,
        }
    }
}
