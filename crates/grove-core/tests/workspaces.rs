//! Workspaces: each agent session's agent panels, panel-area tabs and terminal flag, starting
//! from its project's profile layout and saved with the session in the state file. A subagent
//! session shows in the workspace of its top parent.

use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, AgentPanel, AgentSpec, AgentState, Core, CoreConfig, CoreError, CoreEvent, Layout, Mode,
    NewWorktree, PanelKind, ProfileSettings, RemoveOpts, RepoSettings, SessionId, Workspace,
};
use serde_json::{Value, json};
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

// A config whose Claude Code, Claude sidecar and Codex are a script that only waits, as the
// agents would.
fn config(dir: &Path) -> CoreConfig {
    let fake = dir.join("agent");
    fs::write(&fake, "#!/bin/sh\nexec sleep 1000\n").unwrap();
    let status = Command::new("chmod").arg("+x").arg(&fake).status().unwrap();
    assert!(status.success());
    let mut cfg = CoreConfig::new(dir.join("home"));
    cfg.claude_command = fake.clone();
    cfg.claude_sidecar_command = fake.clone();
    cfg.codex_command = fake;
    cfg
}

fn start(dir: &Path) -> (Core, broadcast::Receiver<CoreEvent>) {
    Core::start(config(dir)).unwrap()
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

// A new agent session in terminal mode. With no `agent` it runs the one its layout names.
fn spec(agent: Option<Agent>) -> AgentSpec {
    AgentSpec {
        agent,
        mode: Mode::Terminal,
        prompt: None,
        cols: 80,
        rows: 24,
    }
}

fn session(core: &Core, worktree: &Path, agent: Agent) -> SessionId {
    core.spawn_agent(worktree, spec(Some(agent))).unwrap()
}

fn new_worktree(core: &Core, repo: &Path, branch: &str) -> PathBuf {
    let new = NewWorktree {
        branch: branch.into(),
        display_name: None,
        issue_link: None,
        base_branch: None,
    };
    block_on(core.create_worktree(repo, new))
        .unwrap()
        .worktree
        .path
}

fn id(n: u64) -> SessionId {
    serde_json::from_value(json!(n)).unwrap()
}

// The core's reply to a panel request from agent session `caller`, sent as grove-mcp sends it,
// with the token of the caller's running process.
fn request(dir: &Path, caller: SessionId, request: Value) -> Value {
    let token = fs::read_to_string(dir.join(format!("home/sessions/{caller}/token"))).unwrap();
    let message = json!({"session": caller.to_string(), "token": token, "request": request});
    let mut stream = UnixStream::connect(dir.join("home/grove.sock")).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.write_all(message.to_string().as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    serde_json::from_slice(&reply).unwrap()
}

// A Claude subagent of `parent`, asked for as the parent's grove-mcp does, in `worktree` or
// else the parent's own.
fn subagent(dir: &Path, parent: SessionId, worktree: Option<&Path>) -> SessionId {
    let mut ask = json!({"type": "subagent_spawn", "agent": "claude", "prompt": "hi"});
    if let Some(worktree) = worktree {
        ask["worktree"] = json!(worktree);
    }
    let reply = request(dir, parent, ask);
    let session = reply["result"]["session"]
        .as_str()
        .unwrap_or_else(|| panic!("no subagent: {reply}"));
    id(session.parse().unwrap())
}

// The workspace `caller` gets from `workspace_get`.
fn workspace_get(dir: &Path, caller: SessionId) -> Value {
    let reply = request(dir, caller, json!({"type": "workspace_get"}));
    assert!(reply.get("result").is_some(), "{reply}");
    reply["result"].clone()
}

// The core's reply when `caller` sends `workspace` with `workspace_set`.
fn workspace_set(dir: &Path, caller: SessionId, workspace: &Value) -> Value {
    let set = json!({"type": "workspace_set", "workspace": workspace});
    request(dir, caller, set)
}

fn panel(agent: Agent, session: SessionId, shown: bool) -> AgentPanel {
    AgentPanel {
        agent,
        session,
        shown,
    }
}

// `chat` with the Diff and Logs tabs, Logs selected and no terminal: nothing a layout in these
// tests gives.
fn arranged(chat: Vec<AgentPanel>) -> Workspace {
    Workspace {
        chat,
        panels: vec![PanelKind::Diff, PanelKind::Logs],
        selected: Some(PanelKind::Logs),
        terminal: false,
    }
}

// grove's own layout, with `chat`.
fn built_in(chat: Vec<AgentPanel>) -> Workspace {
    Workspace {
        chat,
        panels: vec![PanelKind::Files, PanelKind::Diff],
        selected: Some(PanelKind::Files),
        terminal: true,
    }
}

// Every `WorkspaceChanged` within `window`: the session whose workspace changed, and who
// changed it.
fn workspace_changes_within(
    events: &mut broadcast::Receiver<CoreEvent>,
    window: Duration,
) -> Vec<(SessionId, Option<SessionId>)> {
    let end = Instant::now() + window;
    let mut seen = Vec::new();
    while Instant::now() < end {
        match events.try_recv() {
            Ok(CoreEvent::WorkspaceChanged { session, by }) => seen.push((session, by)),
            Ok(_) => {}
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("{e}"),
        }
    }
    seen
}

// Ends the session's process and waits until the core has marked it ended.
fn end(core: &Core, events: &mut broadcast::Receiver<CoreEvent>, session: SessionId) {
    core.kill(session).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(CoreEvent::AgentState {
                session: s,
                state: AgentState::Exited,
                ..
            }) if s == session => return,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "{session} never ended");
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

// The state file once `saved` says it holds what the test waits for.
fn wait_saved(dir: &Path, saved: impl Fn(&Value) -> bool) -> Value {
    let path = dir.join("home/state.json");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let state: Value =
            serde_json::from_slice(&fs::read(&path).unwrap_or_default()).unwrap_or_default();
        if saved(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "not saved: {state}");
        thread::sleep(Duration::from_millis(10));
    }
}

// Every agent session record in the state file, whatever its worktree.
fn saved_records(state: &Value) -> Vec<Value> {
    let mut records = Vec::new();
    for repo in state["repos"].as_array().into_iter().flatten() {
        let linked = repo["linked_worktrees"].as_array().into_iter().flatten();
        for worktree in std::iter::once(&repo["main_worktree"]).chain(linked) {
            records.extend(
                worktree["sessions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
        }
    }
    records
}

// The sessions the agent panels of `session`'s saved workspace name, in order.
fn saved_chat(state: &Value, session: SessionId) -> Option<Vec<Value>> {
    let records = saved_records(state);
    let record = records
        .iter()
        .find(|record| record["id"] == json!(session))?;
    let chat = record["workspace"]["chat"].as_array()?;
    Some(chat.iter().map(|panel| panel["session"].clone()).collect())
}

#[test]
fn a_new_session_starts_from_its_projects_profile_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    use_profile(&core, &repo, Some("Rust crate"), Layout::default());

    // The starter: chat = ["claude"], panels = ["files", "diff"], terminal = true.
    assert_eq!(core.starting_workspace(&repo).unwrap(), built_in(vec![]));
    let started = core.spawn_agent(&repo, spec(None)).unwrap();

    assert_eq!(
        core.workspace(started).unwrap(),
        built_in(vec![panel(Agent::Claude, started, true)])
    );
    // Its workspace starts with it, so nothing changed.
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        []
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
    let browser_only = |chat| Workspace {
        chat,
        panels: vec![PanelKind::Browser],
        selected: Some(PanelKind::Browser),
        terminal: true,
    };

    assert_eq!(
        core.starting_workspace(&repo).unwrap(),
        browser_only(vec![])
    );
    // A new session starts with the first agent in the list. The rest are ignored.
    let started = core.spawn_agent(&repo, spec(None)).unwrap();

    assert_eq!(
        core.workspace(started).unwrap(),
        browser_only(vec![panel(Agent::Codex, started, true)])
    );
    // An agent the caller names wins over the list.
    let named = session(&core, &repo, Agent::Claude);
    assert_eq!(
        core.workspace(named).unwrap().chat,
        [panel(Agent::Claude, named, true)]
    );
}

#[test]
fn a_repo_without_a_profile_gets_the_built_in_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());

    assert_eq!(core.starting_workspace(&repo).unwrap(), built_in(vec![]));
    let first = core.spawn_agent(&repo, spec(None)).unwrap();
    assert_eq!(
        core.workspace(first).unwrap(),
        built_in(vec![panel(Agent::Claude, first, true)])
    );

    // Only what the overrides leave unset comes from it.
    let no_tabs = Layout {
        panels: Some(Vec::new()),
        ..Layout::default()
    };
    use_profile(&core, &repo, None, no_tabs);
    assert_eq!(
        core.starting_workspace(&repo).unwrap(),
        Workspace {
            panels: Vec::new(),
            selected: None,
            ..built_in(vec![])
        }
    );
    // A session keeps the workspace it started with.
    assert_eq!(
        core.workspace(first).unwrap(),
        built_in(vec![panel(Agent::Claude, first, true)])
    );
}

