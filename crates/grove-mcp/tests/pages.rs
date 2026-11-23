//! Inline pages: an agent session in chat mode shows an HTML page in its conversation through
//! grove-mcp's `html_show` tool, which forwards to the core as `panel_open` does.

mod common;

use common::*;
use grove_core::{AgentMessage, Mode};
use tokio::sync::mpsc::unbounded_channel;

const CHART: &str = "<svg viewBox=\"0 0 10 10\"><rect width=\"4\" height=\"9\"/></svg>";

/// The log of the fake that runs `agent` in chat mode.
fn chat_fake(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "claude-sidecar",
        Agent::Codex => "codex",
    }
}

/// An agent session in chat mode that waits, with a grove-mcp of its own.
fn chat_agent_with_client(s: &Setup, agent: Agent) -> (SessionId, McpClient) {
    let session = s.spawn_parent_in(agent, Mode::Chat);
    let token = s.token(chat_fake(agent), session);
    (session, McpClient::start(&s.cfg.socket, session, &token))
}

/// The pages in the session's conversation, once there are `n`.
fn pages(s: &Setup, session: SessionId, n: usize) -> Vec<AgentMessage> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (sink, _frames) = unbounded_channel();
        let pages: Vec<AgentMessage> = s
            .core
            .subscribe(session, sink)
            .unwrap()
            .into_iter()
            .filter(|message| matches!(message, AgentMessage::InlinePage { .. }))
            .collect();
        if pages.len() >= n {
            return pages;
        }
        assert!(Instant::now() < deadline, "{} of {n} pages", pages.len());
        thread::sleep(Duration::from_millis(10));
    }
}

/// The id in the text of an `html_show` that worked.
fn page_id(text: &str) -> String {
    let id = text
        .strip_prefix("Showed the page in the conversation as ")
        .and_then(|rest| rest.strip_suffix('.'))
        .unwrap_or_else(|| panic!("{text}"));
    id.to_owned()
}

fn assert_html_show_puts_the_page_in_the_conversation(agent: Agent) {
    let s = setup(json!([]));
    let (session, mut client) = chat_agent_with_client(&s, agent);

    let (text, is_error) = client.call(
        "html_show",
        json!({"title": "Build workflow", "html": CHART, "height": 320}),
    );

    assert!(!is_error, "{text}");
    assert_eq!(
        pages(&s, session, 1),
        [AgentMessage::InlinePage {
            id: page_id(&text),
            title: "Build workflow".into(),
            html: CHART.into(),
            height: Some(320),
        }]
    );
}

#[test]
fn claude_html_show_puts_the_page_in_the_sessions_conversation() {
    assert_html_show_puts_the_page_in_the_conversation(Agent::Claude);
}

#[test]
fn codex_html_show_puts_the_page_in_the_sessions_conversation() {
    assert_html_show_puts_the_page_in_the_conversation(Agent::Codex);
}

#[test]
fn html_show_takes_a_file_in_the_worktree_and_refuses_one_outside_it() {
    let s = setup(json!([]));
    let (session, mut client) = chat_agent_with_client(&s, Agent::Claude);
    let inside = s.worktree.join("chart.html");
    fs::write(&inside, CHART).unwrap();
    let outside = s.dir.path().join("secret.html");
    fs::write(&outside, "<p>secret</p>").unwrap();

    let (refusal, refused) = client.call("html_show", json!({"title": "No", "path": outside}));
    let (text, is_error) = client.call("html_show", json!({"title": "Chart", "path": inside}));

    assert!(refused);
    assert_eq!(
        refusal,
        format!(
            "grove refused: {} is not a path inside the worktree",
            outside.display()
        )
    );
    assert!(!is_error, "{text}");
    assert_eq!(
        pages(&s, session, 1),
        [AgentMessage::InlinePage {
            id: page_id(&text),
            title: "Chart".into(),
            html: CHART.into(),
            height: None,
        }]
    );
}

#[test]
fn an_agent_shows_a_page_from_the_mcp_config_grove_passes_it() {
    let s = setup(json!([]));
    s.sidecar_plays(json!([
        {"mcp_call": "grove", "tool": "html_show",
         "arguments": {"title": "Build workflow", "html": CHART}},
        {"wait_message": "never"},
    ]));

    let session = s.spawn_parent_in(Agent::Claude, Mode::Chat);

    let (text, is_error) = tool_text(&s.tool_reply("claude-sidecar"));
    assert!(!is_error, "{text}");
    assert_eq!(
        pages(&s, session, 1),
        [AgentMessage::InlinePage {
            id: page_id(&text),
            title: "Build workflow".into(),
            html: CHART.into(),
            height: None,
        }]
    );
}

#[test]
fn html_show_in_terminal_mode_is_refused() {
    let s = setup(json!([{"wait_input": "\r"}]));
    let (session, mut client) = agent_with_client(&s, Agent::Claude);

    let (text, is_error) = client.call("html_show", json!({"title": "Chart", "html": CHART}));

    assert!(is_error);
    assert_eq!(
        text,
        format!("grove refused: session {session} is not an agent session in chat mode")
    );
}
