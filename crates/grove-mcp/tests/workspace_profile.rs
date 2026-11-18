//! Workspace and profile tools: an agent session reads and changes its own worktree's workspace
//! and its repo's profile through grove-mcp, and each change names it.

mod common;

use common::*;
use grove_core::{AgentDefaults, Layout, PanelKind, Workspace};

/// A profile with a default model and the Files and Diff tabs, and no setup script.
fn plain() -> ProfileSettings {
    ProfileSettings {
        agent: AgentDefaults {
            model: Some("opus".into()),
            effort: Some("medium".into()),
            ..AgentDefaults::default()
        },
        layout: Layout {
            panels: Some(vec![PanelKind::Files, PanelKind::Diff]),
            ..Layout::default()
        },
        ..ProfileSettings::default()
    }
}

/// The workspace as `workspace_get` returns it.
fn workspace_of(client: &mut McpClient) -> Value {
    let (text, is_error) = client.call("workspace_get", json!({}));
    assert!(!is_error, "{text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// `workspace` in the shape the tools speak.
fn as_json(workspace: &Workspace) -> Value {
    let chat: Vec<Value> = workspace
        .chat
        .iter()
        .map(|panel| json!({"agent": panel.agent, "session": panel.session, "shown": panel.shown}))
        .collect();
    json!({
        "chat": chat,
        "panels": workspace.panels,
        "selected": workspace.selected,
        "terminal": workspace.terminal,
    })
}

/// A second repo the setup's core tracks, with its main worktree.
fn other_repo(s: &Setup) -> PathBuf {
    let repo = s.dir.path().join("other");
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    block_on(s.core.add_repo(&repo)).unwrap();
    block_on(s.core.list_worktrees())
        .into_iter()
        .find(|w| w.is_main && w.path != s.worktree)
        .unwrap()
        .path
}

#[test]
fn an_agent_opens_logs_and_the_change_names_it_until_undone() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (session, mut client) = agent_with_client(&s, Agent::Claude);
    let before = s.core.workspace(&s.worktree).unwrap();
    let mut workspace = workspace_of(&mut client);
    assert_eq!(workspace, as_json(&before));
    workspace["panels"]
        .as_array_mut()
        .unwrap()
        .push(json!("logs"));
    workspace["selected"] = json!("logs");
    s.drain_events();

    let (text, is_error) = client.call("workspace_set", json!({ "workspace": workspace }));

    assert!(!is_error, "{text}");
    assert_eq!(text, "Changed the workspace.");
    let CoreEvent::WorkspaceChanged { worktree, by } = s.next_change() else {
        panic!("not a workspace change");
    };
    assert_eq!((worktree, by), (s.worktree.clone(), Some(session)));
    let after = s.core.workspace(&s.worktree).unwrap();
    assert_eq!(
        after.panels,
        [PanelKind::Files, PanelKind::Diff, PanelKind::Logs]
    );
    assert_eq!(after.selected, Some(PanelKind::Logs));
    assert_eq!(workspace_of(&mut client), workspace);

    block_on(s.core.undo_workspace(&s.worktree)).unwrap();
    assert_eq!(s.core.workspace(&s.worktree).unwrap(), before);
}

#[test]
fn an_agent_changes_its_profiles_default_model_and_the_next_worktree_starts_from_it() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    s.use_profile(&s.worktree, "Plain", plain());
    let (session, mut client) = agent_with_client(&s, Agent::Claude);
    s.drain_events();

    let changes = json!({"agent": {"model": "sonnet"}, "layout": {"panels": ["files", "logs"]}});
    let (text, is_error) = client.call("profile_set", json!({ "changes": changes }));

    assert!(!is_error, "{text}");
    assert_eq!(text, "Changed profile Plain.");
    let CoreEvent::ProfilesChanged { by } = s.next_change() else {
        panic!("not a profile change");
    };
    assert_eq!(by, Some(session));
    assert!(!s.changed_by_now());
    let file = fs::read_to_string(s.profile_file("Plain")).unwrap();
    assert!(file.contains("model = \"sonnet\""), "{file}");
    // What the change leaves out stays.
    assert!(file.contains("effort = \"medium\""), "{file}");
    let profile = s.core.profiles().unwrap();
    let profile = profile.iter().find(|p| p.name == "Plain").unwrap();
    assert_eq!(profile.settings.agent.model.as_deref(), Some("sonnet"));
    let other = s.other_worktree();
    assert_eq!(
        s.core.workspace(&other).unwrap().panels,
        [PanelKind::Files, PanelKind::Logs]
    );

    let (text, is_error) = client.call("profile_get", json!({}));
    assert!(!is_error, "{text}");
    let got: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(got["name"], "Plain");
    assert_eq!(got["settings"]["agent"]["model"], "sonnet");
    assert_eq!(got["settings"]["agent"]["effort"], "medium");
}

