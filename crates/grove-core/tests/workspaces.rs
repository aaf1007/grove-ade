//! Workspaces: each worktree's agent panels, panel-area tabs and bottom terminal, starting from
//! its profile's layout and saved in the state file.

use std::fs;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, AgentPanel, AgentSpec, Core, CoreConfig, CoreError, CoreEvent, Layout, Mode,
    NewWorktree, PanelKind, ProfileSettings, RemoveOpts, RepoSettings, SessionId, SpawnSpec,
    Workspace,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

const TIMEOUT: Duration = Duration::from_secs(5);

// A child that another test's thread has forked keeps every open file of this process until it
// execs, grove.lock included. A core stopped in that window stays locked, and the next start
// fails with AlreadyRunning. Tests here restart cores, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
// Tests keep the home folder, and the socket in it, under /tmp.
fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

fn start(dir: &Path) -> (Core, broadcast::Receiver<CoreEvent>) {
    Core::start(CoreConfig::new(dir.join("home"))).unwrap()
}

// The core owns its runtime, and dropping it inside an async context panics.
// So tests make and drop the core outside, and only await its calls here.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

// Runs git with the user's config turned off, so their hooks or signing settings never apply.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Grove Test")
        .env("GIT_AUTHOR_EMAIL", "test@grove.invalid")
        .env("GIT_COMMITTER_NAME", "Grove Test")
        .env("GIT_COMMITTER_EMAIL", "test@grove.invalid")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

// A repo on `main` with one commit. Returns its real path, as git reports it.
fn new_repo(dir: &Path, name: &str) -> PathBuf {
    let repo = dir.join(name);
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    repo.canonicalize().unwrap()
}

// A tracked repo, in a started core.
fn core_with_repo(dir: &Path) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf) {
    let repo = new_repo(dir, "repo");
    let (core, events) = start(dir);
    block_on(core.add_repo(&repo)).unwrap();
    (core, events, repo)
}

fn use_profile(core: &Core, repo: &Path, profile: Option<&str>, layout: Layout) {
    let settings = RepoSettings {
        profile: profile.map(Into::into),
        overrides: ProfileSettings {
            layout,
            ..ProfileSettings::default()
        },
    };
    block_on(core.set_repo_settings(repo, settings)).unwrap();
}

fn sleeper(core: &Core, worktree: &Path) -> SessionId {
    let spec = SpawnSpec {
        command: "/bin/sleep".into(),
        args: vec!["30".into()],
        cols: 80,
        rows: 24,
    };
    core.spawn(worktree, spec).unwrap()
}

fn panel(agent: Agent, session: Option<SessionId>, shown: bool) -> AgentPanel {
    AgentPanel {
        agent,
        session,
        shown,
    }
}

// Claude and Codex side by side, with a hidden Codex panel, Logs selected and no terminal.
fn arranged() -> Workspace {
    Workspace {
        chat: vec![
            panel(Agent::Codex, None, true),
            panel(Agent::Claude, None, true),
            panel(Agent::Codex, None, false),
        ],
        panels: vec![PanelKind::Diff, PanelKind::Logs],
        selected: Some(PanelKind::Logs),
        terminal: false,
    }
}

// Every `WorkspaceChanged` within `window`.
fn workspace_changes_within(
    events: &mut broadcast::Receiver<CoreEvent>,
    window: Duration,
) -> Vec<(PathBuf, Option<SessionId>)> {
    let end = Instant::now() + window;
    let mut seen = Vec::new();
    while Instant::now() < end {
        match events.try_recv() {
            Ok(CoreEvent::WorkspaceChanged { worktree, by }) => seen.push((worktree, by)),
            Ok(_) => {}
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("{e}"),
        }
    }
    seen
}