// The decision on issue #63: the core does not fall back to grove's own layout. A start is
// refused, with the error that names the profile file, and leaves no session behind.
#[test]
fn a_start_is_refused_while_the_projects_profile_is_not_valid_and_leaves_no_session() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    use_profile(&core, &repo, Some("Rust crate"), Layout::default());
    let before = session(&core, &repo, Agent::Claude);
    let of_before = core.workspace(before).unwrap();
    let file = core.profile_text("Rust crate").unwrap().path;
    let valid = fs::read(&file).unwrap();
    drop(core);
    fs::write(&file, "name = \"Rust crate\"\n\n[layout]\npanels = 5\n").unwrap();
    let (core, _events) = start(dir.path());
    let names_the_file = |refused: CoreError| {
        assert!(
            matches!(&refused, CoreError::BadProfile { path, .. } if *path == file),
            "{refused:?}"
        );
        let message = refused.to_string();
        assert!(message.contains("rust-crate.toml"), "{message}");
    };

    names_the_file(core.spawn_agent(&repo, spec(None)).unwrap_err());
    names_the_file(
        core.spawn_agent(&repo, spec(Some(Agent::Codex)))
            .unwrap_err(),
    );
    names_the_file(core.starting_workspace(&repo).unwrap_err());

    // Only the session from before, which keeps the workspace it was saved with.
    let listed: Vec<SessionId> = core
        .agent_sessions()
        .into_iter()
        .map(|info| info.session)
        .collect();
    assert_eq!(listed, [before]);
    assert_eq!(core.workspace(before).unwrap(), of_before);
    let folders = fs::read_dir(dir.path().join("home/sessions"))
        .unwrap()
        .count();
    assert_eq!(folders, 1);

    // Repaired, the project starts sessions again.
    drop(core);
    fs::write(&file, valid).unwrap();
    let (core, _events) = start(dir.path());
    let after = core.spawn_agent(&repo, spec(None)).unwrap();
    assert_eq!(
        core.workspace(after).unwrap().panels,
        core.starting_workspace(&repo).unwrap().panels
    );
}

