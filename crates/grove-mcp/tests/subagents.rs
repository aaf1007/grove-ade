//! Subagents: an agent session asks grove-mcp to start one, and grove starts it itself as an
//! agent session in chat mode (ADR 0006, 0013). The fake Claude Code and fake Codex start the
//! real grove-mcp from the MCP config grove passes them, and call its tool as an agent would.

mod common;

use common::*;
use grove_core::SessionKind;
use tokio::sync::mpsc::unbounded_channel;

/// A tool call step for the parent.
fn spawn_step(arguments: Value) -> Value {
    json!({"mcp_call": "grove", "tool": "subagent_spawn", "arguments": arguments})
}

fn assert_spawns_a_subagent(parent_agent: Agent, parent_fake: &str, child: Agent) {
    let name = match child {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
    };
    let mut s = setup(json!([
        spawn_step(json!({"agent": name, "prompt": "write the tests"})),
        {"wait_input": "\r"},
    ]));

    let parent = s.spawn_parent(parent_agent);

    let (asked_by, panel) = s.next_panel_request();
    let PanelSpec::Agent { session, worktree } = panel else {
        panic!("not an agent panel");
    };
    assert_eq!(asked_by, Some(parent));
    assert_ne!(session, parent);
    assert_eq!(worktree, s.worktree);
    let (text, is_error) = tool_text(&s.tool_reply(parent_fake));
    assert!(!is_error, "{text}");
    assert_eq!(
        text,
        format!(
            "Started subagent session {session} in {}.",
            s.worktree.display()
        )
    );
    // A real agent session in chat mode: the core holds its chat.
    assert!(s.core.subscribe(session, unbounded_channel().0).is_ok());
    // The core lists it with its parent. The parent, started as any session is, has none.
    let listed = s.core.sessions(&s.worktree).unwrap();
    let kinds: Vec<_> = listed
        .into_iter()
        .map(|info| (info.session, info.kind))
        .collect();
    let agent_of = |agent, parent| SessionKind::Agent {
        agent,
        mode: Mode::Chat,
        parent,
    };
    assert_eq!(
        kinds,
        [
            (
                parent,
                SessionKind::Agent {
                    agent: parent_agent,
                    mode: Mode::Terminal,
                    parent: None,
                }
            ),
            (session, agent_of(child, Some(parent))),
        ]
    );
}

#[test]
fn claude_code_spawns_a_claude_subagent_through_grove_mcp() {
    assert_spawns_a_subagent(Agent::Claude, "claude", Agent::Claude);
}

#[test]
fn codex_spawns_a_codex_subagent_through_grove_mcp() {
    assert_spawns_a_subagent(Agent::Codex, "codex", Agent::Codex);
}

#[test]
fn claude_code_spawns_a_codex_subagent_through_grove_mcp() {
    assert_spawns_a_subagent(Agent::Claude, "claude", Agent::Codex);
}

#[test]
fn codex_spawns_a_claude_subagent_through_grove_mcp() {
    assert_spawns_a_subagent(Agent::Codex, "codex", Agent::Claude);
}

#[test]
fn a_parent_in_chat_mode_spawns_a_subagent_through_grove_mcp() {
    let mut s = setup(json!([]));
    // Codex for the subagent, since a Claude one would play these steps too.
    s.sidecar_plays(json!([
        spawn_step(json!({"agent": "codex", "prompt": "write the tests"})),
        {"wait_message": "never"},
    ]));

    let parent = s.spawn_parent_in(Agent::Claude, Mode::Chat);

    let (asked_by, PanelSpec::Agent { session, worktree }) = s.next_panel_request() else {
        panic!("not an agent panel");
    };
    assert_eq!(asked_by, Some(parent));
    assert_eq!(worktree, s.worktree);
    let (text, is_error) = tool_text(&s.tool_reply("claude-sidecar"));
    assert!(!is_error, "{text}");
    assert_eq!(
        text,
        format!(
            "Started subagent session {session} in {}.",
            worktree.display()
        )
    );
}