fn wait_exited(events: &mut broadcast::Receiver<CoreEvent>, session: SessionId) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(CoreEvent::SessionExited { session: s, .. }) if s == session => return,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "no SessionExited");
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_new_worktree_starts_from_its_profiles_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    use_profile(&core, &repo, Some("Rust crate"), Layout::default());

    // The starter: chat = ["claude"], panels = ["files", "diff"], terminal = true.
    assert_eq!(
        core.workspace(&repo).unwrap(),
        Workspace {
            chat: vec![panel(Agent::Claude, None, true)],
            panels: vec![PanelKind::Files, PanelKind::Diff],
            selected: Some(PanelKind::Files),
            terminal: true,
        }
    );
}

#[test]
fn the_repos_layout_overrides_win_over_the_profiles() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let overrides = Layout {
        chat: Some(vec![Agent::Codex, Agent::Claude]),
        panels: Some(vec![PanelKind::Browser]),
        terminal: None,
    };
    use_profile(&core, &repo, Some("Rust crate"), overrides);

    assert_eq!(
        core.workspace(&repo).unwrap(),
        Workspace {
            chat: vec![
                panel(Agent::Codex, None, true),
                panel(Agent::Claude, None, true),
            ],
            panels: vec![PanelKind::Browser],
            selected: Some(PanelKind::Browser),
            terminal: true,
        }
    );
}

#[test]
fn a_repo_without_a_profile_gets_the_built_in_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let built_in = Workspace {
        chat: vec![panel(Agent::Claude, None, true)],
        panels: vec![PanelKind::Files, PanelKind::Diff],
        selected: Some(PanelKind::Files),
        terminal: true,
    };

    assert_eq!(core.workspace(&repo).unwrap(), built_in);

    // Only what the overrides leave unset comes from it.
    let no_tabs = Layout {
        panels: Some(Vec::new()),
        ..Layout::default()
    };
    use_profile(&core, &repo, None, no_tabs);
    assert_eq!(
        core.workspace(&repo).unwrap(),
        Workspace {
            panels: Vec::new(),
            selected: None,
            ..built_in
        }
    );
}

#[test]
fn a_saved_workspace_comes_back_after_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());

    block_on(core.set_workspace(&repo, arranged(), None)).unwrap();
    assert_eq!(core.workspace(&repo).unwrap(), arranged());
    drop(core);
    let (core, _events) = start(dir.path());

    assert_eq!(core.workspace(&repo).unwrap(), arranged());
}

// A plain command does not come back after a restart, so a new session may take its id. Its
// panel goes, so that session never shows in it.
#[test]
fn a_panel_whose_session_does_not_come_back_is_gone_after_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let session = sleeper(&core, &repo);
    let mut workspace = arranged();
    workspace.chat[1].session = Some(session);
    block_on(core.set_workspace(&repo, workspace, None)).unwrap();
    drop(core);

    let (core, _events) = start(dir.path());
    let other = sleeper(&core, &repo);

    assert_eq!(other, session);
    let mut restored = arranged();
    restored.chat.remove(1);
    assert_eq!(core.workspace(&repo).unwrap(), restored);
}

#[test]
fn each_change_sends_one_workspace_changed_naming_who_made_it() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let agent = sleeper(&core, &repo);

    block_on(core.set_workspace(&repo, arranged(), Some(agent))).unwrap();
    block_on(core.set_workspace(&repo, arranged(), None)).unwrap();

    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(repo.clone(), Some(agent)), (repo, None)]
    );
}