#[test]
fn two_sessions_in_one_place_keep_different_layouts_across_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let first = session(&core, &repo, Agent::Claude);
    let second = session(&core, &repo, Agent::Codex);
    let of_first = arranged(vec![panel(Agent::Claude, first, true)]);
    let of_second = Workspace {
        panels: vec![PanelKind::Browser],
        selected: None,
        terminal: true,
        chat: vec![panel(Agent::Codex, second, true)],
    };

    block_on(core.set_workspace(first, of_first.clone(), None)).unwrap();
    block_on(core.set_workspace(second, of_second.clone(), None)).unwrap();
    assert_eq!(core.workspace(first).unwrap(), of_first);
    assert_eq!(core.workspace(second).unwrap(), of_second);
    drop(core);
    let (core, _events) = start(dir.path());

    assert_eq!(core.workspace(first).unwrap(), of_first);
    assert_eq!(core.workspace(second).unwrap(), of_second);
}

// The agent's command is gone at the restart, so its session does not come back, and a new
// session may take its id. Its panel goes, so that session never shows in it.
#[test]
fn a_panel_whose_session_does_not_come_back_is_gone_after_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let codex = dir.path().join("codex");
    let mut cfg = config(dir.path());
    fs::copy(dir.path().join("agent"), &codex).unwrap();
    cfg.codex_command = codex.clone();
    let repo = new_repo(dir.path(), "repo");
    let (core, _events) = Core::start(cfg.clone()).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let parent = session(&core, &repo, Agent::Claude);
    let reply = request(
        dir.path(),
        parent,
        json!({"type": "subagent_spawn", "agent": "codex", "prompt": "hi"}),
    );
    let lost = id(reply["result"]["session"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap());
    let both = [
        panel(Agent::Claude, parent, true),
        panel(Agent::Codex, lost, true),
    ];
    assert_eq!(core.workspace(parent).unwrap().chat, both);
    wait_saved(dir.path(), |state| {
        saved_chat(state, parent) == Some(vec![json!(parent), json!(lost)])
    });
    drop(core);
    fs::remove_file(&codex).unwrap();

    let (core, _events) = Core::start(cfg).unwrap();

    assert_eq!(core.workspace(parent).unwrap().chat, both[..1]);
    assert!(matches!(
        core.workspace(lost),
        Err(CoreError::NotAnAgentSession(_))
    ));
    wait_saved(dir.path(), |state| {
        saved_chat(state, parent) == Some(vec![json!(parent)])
    });
}

#[test]
fn each_change_sends_one_workspace_changed_naming_its_session_and_who_made_it() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let agent = session(&core, &repo, Agent::Claude);
    let other = session(&core, &repo, Agent::Claude);
    let changed = arranged(vec![panel(Agent::Claude, agent, true)]);

    block_on(core.set_workspace(agent, changed.clone(), Some(agent))).unwrap();
    block_on(core.set_workspace(agent, changed, None)).unwrap();

    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(agent, Some(agent)), (agent, None)]
    );
    // The other session in the same place keeps its own.
    assert_eq!(
        core.workspace(other).unwrap(),
        built_in(vec![panel(Agent::Claude, other, true)])
    );
}

#[test]
fn undo_restores_the_workspace_from_before_the_last_agent_change() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let agent = session(&core, &repo, Agent::Claude);
    let before = arranged(vec![panel(Agent::Claude, agent, true)]);
    block_on(core.set_workspace(agent, before.clone(), None)).unwrap();
    let opened_files = Workspace {
        panels: vec![PanelKind::Diff, PanelKind::Logs, PanelKind::Files],
        selected: Some(PanelKind::Files),
        ..before.clone()
    };
    block_on(core.set_workspace(agent, opened_files.clone(), Some(agent))).unwrap();
    // The user's change after it is undone too.
    let terminal_open = Workspace {
        terminal: true,
        ..opened_files
    };
    block_on(core.set_workspace(agent, terminal_open, None)).unwrap();
    workspace_changes_within(&mut events, Duration::from_millis(100));

    block_on(core.undo_workspace(agent)).unwrap();

    assert_eq!(core.workspace(agent).unwrap(), before);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(agent, None)]
    );
    // Saved, as any change is.
    drop(core);
    let (core, _events) = start(dir.path());
    assert_eq!(core.workspace(agent).unwrap(), before);
}

#[test]
fn only_the_last_agent_change_can_be_undone_and_only_once() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let agent = session(&core, &repo, Agent::Claude);
    let second = arranged(vec![panel(Agent::Claude, agent, true)]);
    let third = Workspace {
        terminal: true,
        ..second.clone()
    };
    block_on(core.set_workspace(agent, second.clone(), Some(agent))).unwrap();
    block_on(core.set_workspace(agent, third, Some(agent))).unwrap();

    block_on(core.undo_workspace(agent)).unwrap();
    assert_eq!(core.workspace(agent).unwrap(), second);

    let err = block_on(core.undo_workspace(agent)).unwrap_err();
    assert!(
        matches!(err, CoreError::NothingToUndo(s) if s == agent),
        "{err}"
    );
    assert_eq!(core.workspace(agent).unwrap(), second);
}

#[test]
fn undo_with_two_sessions_in_one_place_restores_only_the_session_whose_notice_it_was() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let first = session(&core, &repo, Agent::Claude);
    let second = session(&core, &repo, Agent::Codex);
    let before_first = core.workspace(first).unwrap();
    let before_second = core.workspace(second).unwrap();
    let changed_first = arranged(before_first.chat.clone());
    let changed_second = arranged(before_second.chat.clone());
    block_on(core.set_workspace(first, changed_first, Some(first))).unwrap();
    block_on(core.set_workspace(second, changed_second.clone(), Some(second))).unwrap();
    workspace_changes_within(&mut events, Duration::from_millis(100));

    block_on(core.undo_workspace(first)).unwrap();

    assert_eq!(core.workspace(first).unwrap(), before_first);
    assert_eq!(core.workspace(second).unwrap(), changed_second);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(first, None)]
    );
    // The other session's change is still there to undo.
    block_on(core.undo_workspace(second)).unwrap();
    assert_eq!(core.workspace(second).unwrap(), before_second);
}