#[test]
fn the_worktree_argument_starts_the_subagent_in_another_tracked_worktree() {
    let mut s = setup(json!([]));
    let other = s.other_worktree();
    s.parent_plays(json!([
        spawn_step(json!({"agent": "codex", "prompt": "hi", "worktree": other})),
        {"wait_input": "\r"},
    ]));

    let parent = s.spawn_parent(Agent::Claude);

    let (asked_by, PanelSpec::Agent { session, worktree }) = s.next_panel_request() else {
        panic!("not an agent panel");
    };
    assert_eq!(asked_by, Some(parent));
    assert_eq!(worktree, other);
    let (text, is_error) = tool_text(&s.tool_reply("claude"));
    assert!(!is_error, "{text}");
    assert_eq!(
        text,
        format!("Started subagent session {session} in {}.", other.display())
    );
}

#[test]
fn a_subagent_starts_in_chat_mode_with_the_prompt_as_its_first_message() {
    let mut s = setup(json!([
        spawn_step(json!({"agent": "claude", "prompt": "write the tests"})),
        {"wait_input": "\r"},
    ]));
    s.spawn_parent(Agent::Claude);
    let (_, PanelSpec::Agent { session, .. }) = s.next_panel_request() else {
        panic!("not an agent panel");
    };

    let deadline = Instant::now() + TIMEOUT;
    let sent = loop {
        let stdin: String = s
            .log("claude-sidecar")
            .iter()
            .filter_map(|line| line.get("stdin")?.as_str().map(str::to_owned))
            .collect();
        let sent = stdin
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|message| message["type"] == "user_message");
        if let Some(sent) = sent {
            break sent;
        }
        assert!(Instant::now() < deadline, "the subagent got no message");
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(sent["text"], "write the tests");
    let env = s.log("claude-sidecar")[0]["env"].clone();
    assert_eq!(env["GROVE_SESSION_ID"], session.to_string());
}

#[test]
fn subagents_nest_three_levels_deep_and_the_third_level_cannot_start_another() {
    let mut s = setup(json!([
        spawn_step(json!({"agent": "claude", "prompt": "go deeper"})),
        {"wait_input": "\r"},
    ]));
    // Every Claude subagent asks for one more.
    s.sidecar_plays(json!([
        spawn_step(json!({"agent": "claude", "prompt": "go deeper"})),
        {"wait_message": "never"},
    ]));

    let top = s.spawn_parent(Agent::Claude);

    // Levels 1, 2 and 3, each the subagent of the one before.
    let mut parent = top;
    let mut chain = Vec::new();
    for _ in 0..3 {
        let (asked_by, PanelSpec::Agent { session, .. }) = s.next_panel_request() else {
            panic!("not an agent panel");
        };
        assert_eq!(asked_by, Some(parent));
        chain.push(session);
        parent = session;
    }
    let deepest = chain[2];
    // Levels 1 and 2 started theirs. Level 3 was refused.
    let replies = s.tool_replies("claude-sidecar", 3);
    let refused: Vec<_> = replies
        .iter()
        .map(tool_text)
        .filter(|(_, is_error)| *is_error)
        .map(|(text, _)| text)
        .collect();
    assert_eq!(
        refused,
        [format!(
            "grove refused: session {deepest} is a subagent 3 levels deep, the most grove \
             allows, so it cannot start another"
        )]
    );
    assert!(!s.panel_requested());
    assert_eq!(s.core.sessions(&s.worktree).unwrap().len(), 4);
}

#[test]
fn a_parent_at_the_subagent_limit_gets_a_tool_error_that_names_the_limit() {
    let spawn = || spawn_step(json!({"agent": "claude", "prompt": "write the tests"}));
    let mut s = setup_with(
        json!([spawn(), spawn(), spawn(), {"wait_input": "\r"}]),
        |cfg| cfg.max_live_subagents_per_parent = 2,
    );

    let parent = s.spawn_parent(Agent::Claude);

    let replies = s.tool_replies("claude", 3);
    let replies: Vec<_> = replies.iter().map(tool_text).collect();
    assert!(!replies[0].1 && !replies[1].1, "{replies:?}");
    assert_eq!(
        replies[2],
        (
            format!(
                "grove refused: session {parent} has 2 live subagents, the most grove allows, \
                 so it cannot start another until one ends"
            ),
            true
        )
    );
    // The two that started asked for a panel each. The refused one asked for none.
    s.next_panel_request();
    s.next_panel_request();
    assert!(!s.panel_requested());
    assert_eq!(s.core.sessions(&s.worktree).unwrap().len(), 3);
}