#[test]
fn undo_restores_the_workspace_from_before_the_last_agent_change() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let agent = sleeper(&core, &repo);
    let mut before = arranged();
    before.chat[0].session = Some(agent);
    block_on(core.set_workspace(&repo, before.clone(), None)).unwrap();
    let opened_files = Workspace {
        panels: vec![PanelKind::Diff, PanelKind::Logs, PanelKind::Files],
        selected: Some(PanelKind::Files),
        ..before.clone()
    };
    block_on(core.set_workspace(&repo, opened_files.clone(), Some(agent))).unwrap();
    // The user's change after it is undone too.
    let terminal_open = Workspace {
        terminal: true,
        ..opened_files
    };
    block_on(core.set_workspace(&repo, terminal_open, None)).unwrap();
    workspace_changes_within(&mut events, Duration::from_millis(100));

    block_on(core.undo_workspace(&repo)).unwrap();

    assert_eq!(core.workspace(&repo).unwrap(), before);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(repo.clone(), None)]
    );
    // Saved, as any change is. The plain command in the first panel does not come back, so
    // that panel goes.
    drop(core);
    let (core, _events) = start(dir.path());
    let mut restored = arranged();
    restored.chat.remove(0);
    assert_eq!(core.workspace(&repo).unwrap(), restored);
}

#[test]
fn only_the_last_agent_change_can_be_undone_and_only_once() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let agent = sleeper(&core, &repo);
    let second = arranged();
    let third = Workspace {
        terminal: true,
        ..arranged()
    };
    block_on(core.set_workspace(&repo, second.clone(), Some(agent))).unwrap();
    block_on(core.set_workspace(&repo, third, Some(agent))).unwrap();

    block_on(core.undo_workspace(&repo)).unwrap();
    assert_eq!(core.workspace(&repo).unwrap(), second);

    let err = block_on(core.undo_workspace(&repo)).unwrap_err();
    assert!(
        matches!(err, CoreError::NothingToUndo(ref w) if *w == repo),
        "{err}"
    );
    assert_eq!(core.workspace(&repo).unwrap(), second);
}

#[test]
fn undo_puts_a_session_placed_after_the_agent_change_in_its_own_panel_when_that_is_empty() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_fake_agents(dir.path());
    let agent = sleeper(&core, &repo);
    let before = Workspace {
        chat: vec![
            panel(Agent::Claude, None, true),
            panel(Agent::Codex, None, true),
        ],
        ..arranged()
    };
    block_on(core.set_workspace(&repo, before.clone(), None)).unwrap();
    let one_panel = Workspace {
        chat: vec![panel(Agent::Claude, None, true)],
        ..before.clone()
    };
    block_on(core.set_workspace(&repo, one_panel, Some(agent))).unwrap();
    // Placed in a new panel after the last, the second one.
    let placed = core
        .spawn_agent(&repo, in_panel(Agent::Codex, None))
        .unwrap();
    workspace_changes_within(&mut events, Duration::from_millis(100));

    block_on(core.undo_workspace(&repo)).unwrap();

    assert_eq!(
        core.workspace(&repo).unwrap(),
        Workspace {
            chat: vec![
                panel(Agent::Claude, None, true),
                panel(Agent::Codex, Some(placed), true),
            ],
            ..before
        }
    );
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(repo, None)]
    );
}

#[test]
fn undo_puts_a_placed_session_after_the_last_panel_when_its_own_is_taken() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_fake_agents(dir.path());
    let agent = sleeper(&core, &repo);
    let first = core
        .spawn_agent(&repo, in_panel(Agent::Claude, Some(0)))
        .unwrap();
    let before = core.workspace(&repo).unwrap();
    let swapped = Workspace {
        chat: vec![
            panel(Agent::Codex, None, true),
            panel(Agent::Claude, Some(first), true),
        ],
        ..before.clone()
    };
    block_on(core.set_workspace(&repo, swapped, Some(agent))).unwrap();
    let placed = core
        .spawn_agent(&repo, in_panel(Agent::Codex, Some(0)))
        .unwrap();

    block_on(core.undo_workspace(&repo)).unwrap();

    assert_eq!(
        core.workspace(&repo).unwrap().chat,
        [
            panel(Agent::Claude, Some(first), true),
            panel(Agent::Codex, Some(placed), true),
        ]
    );
}