#[test]
fn undo_keeps_the_panel_of_a_subagent_started_after_the_agent_change() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let parent = session(&core, &repo, Agent::Codex);
    let before = core.workspace(parent).unwrap();
    block_on(core.set_workspace(parent, arranged(before.chat.clone()), Some(parent))).unwrap();
    // Placed in a new panel after the last one.
    let placed = subagent(dir.path(), parent, None);
    workspace_changes_within(&mut events, Duration::from_millis(100));

    block_on(core.undo_workspace(parent)).unwrap();

    assert_eq!(
        core.workspace(parent).unwrap(),
        Workspace {
            chat: vec![
                panel(Agent::Codex, parent, true),
                panel(Agent::Claude, placed, true),
            ],
            ..before
        }
    );
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(parent, None)]
    );
}

#[test]
fn undo_leaves_out_the_panel_of_a_subagent_that_ended() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let parent = session(&core, &repo, Agent::Claude);
    let before = core.workspace(parent).unwrap();
    block_on(core.set_workspace(parent, arranged(before.chat.clone()), Some(parent))).unwrap();
    let placed = subagent(dir.path(), parent, None);
    end(&core, &mut events, placed);

    block_on(core.undo_workspace(parent)).unwrap();

    assert_eq!(core.workspace(parent).unwrap(), before);
}

#[test]
fn save_to_profile_writes_the_sessions_agent_tabs_and_terminal_into_the_repos_profile() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    use_profile(&core, &repo, Some("Rust crate"), Layout::default());
    let codex = session(&core, &repo, Agent::Codex);
    let helper = subagent(dir.path(), codex, None);
    let chat = vec![
        panel(Agent::Claude, helper, true),
        panel(Agent::Codex, codex, false),
    ];
    block_on(core.set_workspace(codex, arranged(chat), None)).unwrap();
    while events.try_recv().is_ok() {}

    // From the subagent's panel it is still the workspace the user sees.
    block_on(core.save_workspace_to_profile(helper)).unwrap();

    let profile = core
        .profiles()
        .unwrap()
        .into_iter()
        .find(|p| p.name == "Rust crate")
        .unwrap();
    // `chat` names the agent a new session starts with: the one of the session the workspace
    // belongs to. The subagent's panel and the selected tab have no place in a layout.
    assert_eq!(
        profile.settings.layout,
        Layout {
            chat: Some(vec![Agent::Codex]),
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
    // So the next session starts as this one was saved.
    let next = core.spawn_agent(&repo, spec(None)).unwrap();
    assert_eq!(
        core.workspace(next).unwrap(),
        Workspace {
            selected: Some(PanelKind::Diff),
            ..arranged(vec![panel(Agent::Codex, next, true)])
        }
    );
}

#[test]
fn save_to_profile_is_refused_for_a_repo_with_no_profile() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    use_profile(&core, &repo, None, Layout::default());
    let agent = session(&core, &repo, Agent::Claude);

    let err = block_on(core.save_workspace_to_profile(agent)).unwrap_err();

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
    let agent = session(&core, &repo, Agent::Claude);
    let changed = arranged(vec![panel(Agent::Claude, agent, true)]);

    block_on(core.set_workspace(agent, changed, None)).unwrap();

    let err = block_on(core.undo_workspace(agent)).unwrap_err();
    assert!(matches!(err, CoreError::NothingToUndo(_)), "{err}");
}

#[test]
fn a_deleted_subagents_panel_goes_and_an_ended_ones_stays() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let parent = session(&core, &repo, Agent::Claude);
    let ended = subagent(dir.path(), parent, None);
    let deleted = subagent(dir.path(), parent, None);
    end(&core, &mut events, ended);
    workspace_changes_within(&mut events, Duration::from_millis(100));

    block_on(core.delete_agent_session(deleted)).unwrap();

    // An ended session keeps its panel, so resuming it changes no layout.
    let kept = [
        panel(Agent::Claude, parent, true),
        panel(Agent::Claude, ended, true),
    ];
    assert_eq!(core.workspace(parent).unwrap().chat, kept);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(parent, None)]
    );
    wait_saved(dir.path(), |state| {
        saved_chat(state, parent) == Some(vec![json!(parent), json!(ended)])
    });
}

#[test]
fn a_selected_tab_that_is_not_open_is_refused() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let agent = session(&core, &repo, Agent::Claude);
    let before = arranged(vec![panel(Agent::Claude, agent, true)]);
    block_on(core.set_workspace(agent, before.clone(), None)).unwrap();
    let workspace = Workspace {
        selected: Some(PanelKind::Browser),
        terminal: true,
        ..before.clone()
    };

    let err = block_on(core.set_workspace(agent, workspace, None)).unwrap_err();

    assert!(matches!(err, CoreError::InvalidWorkspace { .. }), "{err}");
    assert_eq!(core.workspace(agent).unwrap(), before);
}

#[test]
fn a_panel_of_a_session_outside_the_workspace_or_named_twice_is_refused() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let agent = session(&core, &repo, Agent::Claude);
    // Another session of the same place is no part of this one's workspace.
    let unrelated = session(&core, &repo, Agent::Codex);
    let before = core.workspace(agent).unwrap();
    let own = panel(Agent::Claude, agent, true);
    let refused = [
        vec![own.clone(), panel(Agent::Codex, unrelated, true)],
        vec![own.clone(), panel(Agent::Claude, id(999), true)],
        vec![own.clone(), own],
    ];

    for chat in refused {
        let err = block_on(core.set_workspace(agent, arranged(chat), None)).unwrap_err();

        assert!(matches!(err, CoreError::InvalidWorkspace { .. }), "{err}");
    }
    assert_eq!(core.workspace(agent).unwrap(), before);
}

