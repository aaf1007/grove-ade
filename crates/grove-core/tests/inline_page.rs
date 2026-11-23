//! Inline pages: an agent session in chat mode shows a small HTML page in its conversation with
//! an `html_show` panel request, as `grove-mcp` sends it. Driven by the fake agents in
//! fixtures/fake-agents.

use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, AgentMessage, AgentSpec, Core, CoreConfig, CoreEvent, Mode, SessionId, SpawnSpec,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

mod common;
use common::{conversation_only, grove_hook, untimed, write_script};

const TIMEOUT: Duration = Duration::from_secs(10);

// The core owns its runtime, and dropping it inside an async context panics.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn fake(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fake-agents")
        .join(name)
}

/// A core tracking one fresh repo. In chat mode Claude is the fake sidecar: it starts a turn on
/// the first message, calls `html_show`, and waits. In terminal mode it is the fake TUI, which
/// waits. Both keep the conversation's record in `record`.
struct Setup {
    dir: TempDir,
    cfg: CoreConfig,
    core: Core,
    _events: broadcast::Receiver<CoreEvent>,
    worktree: PathBuf,
    record: PathBuf,
    chat_log: PathBuf,
    tui_log: PathBuf,
}

const CALL: &str = "toolu_show";

fn tool_call() -> Value {
    json!({"type": "tool_call", "id": CALL, "tool": "mcp__grove__html_show",
           "input": {"title": "Build workflow"}})
}

fn setup() -> Setup {
    // macOS caps a socket path at 104 bytes, and its default temp folder is long.
    let dir = tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().join("repo");
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());

    let path = |name: &str| dir.path().join(name);
    let scenario = |name: &str, steps: Value| {
        fs::write(path(name), json!({ "steps": steps }).to_string()).unwrap();
        path(name).display().to_string()
    };
    let chat = scenario(
        "chat.json",
        json!([
            {"wait_message": "user_message"},
            {"send": {"type": "turn_start"}},
            {"send": tool_call()},
            {"wait_message": "never"},
        ]),
    );
    let tui = scenario("tui.json", json!([{"wait_input": "\r"}]));
    let record = path("record");
    fs::create_dir(&record).unwrap();
    let (chat_log, tui_log) = (path("chat.log"), path("tui.log"));
    let wrapper = |name: &str, variables: String| {
        let wrapper = path(name);
        let fake = fake(name);
        write_script(
            &wrapper,
            &format!(
                "export {variables} FAKE_CLAUDE_RECORD='{}'\nexec '{}' \"$@\"",
                record.display(),
                fake.display()
            ),
        );
        wrapper
    };
    let mut cfg = CoreConfig::new(path("home"));
    cfg.hook_relay = grove_hook().to_path_buf();
    cfg.claude_sidecar_command = wrapper(
        "claude-sidecar",
        format!(
            "FAKE_CLAUDE_SIDECAR_SCENARIO='{chat}' FAKE_CLAUDE_SIDECAR_LOG='{}'",
            chat_log.display()
        ),
    );
    cfg.claude_command = wrapper(
        "claude",
        format!(
            "FAKE_CLAUDE_SCENARIO='{tui}' FAKE_CLAUDE_LOG='{}'",
            tui_log.display()
        ),
    );
    let (core, events) = Core::start(cfg.clone()).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    Setup {
        dir,
        cfg,
        core,
        _events: events,
        worktree,
        record,
        chat_log,
        tui_log,
    }
}

// A failed test shows what the fakes did.
impl Drop for Setup {
    fn drop(&mut self) {
        if thread::panicking() {
            for log in [&self.chat_log, &self.tui_log] {
                eprintln!(
                    "{}:\n{}",
                    log.display(),
                    fs::read_to_string(log).unwrap_or_default()
                );
            }
        }
    }
}

/// One agent session of a setup, as its `grove-mcp` reaches the core: with the session's id and
/// token on the core socket.
struct Caller<'a> {
    setup: &'a Setup,
    session: SessionId,
    token: String,
}

impl Setup {
    /// Starts Claude in `mode` and waits for its process, which holds the session's token.
    fn agent(&self, mode: Mode) -> Caller<'_> {
        let prompt = (mode == Mode::Chat).then(|| "draw the build workflow".to_owned());
        let spec = AgentSpec {
            agent: Agent::Claude,
            mode,
            prompt,
            cols: 120,
            rows: 40,
            panel: None,
        };
        let session = self.core.spawn_agent(&self.worktree, spec).unwrap();
        let log = match mode {
            Mode::Chat => &self.chat_log,
            Mode::Terminal => &self.tui_log,
        };
        Caller {
            setup: self,
            session,
            token: first_token(log),
        }
    }
}