#[test]
fn undo_leaves_out_the_panel_of_a_session_that_ended() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_fake_agents(dir.path());
    let agent = sleeper(&core, &repo);
    let before = core.workspace(&repo).unwrap();
    block_on(core.set_workspace(&repo, arranged(), Some(agent))).unwrap();
    let placed = core
        .spawn_agent(&repo, in_panel(Agent::Codex, None))
        .unwrap();
    core.kill(placed).unwrap();
    wait_exited(&mut events, placed);

    block_on(core.undo_workspace(&repo)).unwrap();

    assert_eq!(core.workspace(&repo).unwrap(), before);
}

#[test]
fn save_to_profile_writes_the_shown_agents_tabs_and_terminal_into_the_repos_profile() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    use_profile(&core, &repo, Some("Rust crate"), Layout::default());
    block_on(core.set_workspace(&repo, arranged(), None)).unwrap();
    while events.try_recv().is_ok() {}

    block_on(core.save_workspace_to_profile(&repo)).unwrap();

    let profile = core
        .profiles()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "Rust crate")
        .unwrap();
    // The hidden Codex panel and the selected tab have no place in a layout.
    assert_eq!(
        profile.settings.layout,
        Layout {
            chat: Some(vec![Agent::Codex, Agent::Claude]),
            panels: Some(vec![PanelKind::Diff, PanelKind::Logs]),
            terminal: Some(false),
        }
    );
    let mut by = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let CoreEvent::ProfilesChanged { by: who } = event {
            by.push(who);
        }
    }
    assert_eq!(by, [None]);
}

#[test]
fn save_to_profile_is_refused_for_a_repo_with_no_profile() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    use_profile(&core, &repo, None, Layout::default());

    let err = block_on(core.save_workspace_to_profile(&repo)).unwrap_err();

    assert!(
        matches!(err, CoreError::NoRepoProfile(ref r) if *r == repo),
        "{err}"
    );
}

#[test]
fn a_user_change_alone_leaves_nothing_to_undo() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());

    block_on(core.set_workspace(&repo, arranged(), None)).unwrap();

    let err = block_on(core.undo_workspace(&repo)).unwrap_err();
    assert!(matches!(err, CoreError::NothingToUndo(_)), "{err}");
}

#[test]
fn a_panel_whose_session_is_gone_drops_on_read() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let other = new_repo(dir.path(), "other");
    block_on(core.add_repo(&other)).unwrap();
    let ended = sleeper(&core, &repo);
    let running = sleeper(&core, &repo);
    let elsewhere = sleeper(&core, &other);
    let workspace = Workspace {
        chat: vec![
            panel(Agent::Claude, Some(ended), true),
            panel(Agent::Codex, Some(running), true),
            panel(Agent::Claude, Some(elsewhere), true),
            panel(Agent::Claude, None, false),
        ],
        ..arranged()
    };
    block_on(core.set_workspace(&repo, workspace, None)).unwrap();

    core.kill(ended).unwrap();
    wait_exited(&mut events, ended);

    assert_eq!(
        core.workspace(&repo).unwrap().chat,
        [
            panel(Agent::Codex, Some(running), true),
            panel(Agent::Claude, None, false),
        ]
    );
}

#[test]
fn a_selected_tab_that_is_not_open_is_refused() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    block_on(core.set_workspace(&repo, arranged(), None)).unwrap();
    let workspace = Workspace {
        selected: Some(PanelKind::Browser),
        terminal: true,
        ..arranged()
    };

    let err = block_on(core.set_workspace(&repo, workspace, None)).unwrap_err();

    assert!(matches!(err, CoreError::InvalidWorkspace { .. }), "{err}");
    assert_eq!(core.workspace(&repo).unwrap(), arranged());
}

#[test]
fn an_untracked_worktree_has_no_workspace() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, _repo) = core_with_repo(dir.path());
    let nowhere = dir.path().join("nowhere");

    assert!(matches!(
        core.workspace(&nowhere),
        Err(CoreError::UnknownWorktree(_))
    ));
    assert!(matches!(
        block_on(core.set_workspace(&nowhere, arranged(), None)),
        Err(CoreError::UnknownWorktree(_))
    ));
    assert!(matches!(
        block_on(core.undo_workspace(&nowhere)),
        Err(CoreError::UnknownWorktree(_))
    ));
}