#[test]
fn a_session_grove_does_not_list_has_no_workspace() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, _repo) = core_with_repo(dir.path());
    let nowhere = dir.path().join("nowhere");
    let nobody = id(999);

    assert!(matches!(
        core.starting_workspace(&nowhere),
        Err(CoreError::UnknownWorktree(_))
    ));
    assert!(matches!(
        core.workspace(nobody),
        Err(CoreError::NotAnAgentSession(_))
    ));
    assert!(matches!(
        block_on(core.set_workspace(nobody, arranged(vec![]), None)),
        Err(CoreError::NotAnAgentSession(_))
    ));
    assert!(matches!(
        block_on(core.undo_workspace(nobody)),
        Err(CoreError::NotAnAgentSession(_))
    ));
}

// The risk this guards: a workspace left behind under a deleted session's id, which a new
// session then picks up.
#[test]
fn a_deleted_sessions_workspace_never_reaches_a_new_session_that_gets_its_id() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let deleted = session(&core, &repo, Agent::Codex);
    let changed = arranged(vec![panel(Agent::Codex, deleted, true)]);
    block_on(core.set_workspace(deleted, changed, Some(deleted))).unwrap();

    block_on(core.delete_agent_session(deleted)).unwrap();

    assert!(matches!(
        core.workspace(deleted),
        Err(CoreError::NotAnAgentSession(_))
    ));
    let saved = wait_saved(dir.path(), |state| saved_records(state).is_empty());
    assert!(!saved.to_string().contains("workspace"), "{saved}");
    // No session is saved, so after a restart the ids start over.
    drop(core);
    let (core, _events) = start(dir.path());
    let reused = session(&core, &repo, Agent::Claude);

    assert_eq!(reused, deleted);
    assert_eq!(
        core.workspace(reused).unwrap(),
        built_in(vec![panel(Agent::Claude, reused, true)])
    );
    assert!(matches!(
        block_on(core.undo_workspace(reused)),
        Err(CoreError::NothingToUndo(_))
    ));
}

#[test]
fn a_forgotten_repo_added_again_starts_from_the_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    let fresh = core.starting_workspace(&repo).unwrap();
    let agent = session(&core, &repo, Agent::Claude);
    let changed = arranged(vec![panel(Agent::Claude, agent, true)]);
    block_on(core.set_workspace(agent, changed, Some(agent))).unwrap();

    block_on(core.remove_worktree(&repo, RemoveOpts::ForgetRepo)).unwrap();
    block_on(core.add_repo(&repo)).unwrap();

    assert_eq!(core.starting_workspace(&repo).unwrap(), fresh);
    assert!(matches!(
        core.workspace(agent),
        Err(CoreError::NotAnAgentSession(_))
    ));
    let saved = wait_saved(dir.path(), |state| state["repos"][0].is_object());
    assert!(!saved.to_string().contains("workspace"), "{saved}");
}

#[test]
fn removing_a_worktree_leaves_no_workspace_for_its_sessions() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let feature = new_worktree(&core, &repo, "feature");
    let parent = session(&core, &repo, Agent::Claude);
    // One session of its own in the worktree, and one subagent of a session elsewhere.
    let own = session(&core, &feature, Agent::Codex);
    let visiting = subagent(dir.path(), parent, Some(&feature));
    let changed = arranged(vec![panel(Agent::Codex, own, true)]);
    block_on(core.set_workspace(own, changed, None)).unwrap();
    assert_eq!(core.workspace(parent).unwrap().chat.len(), 2);
    workspace_changes_within(&mut events, Duration::from_millis(100));
    let delete = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };

    block_on(core.remove_worktree(&feature, delete)).unwrap();

    for gone in [own, visiting] {
        assert!(matches!(
            core.workspace(gone),
            Err(CoreError::NotAnAgentSession(_))
        ));
    }
    // The subagent's panel left the workspace of its top parent, which says so.
    assert_eq!(
        core.workspace(parent).unwrap().chat,
        [panel(Agent::Claude, parent, true)]
    );
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(parent, None)]
    );
    let saved = wait_saved(dir.path(), |state| {
        saved_chat(state, parent) == Some(vec![json!(parent)])
    });
    let records = saved_records(&saved);
    assert_eq!(records.len(), 1, "{saved}");
    assert_eq!(records[0]["id"], json!(parent));
    // Made again at the same path, it starts from the layout.
    let again = new_worktree(&core, &repo, "feature");
    assert_eq!(again, feature);
    assert_eq!(core.starting_workspace(&feature).unwrap(), built_in(vec![]));
}

// The state file a grove from before version 10 left: `sessions` in the repo's main worktree,
// whose workspace `workspace` is, and a linked worktree with a workspace and no session.
fn state_before_version_10(dir: &Path, version: u32, workspace: Value, sessions: Value) -> PathBuf {
    let repo = new_repo(dir, "repo");
    let linked = dir.canonicalize().unwrap().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let worktree = |path: &Path, branch: &str| {
        json!({"path": path, "branch": branch, "display_name": branch, "issue_link": null,
               "base_branch": "main"})
    };
    let mut main = worktree(&repo, "main");
    main["workspace"] = workspace;
    main["sessions"] = sessions;
    let mut with_no_session = worktree(&linked, "linked");
    with_no_session["workspace"] = json!({
        "chat": [{"agent": "codex", "shown": true}],
        "panels": ["browser"], "selected": "browser", "terminal": false,
    });
    let state = json!({"version": version, "repos": [{
        "main_worktree": main, "linked_worktrees": [with_no_session],
        "profile": null, "overrides": {},
    }]});
    let home = dir.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("state.json"), state.to_string()).unwrap();
    linked
}

fn agent_record(session: u64, agent: &str) -> Value {
    json!({"kind": "agent", "id": session, "agent": agent, "mode": "terminal",
           "resume_id": null, "cols": 80, "rows": 24})
}