#[test]
fn a_worktree_grove_does_not_track_is_refused() {
    let mut s = setup(json!([
        spawn_step(json!({"agent": "claude", "prompt": "hi", "worktree": "/tmp/not-a-worktree"})),
        {"wait_input": "\r"},
    ]));

    s.spawn_parent(Agent::Claude);

    let (text, is_error) = tool_text(&s.tool_reply("claude"));
    assert!(is_error);
    assert_eq!(
        text,
        "grove refused: grove does not track a worktree at /tmp/not-a-worktree"
    );
    assert!(!s.panel_requested());
}

#[test]
fn a_spawn_request_without_its_own_sessions_token_is_refused() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn_parent(Agent::Claude);
    let other = s.spawn_parent(Agent::Codex);
    let token = s.token("claude", session);
    let others_token = s.token("codex", other);
    let spawn = json!({"type": "subagent_spawn", "agent": "claude", "prompt": "hi"});

    let refused = [
        json!({"session": session.to_string(), "token": others_token, "request": spawn}),
        json!({"session": other.to_string(), "token": token, "request": spawn}),
        json!({"session": session.to_string(), "request": spawn}),
        json!({"session": session.to_string(), "token": "", "request": spawn}),
    ];

    for request in &refused {
        assert_eq!(
            panel_request(&s.cfg.socket, request),
            json!({"error": "not authorized"}),
            "{request}"
        );
    }
    assert!(!s.panel_requested());
}

#[test]
fn a_spawn_request_with_its_own_token_names_its_session_as_the_parent() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn_parent(Agent::Codex);
    let token = s.token("codex", session);
    let request = json!({
        "session": session.to_string(),
        "token": token,
        "request": {"type": "subagent_spawn", "agent": "codex", "prompt": "hi"},
    });

    let reply = panel_request(&s.cfg.socket, &request);

    let (parent, PanelSpec::Agent { session: child, .. }) = s.next_panel_request() else {
        panic!("not an agent panel");
    };
    assert_eq!(parent, Some(session));
    assert_eq!(reply["result"]["session"], child.to_string());
}

#[test]
fn dropping_the_core_right_after_a_spawn_request_stops_it_at_once() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn_parent(Agent::Claude);
    let token = s.token("claude", session);
    let request = json!({
        "session": session.to_string(),
        "token": token,
        "request": {"type": "subagent_spawn", "agent": "claude", "prompt": "hi"},
    });
    // The setup owns its core, so a spare one on a home of its own takes the place.
    let (spare, _) = Core::start(CoreConfig::new(s.dir.path().join("spare"))).unwrap();
    let mut stream = UnixStream::connect(&s.cfg.socket).unwrap();
    stream.write_all(request.to_string().as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();

    drop(std::mem::replace(&mut s.core, spare));

    // Gone when the drop returns, not later on another thread.
    let cfg = s.cfg.clone();
    assert!(!cfg.socket.exists());
    // A child another test has forked holds grove.lock until it execs.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match Core::start(cfg.clone()) {
            Ok(_) => break,
            Err(grove_core::CoreError::AlreadyRunning(_)) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_request_too_large_for_the_socket_is_refused_with_a_reply() {
    let mut s = setup(json!([
        spawn_step(json!({"agent": "claude", "prompt": "x".repeat(100 * 1024)})),
        {"wait_input": "\r"},
    ]));

    s.spawn_parent(Agent::Claude);

    let (text, is_error) = tool_text(&s.tool_reply("claude"));
    assert!(is_error);
    assert_eq!(text, "grove refused: request too large");
    assert!(!s.panel_requested());
}