#[test]
fn a_forgotten_repo_added_again_starts_from_the_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let fresh = core.workspace(&repo).unwrap();
    let agent = sleeper(&core, &repo);
    block_on(core.set_workspace(&repo, arranged(), Some(agent))).unwrap();

    block_on(core.remove_worktree(&repo, RemoveOpts::ForgetRepo)).unwrap();
    block_on(core.add_repo(&repo)).unwrap();

    assert_eq!(core.workspace(&repo).unwrap(), fresh);
    assert!(matches!(
        block_on(core.undo_workspace(&repo)),
        Err(CoreError::NothingToUndo(_))
    ));
}

#[test]
fn a_deleted_worktree_made_again_starts_from_the_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let new = || NewWorktree {
        branch: "feature".into(),
        display_name: None,
        issue_link: None,
        base_branch: None,
    };
    let feature = block_on(core.create_worktree(&repo, new()))
        .unwrap()
        .worktree
        .path;
    let fresh = core.workspace(&feature).unwrap();
    block_on(core.set_workspace(&feature, arranged(), None)).unwrap();
    let delete = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };

    block_on(core.remove_worktree(&feature, delete)).unwrap();
    let again = block_on(core.create_worktree(&repo, new()))
        .unwrap()
        .worktree
        .path;

    assert_eq!(again, feature);
    assert_eq!(core.workspace(&feature).unwrap(), fresh);
}

#[test]
fn a_state_file_from_before_workspaces_starts_each_worktree_from_the_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let cfg = CoreConfig::new(dir.path().join("home"));
    fs::create_dir(&cfg.home).unwrap();
    // As a version 3 grove wrote it.
    let v3 = format!(
        r#"{{"version": 3, "repos": [{{"main_worktree": {{"path": "{}", "branch": "main",
        "display_name": "main", "issue_link": null, "base_branch": "main"}},
        "linked_worktrees": [], "profile": "Rust crate", "overrides": {{}}}}]}}"#,
        repo.display()
    );
    let state = cfg.home.join("state.json");
    fs::write(&state, v3).unwrap();

    let (core, _events) = Core::start(cfg).unwrap();

    assert_eq!(
        core.workspace(&repo).unwrap().panels,
        [PanelKind::Files, PanelKind::Diff]
    );
    block_on(core.set_workspace(&repo, arranged(), None)).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(saved["version"], 8);
}

// A tracked repo, in a started core whose Claude Code, Claude sidecar and Codex are a script
// that only waits, as the agents would.
fn core_with_fake_agents(dir: &Path) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf) {
    let fake = dir.join("agent");
    fs::write(&fake, "#!/bin/sh\nexec sleep 1000\n").unwrap();
    let status = Command::new("chmod").arg("+x").arg(&fake).status().unwrap();
    assert!(status.success());
    let mut cfg = CoreConfig::new(dir.join("home"));
    cfg.claude_command = fake.clone();
    cfg.claude_sidecar_command = fake.clone();
    cfg.codex_command = fake;
    let repo = new_repo(dir, "repo");
    let (core, events) = Core::start(cfg).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    (core, events, repo)
}

fn in_panel(agent: Agent, panel: Option<usize>) -> AgentSpec {
    AgentSpec {
        agent,
        mode: Mode::Terminal,
        prompt: None,
        cols: 80,
        rows: 24,
        panel,
    }
}

#[test]
fn an_agent_session_starts_in_the_panel_it_names_with_one_workspace_changed() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_fake_agents(dir.path());
    // The built-in layout: one Claude panel with no session yet.

    let session = core
        .spawn_agent(&repo, in_panel(Agent::Claude, Some(0)))
        .unwrap();

    assert_eq!(
        core.workspace(&repo).unwrap().chat,
        [panel(Agent::Claude, Some(session), true)]
    );
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(repo, None)]
    );
}