// What the migration to version 10 leaves in the state file: no workspace on a worktree, and
// one on each agent session.
fn assert_saved_as_version_10(dir: &Path, sessions: &[SessionId]) {
    let all_saved = |state: &Value| sessions.iter().all(|&s| saved_chat(state, s).is_some());
    let saved = wait_saved(dir, |state| state["version"] == 10 && all_saved(state));
    let repo = &saved["repos"][0];
    assert!(repo["main_worktree"].get("workspace").is_none(), "{saved}");
    assert!(
        repo["linked_worktrees"][0].get("workspace").is_none(),
        "{saved}"
    );
    for &session in sessions {
        assert_eq!(
            saved_chat(&saved, session),
            Some(vec![json!(session)]),
            "{saved}"
        );
    }
}

// Version 8 kept only the sessions that ran. Both ran in one worktree and shared its workspace,
// which also had a panel with no session yet.
#[test]
fn a_version_8_state_file_gives_each_session_of_a_worktree_a_copy_of_its_tabs() {
    let _serial = serial();
    let dir = short_temp_dir();
    let workspace = json!({
        "chat": [
            {"agent": "claude", "shown": true, "session": 1},
            {"agent": "claude", "shown": true},
            {"agent": "codex", "shown": false, "session": 2},
        ],
        "panels": ["diff", "logs"], "selected": "logs", "terminal": false,
    });
    let sessions = json!([agent_record(1, "claude"), agent_record(2, "codex")]);
    let linked = state_before_version_10(dir.path(), 8, workspace, sessions);

    let (core, _events) = start(dir.path());

    // Each gets the tabs, the selected tab and the terminal flag, and its own panel, shown.
    assert_eq!(
        core.workspace(id(1)).unwrap(),
        arranged(vec![panel(Agent::Claude, id(1), true)])
    );
    assert_eq!(
        core.workspace(id(2)).unwrap(),
        arranged(vec![panel(Agent::Codex, id(2), true)])
    );
    // The workspace of the worktree with no session is dropped.
    assert_eq!(core.starting_workspace(&linked).unwrap(), built_in(vec![]));
    assert_saved_as_version_10(dir.path(), &[id(1), id(2)]);
}

// Version 9 kept ended sessions too. An ended one gets its copy like any other, and a session
// whose worktree had no saved workspace starts from the layout.
#[test]
fn a_version_9_state_file_gives_ended_sessions_their_copy_and_drops_a_workspace_with_no_session() {
    let _serial = serial();
    let dir = short_temp_dir();
    let workspace = json!({
        "chat": [{"agent": "claude", "shown": true}],
        "panels": ["diff", "logs"], "selected": "logs", "terminal": false,
    });
    let mut ended = agent_record(4, "codex");
    ended["ended"] = json!(true);
    ended["title"] = json!("fix the bug");
    let sessions = json!([agent_record(3, "claude"), ended]);
    let linked = state_before_version_10(dir.path(), 9, workspace, sessions);

    let (core, _events) = start(dir.path());

    assert_eq!(
        core.workspace(id(3)).unwrap(),
        arranged(vec![panel(Agent::Claude, id(3), true)])
    );
    assert_eq!(
        core.workspace(id(4)).unwrap(),
        arranged(vec![panel(Agent::Codex, id(4), true)])
    );
    assert_eq!(core.starting_workspace(&linked).unwrap(), built_in(vec![]));
    // A session started there now gets the layout, not the dropped workspace.
    let new = session(&core, &linked, Agent::Claude);
    assert_eq!(
        core.workspace(new).unwrap(),
        built_in(vec![panel(Agent::Claude, new, true)])
    );
    assert_saved_as_version_10(dir.path(), &[id(3), id(4), new]);
}

// A file from before version 10 whose one workspace showed a session, its subagent, the subagent
// of that one and a second subagent that had no panel. Review thread 1 of PR #124.
fn a_subagents_panel_moves_to_its_top_parents_workspace(version: u32) {
    let dir = short_temp_dir();
    let workspace = json!({
        "chat": [
            {"agent": "codex", "shown": true, "session": 3},
            {"agent": "claude", "shown": false, "session": 1},
            {"agent": "codex", "shown": false, "session": 2},
        ],
        "panels": ["diff", "logs"], "selected": "logs", "terminal": false,
    });
    let subagent = |session: u64, parent: u64| {
        let mut record = agent_record(session, "codex");
        record["parent"] = json!(parent);
        record
    };
    let sessions = json!([
        agent_record(1, "claude"),
        subagent(2, 1),
        subagent(3, 2),
        subagent(4, 1),
    ]);
    state_before_version_10(dir.path(), version, workspace, sessions);

    let (core, _events) = start(dir.path());

    // The top parent's own panel first, shown. Then each subagent's in the order the old
    // workspace had them, shown or hidden as it was, and the one that had no panel last, hidden.
    let of_parent = arranged(vec![
        panel(Agent::Claude, id(1), true),
        panel(Agent::Codex, id(3), true),
        panel(Agent::Codex, id(2), false),
        panel(Agent::Codex, id(4), false),
    ]);
    for session in [1, 2, 3, 4] {
        assert_eq!(core.workspace(id(session)).unwrap(), of_parent, "{session}");
    }
    let all = json!([1, 3, 2, 4]);
    let saved = wait_saved(dir.path(), |state| {
        state["version"] == 10 && saved_chat(state, id(1)).map(Value::from) == Some(all.clone())
    });
    // Each subagent keeps a copy with its own panel, for when its parent is deleted.
    for session in [2, 3, 4] {
        assert_eq!(
            saved_chat(&saved, id(session)),
            Some(vec![json!(session)]),
            "{saved}"
        );
    }
}

