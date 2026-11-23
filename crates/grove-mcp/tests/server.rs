//! grove-mcp as an agent runs it: a child on stdio, spoken to in JSON-RPC lines.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Starts grove-mcp with `env` only, sends `messages`, closes its stdin and returns every line
/// it wrote, and how long that took.
fn run(env: &[(&str, &str)], messages: &[Value]) -> (Vec<Value>, Duration) {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_grove-mcp"))
        .env_clear()
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut lines = stdout.lines();
    let mut replies = Vec::new();
    for message in messages {
        writeln!(stdin, "{message}").unwrap();
        // A request waits for its reply before the next message goes, as a client does.
        if message.get("id").is_some() {
            replies.push(serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap());
        }
    }
    drop(stdin);
    child.wait().unwrap();
    (replies, started.elapsed())
}

fn initialize() -> [Value; 2] {
    [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        }}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ]
}

const SESSION: [(&str, &str); 3] = [
    ("GROVE_SESSION_ID", "3"),
    ("GROVE_SESSION_TOKEN", "a-token"),
    ("GROVE_SOCKET", "/tmp/grove-mcp-test-no-core.sock"),
];

#[test]
fn with_grove_not_running_the_spawn_tool_says_so_at_once() {
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "subagent_spawn",
        "arguments": {"agent": "codex", "prompt": "hi"},
    }});
    let [init, initialized] = initialize();

    let (replies, took) = run(&SESSION, &[init, initialized, call]);

    let result = &replies[1]["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(
            "grove is not running: nothing answers on /tmp/grove-mcp-test-no-core.sock"
        ),
        "{text}"
    );
    assert!(took < Duration::from_secs(5), "took {took:?}");
}

#[test]
fn the_spawn_tool_takes_an_agent_a_prompt_and_an_optional_worktree() {
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let [init, initialized] = initialize();

    let (replies, _) = run(&SESSION, &[init, initialized, list]);

    let schema = input_schema(&replies[1], "subagent_spawn");
    assert_eq!(schema["required"], json!(["agent", "prompt"]));
    assert_eq!(
        schema["properties"]["agent"]["enum"],
        json!(["claude", "codex"]),
        "{schema}"
    );
    assert!(schema["properties"]["worktree"].is_object(), "{schema}");
}

/// The input schema of `tool` in a tools/list reply.
fn input_schema(reply: &Value, tool: &str) -> Value {
    let tools = reply["result"]["tools"].as_array().unwrap();
    let found = tools.iter().find(|t| t["name"] == tool);
    found.unwrap_or_else(|| panic!("no {tool} in {reply}"))["inputSchema"].clone()
}

#[test]
fn the_panel_tools_take_a_kind_a_title_a_command_an_id_and_a_state() {
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let [init, initialized] = initialize();

    let (replies, _) = run(&SESSION, &[init, initialized, list]);

    let open = input_schema(&replies[1], "panel_open");
    assert_eq!(open["required"], json!(["kind", "title", "cmd"]), "{open}");
    assert_eq!(
        open["properties"]["kind"]["enum"],
        json!(["log", "command"]),
        "{open}"
    );
    let read = input_schema(&replies[1], "panel_read");
    assert_eq!(read["required"], json!(["id"]), "{read}");
    assert!(read["properties"]["lines"].is_object(), "{read}");
    let wait = input_schema(&replies[1], "panel_wait");
    assert_eq!(wait["required"], json!(["id", "state"]), "{wait}");
    assert_eq!(
        wait["properties"]["state"]["enum"],
        json!(["exited"]),
        "{wait}"
    );
    assert!(wait["properties"]["timeout_seconds"].is_object(), "{wait}");
    let close = input_schema(&replies[1], "panel_close");
    assert_eq!(close["required"], json!(["id"]), "{close}");
}

#[test]
fn the_workspace_and_profile_tools_take_nothing_or_one_object() {
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let [init, initialized] = initialize();

    let (replies, _) = run(&SESSION, &[init, initialized, list]);

    for tool in ["workspace_get", "profile_get"] {
        let schema = input_schema(&replies[1], tool);
        assert_eq!(schema["type"], "object", "{tool}: {schema}");
        assert!(schema.get("required").is_none(), "{tool}: {schema}");
    }
    for (tool, argument) in [("workspace_set", "workspace"), ("profile_set", "changes")] {
        let schema = input_schema(&replies[1], tool);
        assert_eq!(schema["required"], json!([argument]), "{tool}: {schema}");
        assert_eq!(
            schema["properties"][argument]["type"], "object",
            "{tool}: {schema}"
        );
    }
}

#[test]
fn the_workspace_tools_say_the_workspace_is_the_sessions_and_a_subagent_shares_its_parents() {
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let [init, initialized] = initialize();

    let (replies, _) = run(&SESSION, &[init, initialized, list]);

    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    for tool in ["workspace_get", "workspace_set"] {
        let found = tools.iter().find(|t| t["name"] == tool).unwrap();
        let text = found["description"].as_str().unwrap();
        assert!(text.contains("this session's workspace"), "{tool}: {text}");
        assert!(text.contains("A subagent session"), "{tool}: {text}");
        assert!(!text.contains("worktree"), "{tool}: {text}");
    }
    let set = input_schema(&replies[1], "workspace_set");
    let shape = set["properties"]["workspace"]["description"]
        .as_str()
        .unwrap();
    // An agent panel always names a session.
    assert!(shape.contains("\"session\": <id>,"), "{shape}");
    assert!(
        shape.contains("this workspace's session or a subagent"),
        "{shape}"
    );
}

#[test]
fn panel_calls_that_cannot_be_right_are_refused_before_asking_grove() {
    let calls = [
        (
            "panel_open",
            json!({"kind": "log", "title": "t", "cmd": "  "}),
            "A panel needs a command.",
        ),
        (
            "panel_wait",
            json!({"id": "7", "state": "exited", "timeout_seconds": 601}),
            "timeout_seconds is at most 600.",
        ),
        (
            "panel_read",
            json!({"id": "seven"}),
            "There is no panel seven.",
        ),
    ];
    for (n, (tool, arguments, refusal)) in calls.into_iter().enumerate() {
        let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": tool,
            "arguments": arguments,
        }});
        let [init, initialized] = initialize();

        // No core runs, so a call that reached grove would say so.
        let (replies, _) = run(&SESSION, &[init, initialized, call]);

        let result = &replies[1]["result"];
        assert_eq!(result["isError"], true, "{n}: {result}");
        assert_eq!(result["content"][0]["text"], refusal, "{n}");
    }
}

#[test]
fn without_its_session_variables_it_will_not_start() {
    let output = Command::new(env!("CARGO_BIN_EXE_grove-mcp"))
        .env_clear()
        .env("GROVE_SESSION_ID", "3")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .contains("GROVE_SESSION_TOKEN is not set, so grove did not start this agent session"),
        "{stderr}"
    );
}