/// The session token of each process the fake logged in `log`, in launch order.
fn launches(log: &Path) -> Vec<String> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|line| {
            line["env"]["GROVE_SESSION_TOKEN"]
                .as_str()
                .map(str::to_owned)
        })
        .collect()
}

/// The session token of the first process the fake logs in `log`.
fn first_token(log: &Path) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(token) = launches(log).into_iter().next() {
            return token;
        }
        assert!(Instant::now() < deadline, "no launch in the log");
        thread::sleep(Duration::from_millis(10));
    }
}

impl Caller<'_> {
    /// Sends an `html_show` request with `arguments` and returns the core's reply.
    fn html_show(&self, arguments: Value) -> Value {
        let mut request = arguments;
        request["type"] = json!("html_show");
        let message = json!({
            "session": self.session.to_string(),
            "token": self.token,
            "request": request,
        });
        let mut stream = UnixStream::connect(&self.setup.cfg.socket).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        stream.write_all(message.to_string().as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        serde_json::from_slice(&reply).unwrap()
    }

    /// The id of the page an `html_show` request with `arguments` showed.
    fn shown(&self, arguments: Value) -> String {
        let reply = self.html_show(arguments);
        let page = reply["result"]["page"].as_str();
        page.unwrap_or_else(|| panic!("no page in {reply}"))
            .to_owned()
    }

    /// Why the core refused an `html_show` request with `arguments`.
    fn refused(&self, arguments: Value) -> String {
        let reply = self.html_show(arguments);
        let error = reply["error"].as_str();
        error
            .unwrap_or_else(|| panic!("no error in {reply}"))
            .to_owned()
    }

    fn conversation(&self) -> Conversation {
        Conversation::subscribe(&self.setup.core, self.session)
    }
}

/// A session's conversation as one subscribed panel gets it: the messages before, then each
/// frame. Times are taken out.
struct Conversation {
    messages: Vec<AgentMessage>,
    frames: UnboundedReceiver<Vec<AgentMessage>>,
}

impl Conversation {
    fn subscribe(core: &Core, session: SessionId) -> Self {
        let (sink, frames) = unbounded_channel();
        let before = core.subscribe(session, sink).unwrap();
        Self {
            messages: conversation_only(untimed(before)),
            frames,
        }
    }