#[test]
fn a_version_9_state_file_keeps_each_subagents_panel_in_its_top_parents_workspace() {
    let _serial = serial();
    a_subagents_panel_moves_to_its_top_parents_workspace(9);
}

#[test]
fn a_version_8_state_file_keeps_each_subagents_panel_in_its_top_parents_workspace() {
    let _serial = serial();
    a_subagents_panel_moves_to_its_top_parents_workspace(8);
}

#[test]
fn a_state_file_from_before_workspaces_starts_each_session_from_the_layout() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let cfg = config(dir.path());
    fs::create_dir(&cfg.home).unwrap();
    // As a version 3 grove wrote it.
    let v3 = format!(
        r#"{{"version": 3, "repos": [{{"main_worktree": {{"path": "{}", "branch": "main",
        "display_name": "main", "issue_link": null, "base_branch": "main"}},
        "linked_worktrees": [], "profile": "Rust crate", "overrides": {{}}}}]}}"#,
        repo.display()
    );
    fs::write(cfg.home.join("state.json"), v3).unwrap();

    let (core, _events) = Core::start(cfg).unwrap();

    assert_eq!(
        core.starting_workspace(&repo).unwrap().panels,
        [PanelKind::Files, PanelKind::Diff]
    );
    let agent = session(&core, &repo, Agent::Claude);
    let changed = arranged(vec![panel(Agent::Claude, agent, true)]);
    block_on(core.set_workspace(agent, changed, None)).unwrap();
    wait_saved(dir.path(), |state| state["version"] == 10);
}

#[test]
fn a_subagent_gets_a_shown_panel_beside_its_top_parent_with_one_workspace_changed() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let parent = session(&core, &repo, Agent::Codex);
    // Another session of the place. Its panel never joins the parent's.
    let unrelated = session(&core, &repo, Agent::Claude);

    let helper = subagent(dir.path(), parent, None);

    let chat = [
        panel(Agent::Codex, parent, true),
        panel(Agent::Claude, helper, true),
    ];
    assert_eq!(core.workspace(parent).unwrap().chat, chat);
    // A subagent has no workspace of its own to show: it shows in its top parent's.
    assert_eq!(
        core.workspace(helper).unwrap(),
        core.workspace(parent).unwrap()
    );
    assert_eq!(
        core.workspace(unrelated).unwrap().chat,
        [panel(Agent::Claude, unrelated, true)]
    );
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(parent, None)]
    );
}

#[test]
fn a_subagent_that_fails_to_start_leaves_the_workspace_unchanged() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let parent = session(&core, &repo, Agent::Claude);
    let before = core.workspace(parent).unwrap();
    // The fake agent is gone, so no new process can start. The parent's runs on.
    fs::remove_file(dir.path().join("agent")).unwrap();

    let reply = request(
        dir.path(),
        parent,
        json!({"type": "subagent_spawn", "agent": "claude", "prompt": "hi"}),
    );
    let started = core.spawn_agent(&repo, spec(None));

    assert!(reply.get("error").is_some(), "{reply}");
    assert!(started.is_err(), "{started:?}");
    assert_eq!(core.workspace(parent).unwrap(), before);
    assert_eq!(core.agent_sessions().len(), 1);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        []
    );
}

#[test]
fn a_new_sessions_workspace_and_a_subagents_panel_are_saved_while_the_core_runs() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());

    let parent = session(&core, &repo, Agent::Claude);
    let saved = wait_saved(dir.path(), |state| saved_chat(state, parent).is_some());
    assert_eq!(saved_chat(&saved, parent), Some(vec![json!(parent)]));

    let helper = subagent(dir.path(), parent, None);
    wait_saved(dir.path(), |state| {
        saved_chat(state, parent) == Some(vec![json!(parent), json!(helper)])
    });
}

#[test]
fn an_agents_workspace_set_changes_only_its_own_sessions_workspace() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let agent = session(&core, &repo, Agent::Claude);
    // In the same place, with a workspace of its own.
    let other = session(&core, &repo, Agent::Codex);
    let others_before = core.workspace(other).unwrap();
    let mut workspace = workspace_get(dir.path(), agent);
    assert_eq!(
        workspace,
        json!({
            "chat": [{"agent": "claude", "session": agent, "shown": true}],
            "panels": ["files", "diff"], "selected": "files", "terminal": true,
        })
    );
    workspace["panels"] = json!(["files", "diff", "logs"]);
    workspace["selected"] = json!("logs");

    let reply = workspace_set(dir.path(), agent, &workspace);

    assert_eq!(reply, json!({"result": {}}));
    assert_eq!(
        core.workspace(agent).unwrap(),
        Workspace {
            panels: vec![PanelKind::Files, PanelKind::Diff, PanelKind::Logs],
            selected: Some(PanelKind::Logs),
            ..built_in(vec![panel(Agent::Claude, agent, true)])
        }
    );
    assert_eq!(core.workspace(other).unwrap(), others_before);
    assert_eq!(
        workspace_get(dir.path(), other)["panels"],
        json!(["files", "diff"])
    );
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(agent, Some(agent))]
    );
}