#[test]
fn a_panel_takes_the_agent_of_the_session_started_in_it() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_fake_agents(dir.path());

    let session = core
        .spawn_agent(&repo, in_panel(Agent::Codex, Some(0)))
        .unwrap();

    assert_eq!(
        core.workspace(&repo).unwrap().chat,
        [panel(Agent::Codex, Some(session), true)]
    );
}

#[test]
fn a_panel_with_a_running_session_keeps_it_and_the_new_one_goes_after_the_last() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_fake_agents(dir.path());
    let hidden = Workspace {
        chat: vec![
            panel(Agent::Claude, None, true),
            panel(Agent::Codex, None, false),
        ],
        ..arranged()
    };
    block_on(core.set_workspace(&repo, hidden, None)).unwrap();
    let first = core
        .spawn_agent(&repo, in_panel(Agent::Claude, Some(0)))
        .unwrap();

    let second = core
        .spawn_agent(&repo, in_panel(Agent::Claude, Some(0)))
        .unwrap();

    assert_eq!(
        core.workspace(&repo).unwrap().chat,
        [
            panel(Agent::Claude, Some(first), true),
            panel(Agent::Codex, None, false),
            panel(Agent::Claude, Some(second), true),
        ]
    );
}

#[test]
fn an_agent_session_with_no_panel_goes_in_a_new_one_after_the_last() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_fake_agents(dir.path());

    let session = core
        .spawn_agent(&repo, in_panel(Agent::Codex, None))
        .unwrap();

    assert_eq!(
        core.workspace(&repo).unwrap().chat,
        [
            panel(Agent::Claude, None, true),
            panel(Agent::Codex, Some(session), true),
        ]
    );
}

#[test]
fn a_panel_past_the_last_is_refused_and_starts_nothing() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_fake_agents(dir.path());
    let before = core.workspace(&repo).unwrap();

    let err = core
        .spawn_agent(&repo, in_panel(Agent::Claude, Some(1)))
        .unwrap_err();

    assert!(matches!(err, CoreError::InvalidWorkspace { .. }), "{err}");
    assert_eq!(core.sessions(&repo).unwrap(), []);
    assert_eq!(core.workspace(&repo).unwrap(), before);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        []
    );
}

#[test]
fn an_agent_session_that_fails_to_start_leaves_the_workspace_unchanged() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_fake_agents(dir.path());
    let before = core.workspace(&repo).unwrap();
    // The fake agent's folder is gone, so its process cannot start.
    fs::remove_file(dir.path().join("agent")).unwrap();

    let started = core.spawn_agent(&repo, in_panel(Agent::Claude, Some(0)));

    assert!(started.is_err(), "{started:?}");
    assert_eq!(core.workspace(&repo).unwrap(), before);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        []
    );
}