#[test]
fn an_unknown_panel_kind_or_field_is_refused_and_nothing_changes() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    s.use_profile(&s.worktree, "Plain", plain());
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    let workspace = workspace_of(&mut client);
    let before = s.core.workspace(&s.worktree).unwrap();
    let profile_before = fs::read(s.profile_file("Plain")).unwrap();
    s.drain_events();
    let mut widget = workspace.clone();
    widget["panels"] = json!(["files", "widget"]);
    let mut colour = workspace.clone();
    colour["colour"] = json!("green");
    let mut chat_field = workspace.clone();
    chat_field["chat"][0]["width"] = json!(300);
    let cases = [
        (
            "workspace_set",
            json!({ "workspace": widget }),
            "unknown variant `widget`",
        ),
        (
            "workspace_set",
            json!({ "workspace": colour }),
            "unknown field `colour`",
        ),
        (
            "workspace_set",
            json!({ "workspace": chat_field }),
            "unknown field `width`",
        ),
        (
            "profile_set",
            json!({"changes": {"agent": {"modle": "sonnet"}}}),
            "unknown field `modle`",
        ),
        (
            "profile_set",
            json!({"changes": {"layout": {"panels": ["widget"]}}}),
            "unknown variant `widget`",
        ),
        // A profile's name is not one of its settings.
        (
            "profile_set",
            json!({"changes": {"name": "Other"}}),
            "unknown field `name`",
        ),
    ];

    for (tool, arguments, reason) in cases {
        let (text, is_error) = client.call(tool, arguments);

        assert!(is_error, "{tool}: {text}");
        assert!(text.contains(reason), "{tool}: {text}");
    }
    assert_eq!(s.core.workspace(&s.worktree).unwrap(), before);
    assert_eq!(fs::read(s.profile_file("Plain")).unwrap(), profile_before);
    assert!(!s.changed_by_now());
}

#[test]
fn a_session_changes_only_its_own_worktrees_workspace_and_its_own_repos_profile() {
    let s = setup(json!([{"wait_input": "\r"}]));
    s.use_profile(&s.worktree, "Plain", plain());
    let other_repo = other_repo(&s);
    s.use_profile(&other_repo, "Other", plain());
    let other_worktree = s.other_worktree();
    let (session, mut client) = agent_with_client(&s, Agent::Claude);
    let token = s.token("claude", session);
    let others_before = [
        s.core.workspace(&other_repo).unwrap(),
        s.core.workspace(&other_worktree).unwrap(),
    ];
    let other_profile = fs::read(s.profile_file("Other")).unwrap();
    let mut workspace = workspace_of(&mut client);
    workspace["terminal"] = json!(false);

    // Naming another worktree or profile in the request changes nothing about where it acts.
    let requests = [
        json!({"type": "workspace_set", "workspace": workspace, "worktree": other_worktree}),
        json!({"type": "profile_set", "changes": {"setup": "true"}, "profile": "Other",
               "repo": other_repo}),
    ];
    for request in requests {
        let message = json!({"session": session.to_string(), "token": token, "request": request});
        let reply = panel_request(&s.cfg.socket, &message);
        assert!(reply.get("result").is_some(), "{reply}");
    }

    assert!(!s.core.workspace(&s.worktree).unwrap().terminal);
    assert_eq!(
        [
            s.core.workspace(&other_repo).unwrap(),
            s.core.workspace(&other_worktree).unwrap(),
        ],
        others_before
    );
    let plain = fs::read_to_string(s.profile_file("Plain")).unwrap();
    assert!(plain.contains("setup = \"true\""), "{plain}");
    assert_eq!(fs::read(s.profile_file("Other")).unwrap(), other_profile);
}