#[test]
fn an_agents_workspace_set_that_drops_the_panel_of_a_running_session_is_refused() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let parent = session(&core, &repo, Agent::Claude);
    let helper = subagent(dir.path(), parent, None);
    // A session of the same place that is no part of this workspace.
    let unrelated = session(&core, &repo, Agent::Codex);
    let before = core.workspace(parent).unwrap();
    let workspace = workspace_get(dir.path(), parent);
    workspace_changes_within(&mut events, Duration::from_millis(100));
    let without = |session: SessionId| {
        let mut dropped = workspace.clone();
        let chat = dropped["chat"].as_array_mut().unwrap();
        chat.retain(|panel| panel["session"] != json!(session));
        dropped
    };
    let mut names_unrelated = workspace.clone();
    names_unrelated["chat"].as_array_mut().unwrap().push(json!(
        {"agent": "codex", "session": unrelated, "shown": true}
    ));
    let refused = [
        (
            without(helper),
            format!("it drops the panel of agent session {helper}, which is running"),
        ),
        (
            without(parent),
            format!("it drops the panel of agent session {parent}, which is running"),
        ),
        (
            names_unrelated,
            format!(
                "agent session {unrelated} is not this workspace's session or a subagent of it"
            ),
        ),
    ];

    for (workspace, reason) in refused {
        let reply = workspace_set(dir.path(), parent, &workspace);

        let error = reply["error"].as_str().unwrap_or_default();
        assert!(error.contains(&reason), "{workspace}: {reply}");
    }
    assert_eq!(core.workspace(parent).unwrap(), before);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        []
    );

    // An ended session runs nothing, so its panel may go.
    end(&core, &mut events, helper);
    let reply = workspace_set(dir.path(), parent, &without(helper));
    assert_eq!(reply, json!({"result": {}}));
    assert_eq!(
        core.workspace(parent).unwrap().chat,
        [panel(Agent::Claude, parent, true)]
    );
}

#[test]
fn a_subagent_of_a_subagent_reads_and_changes_its_top_parents_workspace() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let top = session(&core, &repo, Agent::Codex);
    let middle = subagent(dir.path(), top, None);
    let bottom = subagent(dir.path(), middle, None);
    let before = core.workspace(top).unwrap();
    assert_eq!(
        before.chat,
        [
            panel(Agent::Codex, top, true),
            panel(Agent::Claude, middle, true),
            panel(Agent::Claude, bottom, true),
        ]
    );
    workspace_changes_within(&mut events, Duration::from_millis(100));

    let mut workspace = workspace_get(dir.path(), bottom);
    assert_eq!(workspace, workspace_get(dir.path(), top));
    workspace["terminal"] = json!(false);
    let reply = workspace_set(dir.path(), bottom, &workspace);

    assert_eq!(reply, json!({"result": {}}));
    let changed = Workspace {
        terminal: false,
        ..before.clone()
    };
    for session in [top, middle, bottom] {
        assert_eq!(core.workspace(session).unwrap(), changed);
    }
    // The workspace is the top parent's, and the change names the subagent that made it.
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(top, Some(bottom))]
    );
    // Undo from the subagent's notice restores that same workspace.
    block_on(core.undo_workspace(bottom)).unwrap();
    assert_eq!(core.workspace(top).unwrap(), before);
}

#[test]
fn a_subagent_in_another_worktree_changes_its_top_parents_workspace() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let feature = new_worktree(&core, &repo, "feature");
    // A session of the subagent's own worktree, which the change must leave alone.
    let in_feature = session(&core, &feature, Agent::Claude);
    let parent = session(&core, &repo, Agent::Claude);
    let helper = subagent(dir.path(), parent, Some(&feature));
    let others_before = core.workspace(in_feature).unwrap();
    workspace_changes_within(&mut events, Duration::from_millis(100));

    let mut workspace = workspace_get(dir.path(), helper);
    assert_eq!(workspace, workspace_get(dir.path(), parent));
    workspace["panels"] = json!(["logs"]);
    workspace["selected"] = json!("logs");
    let reply = workspace_set(dir.path(), helper, &workspace);

    assert_eq!(reply, json!({"result": {}}));
    assert_eq!(core.workspace(parent).unwrap().panels, [PanelKind::Logs]);
    assert_eq!(core.workspace(in_feature).unwrap(), others_before);
    assert_eq!(
        workspace_changes_within(&mut events, Duration::from_millis(300)),
        [(parent, Some(helper))]
    );
}

#[test]
fn a_session_between_its_two_processes_in_a_mode_switch_keeps_its_workspace() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events, repo) = core_with_repo(dir.path());
    let session = session(&core, &repo, Agent::Codex);
    let helper = subagent(dir.path(), session, None);
    let before = core.workspace(session).unwrap();

    // Polled once, the switch kills the old process and waits for its exit. Not polled again,
    // it holds there, before the new process starts.
    let mut switching = pin!(core.set_mode(session, Mode::Chat));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(switching.as_mut().poll(&mut cx).is_pending());
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(CoreEvent::SessionExited { session: s, .. }) if s == session => break,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "no SessionExited");
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }

    assert_eq!(core.workspace(session).unwrap(), before);
    // An agent's change still counts it as running, so its panel may not go.
    let mut dropped = workspace_get(dir.path(), helper);
    let chat = dropped["chat"].as_array_mut().unwrap();
    chat.retain(|panel| panel["session"] != json!(session));
    let reply = workspace_set(dir.path(), helper, &dropped);
    assert!(reply.get("error").is_some(), "{reply}");
}

#[test]
fn two_starts_that_overlap_each_get_their_own_workspace() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events, repo) = core_with_repo(dir.path());
    // Session ids count up, so the first start below gets the one after this.
    let before: u64 = session(&core, &repo, Agent::Claude)
        .to_string()
        .parse()
        .unwrap();
    let first = id(before + 1);
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
                ..spec(Some(Agent::Claude))
            };
            core.spawn_agent(&repo, spec)
        });
        // Listed once tracked.
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

        let second = session(&core, &repo, Agent::Codex);
        // Opening the read end lets the first start go on.
        drop(fs::File::open(&log).unwrap());
        assert_eq!(held.join().unwrap().unwrap(), first);

        assert_eq!(
            core.workspace(first).unwrap().chat,
            [panel(Agent::Claude, first, true)]
        );
        assert_eq!(
            core.workspace(second).unwrap().chat,
            [panel(Agent::Codex, second, true)]
        );
    });
}