    /// The messages once `done` holds for them.
    fn until(mut self, done: impl Fn(&[AgentMessage]) -> bool) -> Vec<AgentMessage> {
        let deadline = Instant::now() + TIMEOUT;
        while !done(&self.messages) {
            match self.frames.try_recv() {
                Ok(frame) => self.messages.extend(conversation_only(untimed(frame))),
                Err(_) => {
                    assert!(
                        Instant::now() < deadline,
                        "the conversation stopped at {:?}",
                        self.messages
                    );
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
        self.messages
    }

    /// The messages once the agent has called the tool.
    fn until_the_call(self) -> Vec<AgentMessage> {
        self.until(|messages| messages.iter().any(is_call))
    }

    /// The messages once a page shows.
    fn until_a_page(self) -> Vec<AgentMessage> {
        self.until(|messages| messages.iter().any(is_page))
    }
}

fn is_call(message: &AgentMessage) -> bool {
    matches!(message, AgentMessage::ToolCall { .. })
}

fn is_page(message: &AgentMessage) -> bool {
    matches!(message, AgentMessage::InlinePage { .. })
}

fn pages(messages: &[AgentMessage]) -> Vec<&AgentMessage> {
    messages.iter().filter(|m| is_page(m)).collect()
}

const CHART: &str = "<svg viewBox=\"0 0 10 10\"><rect width=\"4\" height=\"9\"/></svg>";

#[test]
fn html_show_adds_the_page_to_the_conversation_where_the_agent_called_the_tool() {
    let s = setup();
    let agent = s.agent(Mode::Chat);
    agent.conversation().until_the_call();
    let conversation = agent.conversation();

    let page = agent.shown(json!({"title": "Build workflow", "html": CHART, "height": 320}));

    let messages = conversation.until_a_page();
    assert_eq!(
        messages[messages.len() - 2..],
        [
            AgentMessage::ToolCall {
                id: CALL.into(),
                tool: "mcp__grove__html_show".into(),
                input: json!({"title": "Build workflow"}),
                time_unix_ms: None,
            },
            AgentMessage::InlinePage {
                id: page.clone(),
                title: "Build workflow".into(),
                html: CHART.into(),
                height: Some(320),
            },
        ]
    );
    // A panel that opens later gets it with the rest of the conversation.
    assert_eq!(agent.conversation().until_a_page(), messages);
}

#[test]
fn a_page_from_a_file_in_the_worktree_shows_what_the_file_held_then() {
    let s = setup();
    let agent = s.agent(Mode::Chat);
    agent.conversation().until_the_call();
    let file = s.worktree.join("docs/chart.html");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, CHART).unwrap();

    let page = agent.shown(json!({"title": "Chart", "path": file}));
    fs::write(&file, "<p>changed later</p>").unwrap();

    let messages = agent.conversation().until_a_page();
    assert_eq!(
        pages(&messages),
        [&AgentMessage::InlinePage {
            id: page,
            title: "Chart".into(),
            html: CHART.into(),
            height: None,
        }]
    );
}

#[test]
fn a_path_that_is_not_an_html_file_inside_the_worktree_is_refused() {
    let s = setup();
    let agent = s.agent(Mode::Chat);
    agent.conversation().until_the_call();
    let outside = s.dir.path().join("secret.html");
    fs::write(&outside, "<p>secret</p>").unwrap();
    fs::write(s.worktree.join("notes.txt"), "notes").unwrap();
    fs::create_dir(s.worktree.join("folder.html")).unwrap();
    // A link in the worktree to a file outside it.
    symlink(&outside, s.worktree.join("link.html")).unwrap();
    // A link in the worktree to a folder outside it.
    symlink(s.dir.path(), s.worktree.join("up")).unwrap();
    let repo = s.worktree.display();
    let not_inside = "is not a path inside the worktree";

    let refusals = [
        (outside.display().to_string(), not_inside),
        (format!("{repo}/../secret.html"), not_inside),
        (format!("{repo}/link.html"), not_inside),
        (format!("{repo}/up/secret.html"), not_inside),
        (
            "secret.html".to_owned(),
            "cannot be shown: it is not an absolute path",
        ),
        (
            format!("{repo}/notes.txt"),
            "cannot be shown: it is not an .html file",
        ),
        (
            format!("{repo}/folder.html"),
            "cannot be shown: it is not a file",
        ),
    ];
    for (path, why) in refusals {
        assert_eq!(
            agent.refused(json!({"title": "Chart", "path": path})),
            format!("{path} {why}"),
        );
    }
    let missing = format!("{repo}/missing.html");
    assert_eq!(
        agent.refused(json!({"title": "Chart", "path": missing})),
        format!("could not resolve {missing}")
    );

    // A page that shows now comes after every refusal, and is the only one.
    let page = agent.shown(json!({"title": "Chart", "html": CHART}));
    let messages = agent.conversation().until_a_page();
    assert!(
        matches!(pages(&messages)[..], [AgentMessage::InlinePage { id, .. }] if *id == page),
        "{messages:?}"
    );
}

#[test]
fn a_call_that_cannot_be_a_page_is_refused() {
    let s = setup();
    let agent = s.agent(Mode::Chat);
    agent.conversation().until_the_call();
    let file = s.worktree.join("chart.html");
    fs::write(&file, CHART).unwrap();
    let large = s.worktree.join("large.html");
    fs::write(&large, "a".repeat(1024 * 1024 + 1)).unwrap();

    assert_eq!(
        agent.refused(json!({"title": "Chart"})),
        "html_show takes html or a path, one of the two"
    );
    assert_eq!(
        agent.refused(json!({"title": "Chart", "html": CHART, "path": file})),
        "html_show takes html or a path, one of the two"
    );
    assert_eq!(
        agent.refused(json!({"title": "  ", "html": CHART})),
        "a page needs a title"
    );
    assert_eq!(
        agent.refused(json!({"title": "Chart", "html": "  \n"})),
        "the page is empty"
    );
    assert_eq!(
        agent.refused(json!({"title": "Chart", "path": large})),
        "the page has 1048577 bytes, and grove shows at most 1048576"
    );
}

#[test]
fn a_height_is_capped_at_2000_and_0_fits_the_content() {
    let s = setup();
    let agent = s.agent(Mode::Chat);
    agent.conversation().until_the_call();

    agent.shown(json!({"title": "Tall", "html": CHART, "height": 9000}));
    agent.shown(json!({"title": "Fit", "html": CHART, "height": 0}));

    let messages = agent
        .conversation()
        .until(|messages| pages(messages).len() == 2);
    let heights: Vec<Option<u32>> = pages(&messages)
        .into_iter()
        .map(|page| match page {
            AgentMessage::InlinePage { height, .. } => *height,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(heights, [Some(2000), None]);
}

#[test]
fn html_show_is_refused_in_terminal_mode_and_for_a_session_that_is_no_agent() {
    let s = setup();
    let agent = s.agent(Mode::Terminal);

    assert_eq!(
        agent.refused(json!({"title": "Chart", "html": CHART})),
        format!(
            "session {} is not an agent session in chat mode",
            agent.session
        )
    );

    // A shell has no token, so nothing it sends is taken as a session's.
    let shell = SpawnSpec {
        command: "/bin/cat".into(),
        args: vec![],
        cols: 80,
        rows: 24,
    };
    let shell = s.core.spawn(&s.worktree, shell).unwrap();
    let as_shell = Caller {
        setup: &s,
        session: shell,
        token: agent.token.clone(),
    };
    assert_eq!(
        as_shell.refused(json!({"title": "Chart", "html": CHART})),
        "not authorized"
    );
}

/// Adds `messages` to the record of the conversation `session` resumes, in the shape the sidecar
/// sends a `history` in.
fn record(s: &Setup, session: SessionId, messages: &[Value]) {
    let conversation = s.core.resume_id(session).unwrap();
    let lines: String = messages.iter().map(|m| format!("{m}\n")).collect();
    fs::write(s.record.join(format!("{conversation}.jsonl")), lines).unwrap();
}

fn tool_result(output: &str) -> Value {
    json!({"type": "tool_result", "id": CALL, "output": output})
}

/// Ends the session's process and starts it again, which resumes the conversation from its
/// record. Returns the whole conversation, which ends with the record's last turn.
fn resumed(agent: &Caller<'_>) -> Vec<AgentMessage> {
    let core = &agent.setup.core;
    core.kill(agent.session).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    // A resume does nothing until the killed process has ended.
    while launches(&agent.setup.chat_log).len() < 2 {
        core.resume_agent_session(agent.session).unwrap();
        assert!(Instant::now() < deadline, "the session did not resume");
        thread::sleep(Duration::from_millis(10));
    }
    agent
        .conversation()
        .until(|messages| matches!(messages.last(), Some(AgentMessage::TurnEnded { .. })))
}

#[test]
fn a_shown_page_comes_back_with_the_conversation_even_after_its_file_is_gone() {
    let s = setup();
    let agent = s.agent(Mode::Chat);
    agent.conversation().until_the_call();
    let file = s.worktree.join("chart.html");
    fs::write(&file, CHART).unwrap();
    let page = agent.shown(json!({"title": "Chart", "path": file, "height": 240}));
    fs::remove_file(&file).unwrap();
    record(
        &s,
        agent.session,
        &[
            json!({"type": "user", "text": "draw the build workflow"}),
            json!({"type": "turn_start"}),
            tool_call(),
            tool_result(&format!("Showed the page in the conversation as {page}.")),
            json!({"type": "text_delta", "text": "Here it is."}),
            json!({"type": "turn_end"}),
        ],
    );

    let messages = resumed(&agent);

    assert_eq!(
        messages[3..6],
        [
            AgentMessage::ToolResult {
                id: CALL.into(),
                output: format!("Showed the page in the conversation as {page}."),
                is_error: false,
            },
            AgentMessage::InlinePage {
                id: page,
                title: "Chart".into(),
                html: CHART.into(),
                height: Some(240),
            },
            AgentMessage::TextDelta {
                text: "Here it is.".into()
            },
        ]
    );
}

#[test]
fn only_a_result_that_names_a_saved_page_of_the_session_brings_one_back_and_only_once() {
    let s = setup();
    let agent = s.agent(Mode::Chat);
    agent.conversation().until_the_call();
    let page = agent.shown(json!({"title": "Chart", "html": CHART}));
    let said = format!("Showed the page in the conversation as {page}.");
    record(
        &s,
        agent.session,
        &[
            json!({"type": "user", "text": "draw the build workflow"}),
            json!({"type": "turn_start"}),
            tool_call(),
            // No such page, a page id that is no file name, and a failed call.
            tool_result("Showed the page in the conversation as page-0000000000000000."),
            tool_result("Showed the page in the conversation as page-../../../token."),
            json!({"type": "tool_result", "id": CALL, "output": said, "is_error": true}),
            tool_result(&said),
            // The agent printing the id again shows no second copy.
            tool_result(&said),
            json!({"type": "turn_end"}),
        ],
    );

    let messages = resumed(&agent);

    let shown: Vec<usize> = (0..messages.len())
        .filter(|&at| is_page(&messages[at]))
        .collect();
    // After the first result that worked and names it.
    assert_eq!(shown, [7], "{messages:?}");
}