#[test]
fn the_placement_is_saved_while_the_core_runs() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_fake_agents(dir.path());
    let state = dir.path().join("home/state.json");

    let session = core
        .spawn_agent(&repo, in_panel(Agent::Claude, Some(0)))
        .unwrap();

    let deadline = Instant::now() + TIMEOUT;
    loop {
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&state).unwrap_or_default()).unwrap_or_default();
        let chat = &saved["repos"][0]["main_worktree"]["workspace"]["chat"];
        if chat[0]["session"] == serde_json::to_value(session).unwrap() {
            break;
        }
        assert!(Instant::now() < deadline, "not saved: {saved}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn placing_a_session_keeps_the_panels_of_ended_sessions_in_the_saved_workspace() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_fake_agents(dir.path());
    let ended = sleeper(&core, &repo);
    let workspace = Workspace {
        chat: vec![
            panel(Agent::Claude, Some(ended), true),
            panel(Agent::Codex, None, true),
        ],
        ..arranged()
    };
    block_on(core.set_workspace(&repo, workspace, None)).unwrap();
    core.kill(ended).unwrap();
    wait_exited(&mut events, ended);

    // Panel 0 of the workspace as `workspace` returns it is the Codex one.
    let session = core
        .spawn_agent(&repo, in_panel(Agent::Codex, Some(0)))
        .unwrap();

    assert_eq!(
        core.workspace(&repo).unwrap().chat,
        [panel(Agent::Codex, Some(session), true)]
    );
    let state = dir.path().join("home/state.json");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&state).unwrap_or_default()).unwrap_or_default();
        let chat = &saved["repos"][0]["main_worktree"]["workspace"]["chat"];
        if chat[1]["session"] == serde_json::to_value(session).unwrap() {
            assert_eq!(chat[0]["session"], serde_json::to_value(ended).unwrap());
            break;
        }
        assert!(Instant::now() < deadline, "not saved: {saved}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_session_between_its_two_processes_in_a_mode_switch_keeps_its_panel() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_fake_agents(dir.path());
    let session = core
        .spawn_agent(&repo, in_panel(Agent::Codex, None))
        .unwrap();
    let placed = core.workspace(&repo).unwrap().chat;

    // Polled once, the switch kills the old process and waits for its exit. Not polled again,
    // it holds there, before the new process starts.
    let mut switching = pin!(core.set_mode(session, Mode::Chat));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(switching.as_mut().poll(&mut cx).is_pending());
    wait_exited(&mut events, session);

    assert_eq!(core.workspace(&repo).unwrap().chat, placed);
    // A start counts the panels the same way: panel 1 is the switching session's, so the new
    // session goes after it.
    let started = core
        .spawn_agent(&repo, in_panel(Agent::Claude, Some(1)))
        .unwrap();
    let mut expected = placed;
    expected.push(panel(Agent::Claude, Some(started), true));
    assert_eq!(core.workspace(&repo).unwrap().chat, expected);
}

#[test]
fn two_starts_that_overlap_each_land_in_the_panel_they_name() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_fake_agents(dir.path());
    let two = Workspace {
        chat: vec![
            panel(Agent::Claude, None, true),
            panel(Agent::Codex, None, true),
        ],
        ..arranged()
    };
    block_on(core.set_workspace(&repo, two, None)).unwrap();
    // Session ids count up, so the first start below gets the one after this.
    let before: u64 = sleeper(&core, &repo).to_string().parse().unwrap();
    let first: SessionId = serde_json::from_value((before + 1).into()).unwrap();
    // The first start opens its sidecar's log to write. A pipe there holds it until the test
    // opens the other end.
    let session_dir = dir.path().join(format!("home/sessions/{first}"));
    fs::create_dir_all(&session_dir).unwrap();
    let log = session_dir.join("sidecar.log");
    let status = Command::new("mkfifo").arg(&log).status().unwrap();
    assert!(status.success());

    thread::scope(|scope| {
        let held = scope.spawn(|| {
            let spec = AgentSpec {
                mode: Mode::Chat,
                ..in_panel(Agent::Claude, Some(1))
            };
            core.spawn_agent(&repo, spec)
        });
        // Listed once tracked, after its panel was checked.
        let deadline = Instant::now() + TIMEOUT;
        while !core
            .sessions(&repo)
            .unwrap()
            .iter()
            .any(|info| info.session == first)
        {
            assert!(Instant::now() < deadline, "the first start never began");
            thread::sleep(Duration::from_millis(5));
        }

        let second = core
            .spawn_agent(&repo, in_panel(Agent::Codex, Some(0)))
            .unwrap();
        // Opening the read end lets the first start go on.
        drop(fs::File::open(&log).unwrap());
        assert_eq!(held.join().unwrap().unwrap(), first);

        assert_eq!(
            core.workspace(&repo).unwrap().chat,
            [
                panel(Agent::Codex, Some(second), true),
                panel(Agent::Claude, Some(first), true),
            ]
        );
    });
}