#[test]
fn a_workspace_that_drops_a_running_sessions_panel_or_names_another_session_is_refused() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (session, mut client) = agent_with_client(&s, Agent::Claude);
    let other = s.other_worktree();
    let elsewhere = s
        .core
        .spawn_agent(
            &other,
            AgentSpec {
                agent: Agent::Codex,
                mode: Mode::Terminal,
                prompt: None,
                cols: 100,
                rows: 30,
                panel: None,
            },
        )
        .unwrap();
    let workspace = workspace_of(&mut client);
    let before = s.core.workspace(&s.worktree).unwrap();
    s.drain_events();
    let own = json!(session);
    let mut dropped = workspace.clone();
    dropped["chat"]
        .as_array_mut()
        .unwrap()
        .retain(|panel| panel["session"] != own);
    let mut twice = workspace.clone();
    let own_panel = json!({"agent": "claude", "session": own, "shown": true});
    twice["chat"].as_array_mut().unwrap().push(own_panel);
    let mut wrong_agent = workspace.clone();
    for panel in wrong_agent["chat"].as_array_mut().unwrap() {
        if panel["session"] == own {
            panel["agent"] = json!("codex");
        }
    }
    let with = |session: Value| {
        let mut named = workspace.clone();
        let panel = json!({"agent": "codex", "session": session, "shown": true});
        named["chat"].as_array_mut().unwrap().push(panel);
        named
    };
    let cases = [
        (
            dropped,
            format!("it drops the panel of agent session {session}"),
        ),
        (twice, format!("it names agent session {session} twice")),
        (
            wrong_agent,
            format!("agent session {session} runs claude, not codex"),
        ),
        (
            with(json!(elsewhere)),
            format!("agent session {elsewhere} is not running in this worktree"),
        ),
        (
            with(json!(999)),
            "agent session 999 is not running in this worktree".into(),
        ),
    ];

    for (workspace, reason) in cases {
        let (text, is_error) = client.call("workspace_set", json!({ "workspace": workspace }));

        assert!(is_error, "{workspace}: {text}");
        assert!(text.contains(&reason), "{text}");
    }
    assert_eq!(s.core.workspace(&s.worktree).unwrap(), before);
    assert!(!s.changed_by_now());

    // Hiding it keeps the session's panel, so that is allowed.
    let mut hidden = workspace.clone();
    for panel in hidden["chat"].as_array_mut().unwrap() {
        panel["shown"] = json!(false);
    }
    let (text, is_error) = client.call("workspace_set", json!({ "workspace": hidden }));
    assert!(!is_error, "{text}");
}

#[test]
fn a_profile_broken_on_disk_is_reported_and_never_overwritten() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    s.use_profile(&s.worktree, "Plain", plain());
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    s.drain_events();
    let path = s.profile_file("Plain");
    let broken = "name = \"Plain\"\n[agent\n";
    let temp = path.with_file_name(".plain.toml.test");
    fs::write(&temp, broken).unwrap();
    fs::rename(&temp, &path).unwrap();
    // The watcher has read it.
    let CoreEvent::ProfilesChanged { by: None } = s.next_change() else {
        panic!("not the watcher's change");
    };

    for (tool, arguments) in [
        ("profile_get", json!({})),
        (
            "profile_set",
            json!({"changes": {"agent": {"model": "sonnet"}}}),
        ),
    ] {
        let (text, is_error) = client.call(tool, arguments);

        assert!(is_error, "{tool}: {text}");
        assert!(text.contains("plain.toml is not valid"), "{tool}: {text}");
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    assert!(!s.changed_by_now());
}

#[test]
fn a_repo_that_uses_no_profile_has_none_to_get_or_set() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    s.drain_events();

    for (tool, arguments) in [
        ("profile_get", json!({})),
        (
            "profile_set",
            json!({"changes": {"agent": {"model": "sonnet"}}}),
        ),
    ] {
        let (text, is_error) = client.call(tool, arguments);

        assert!(is_error, "{tool}: {text}");
        assert!(text.contains("uses no profile"), "{tool}: {text}");
    }
    assert!(!s.changed_by_now());
}
