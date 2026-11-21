//! Review batches, sent to the fakes in fixtures/fake-agents. In terminal mode the fake Claude
//! Code's state comes from the real grove-hook binary. In chat mode the fake Claude sidecar or the
//! fake Codex app server reports it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, AgentMessage, AgentSpec, AgentState, BatchComment, CommentId, CommentState, Core,
    CoreConfig, CoreError, CoreEvent, InputKind, Mode, NewReviewComment, SessionId,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::sync::mpsc::unbounded_channel;

mod common;
use common::{conversation_only, grove_hook};

const TIMEOUT: Duration = Duration::from_secs(10);

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

// "line 1\n" to "line <n>\n".
fn numbered(n: u32) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

// The fake waits here until its stdin closes. It waits for a control character, which grove
// strips from every batch, so no batch ends the wait.
fn keep_running() -> Value {
    json!({"wait_input": "\u{4}"})
}

// The fake's turn ends with bracketed paste on, so the session is `Done` and can take a batch.
fn done_with_paste_on() -> Value {
    json!([
        {"mode": "bracketed_paste", "on": true},
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        keep_running(),
    ])
}

/// A core tracking one repo with `a.rs` and `b.rs` committed, 20 numbered lines each. Its agent
/// sessions run the fake for their agent and mode, playing `steps`.
struct Setup {
    dir: TempDir,
    core: Core,
    events: broadcast::Receiver<CoreEvent>,
    worktree: PathBuf,
}

fn setup(steps: Value) -> Setup {
    let dir = short_temp_dir();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.rs"), numbered(20)).unwrap();
    fs::write(repo.join("b.rs"), numbered(20)).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);

    // The fakes read their scenario and log paths from their environment, so wrappers set them.
    // Each session logs to its own file, and plays the one scenario.
    let scenario = dir.path().join("scenario.json");
    fs::write(&scenario, json!({ "steps": steps }).to_string()).unwrap();
    let wrap = |name: &str, fake: &str, exports: &str| {
        let fake = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/fake-agents")
            .join(fake);
        let wrapper = dir.path().join(name);
        let exports = exports
            .replace("{scenario}", &scenario.display().to_string())
            .replace(
                "{log}",
                &format!("{}/fake-$GROVE_SESSION_ID.log", dir.path().display()),
            );
        let script = format!(
            "#!/bin/sh\nexport {exports}\nexec '{}' \"$@\"\n",
            fake.display()
        );
        fs::write(&wrapper, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        wrapper
    };
    let claude = wrap(
        "claude",
        "claude",
        "FAKE_CLAUDE_SCENARIO='{scenario}' FAKE_CLAUDE_LOG=\"{log}\"",
    );
    let sidecar = wrap(
        "claude-sidecar",
        "claude-sidecar",
        "FAKE_CLAUDE_SIDECAR_SCENARIO='{scenario}' FAKE_CLAUDE_SIDECAR_LOG=\"{log}\"",
    );
    let codex = wrap(
        "codex",
        "codex",
        "FAKE_CODEX_APP_SERVER_SCENARIO='{scenario}' FAKE_CODEX_LOG=\"{log}\"",
    );
    let mut cfg = CoreConfig::new(dir.path().join("home"));
    cfg.claude_command = claude;
    cfg.claude_sidecar_command = sidecar;
    cfg.codex_command = codex;
    cfg.hook_relay = grove_hook().to_path_buf();
    let (core, events) = Core::start(cfg).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    Setup {
        dir,
        core,
        events,
        worktree,
    }
}

impl Setup {
    fn spawn(&self) -> SessionId {
        let spec = AgentSpec {
            agent: Some(Agent::Claude),
            mode: Mode::Terminal,
            prompt: None,
            cols: 120,
            rows: 40,
        };
        self.core.spawn_agent(&self.worktree, spec).unwrap()
    }

    /// Starts an agent session in chat mode, with "hi" as its first message.
    fn spawn_chat(&self, agent: Agent) -> SessionId {
        let spec = AgentSpec {
            agent: Some(agent),
            mode: Mode::Chat,
            prompt: Some("hi".into()),
            cols: 120,
            rows: 40,
        };
        self.core.spawn_agent(&self.worktree, spec).unwrap()
    }

    /// The session's conversation so far.
    fn conversation(&self, session: SessionId) -> Vec<AgentMessage> {
        let (sink, _messages) = unbounded_channel();
        conversation_only(self.core.subscribe(session, sink).unwrap())
    }

    /// The whole JSON lines the session's fake has read so far.
    fn lines(&self, session: SessionId) -> Vec<Value> {
        let stdin = self.stdin(session);
        let whole = stdin.rfind('\n').map_or("", |end| &stdin[..end]);
        whole
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Waits until the session's fake has read `n` lines that pass `wanted`, and returns them.
    fn wait_lines(
        &self,
        session: SessionId,
        n: usize,
        wanted: impl Fn(&Value) -> bool,
    ) -> Vec<Value> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let lines: Vec<Value> = self.lines(session).into_iter().filter(&wanted).collect();
            if lines.len() >= n {
                return lines;
            }
            assert!(Instant::now() < deadline, "the fake read {lines:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The text of each message the session's fake Claude sidecar has read, waiting for `n`.
    fn wait_user_messages(&self, session: SessionId, n: usize) -> Vec<String> {
        self.wait_lines(session, n, |line| line["type"] == "user_message")
            .iter()
            .map(|line| line["text"].as_str().unwrap().to_owned())
            .collect()
    }

    fn comment(&self, path: &str, start_line: u32, end_line: u32, body: &str) -> CommentId {
        let new = NewReviewComment {
            path: PathBuf::from(path),
            start_line,
            end_line,
            body: body.into(),
        };
        block_on(self.core.add_comment(&self.worktree, new)).unwrap()
    }

    /// Waits until the core says `session` is in `state`.
    fn wait_state(&mut self, session: SessionId, state: AgentState) {
        loop {
            if let CoreEvent::AgentState {
                session: s,
                state: now,
                ..
            } = next_event(&mut self.events)
                && (s, now) == (session, state)
            {
                return;
            }
        }
    }

    /// Waits until the session's process exits.
    fn wait_exit(&mut self, session: SessionId) {
        while !matches!(next_event(&mut self.events),
            CoreEvent::SessionExited { session: s, .. } if s == session)
        {}
    }

    /// Each comment on the worktree that is not resolved, oldest first, with its state.
    fn states(&self) -> Vec<(CommentId, CommentState)> {
        let comments = self.core.comments(&self.worktree).unwrap();
        comments.iter().map(|c| (c.id, c.state)).collect()
    }

    /// Drops the events sent so far.
    fn skip_events(&mut self) {
        while self.events.try_recv().is_ok() {}
    }

    /// Waits for the next `CommentsChanged` for the worktree.
    fn wait_comments_changed(&mut self) {
        loop {
            if let CoreEvent::CommentsChanged { worktree } = next_event(&mut self.events)
                && worktree == self.worktree
            {
                return;
            }
        }
    }

    /// Everything the session's fake read from its stdin so far, one char per byte.
    fn stdin(&self, session: SessionId) -> String {
        let log = self.dir.path().join(format!("fake-{session}.log"));
        fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                Some(line.get("stdin")?.as_str()?.to_owned())
            })
            .collect()
    }

    /// Waits until the session's fake has read as much as `want`, then checks it is `want`.
    fn wait_stdin(&self, session: SessionId, want: &str) {
        let deadline = Instant::now() + TIMEOUT;
        while self.stdin(session).len() < want.len() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(self.stdin(session), want);
    }
}

fn next_event(events: &mut broadcast::Receiver<CoreEvent>) -> CoreEvent {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(event) => return event,
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "no core event");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn three_comments_on_two_files_reach_the_agent_as_one_bracketed_paste_and_enter() {
    let mut s = setup(done_with_paste_on());
    let session = s.spawn();
    s.wait_state(session, AgentState::Done);
    s.comment("a.rs", 3, 3, "rename this");
    s.comment("b.rs", 5, 5, "add a test");
    s.comment("a.rs", 10, 11, "split this\ninto two");

    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    s.wait_stdin(
        session,
        "\x1b[200~\
         a.rs:3\n> line 3\nrename this\n\n\
         b.rs:5\n> line 5\nadd a test\n\n\
         a.rs:10-11\n> line 10\n> line 11\nsplit this\ninto two\
         \x1b[201~\r",
    );
}

#[test]
fn control_characters_in_any_part_arrive_as_plain_text() {
    let mut s = setup(done_with_paste_on());
    let session = s.spawn();
    s.wait_state(session, AgentState::Done);
    // Each part could end the paste early or reach the agent as a key.
    let path = "x\x1b[201~.rs";
    fs::write(
        s.worktree.join(path),
        "let s = \"\x1b[201~\x03\r\";\tok\n\x7fdone\u{9b}\n",
    )
    .unwrap();
    s.comment(path, 1, 2, "why \x1b[31mred\x00?\tkeep tabs\nand newlines");

    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    s.wait_stdin(
        session,
        "\x1b[200~\
         x[201~.rs:1-2\n> let s = \"[201~\";\tok\n> done\n\
         why [31mred?\tkeep tabs\nand newlines\
         \x1b[201~\r",
    );
}

// Plays `hooks` with bracketed paste on, waits for `state`, and checks a batch to the session is
// refused and nothing reaches it.
fn refused_while(hooks: &[&str], state: AgentState) {
    let mut steps = vec![json!({"mode": "bracketed_paste", "on": true})];
    steps.extend(hooks.iter().map(|hook| json!({ "hook": hook })));
    steps.push(keep_running());
    let mut s = setup(Value::Array(steps));
    let session = s.spawn();
    s.wait_state(session, state);
    s.comment("a.rs", 3, 3, "rename this");

    let refused = s.core.send_review_batch(&s.worktree, Some(session));

    assert!(
        matches!(
            refused,
            Err(CoreError::AgentNotReady { session: to, state: Some(now) })
                if (to, now) == (session, state)
        ),
        "{refused:?}"
    );
    // Writes reach the fake in order, so a batch written before this would show first.
    s.core.write(session, b"x").unwrap();
    s.wait_stdin(session, "x");
}

#[test]
fn a_working_session_is_refused_and_gets_nothing() {
    refused_while(&["UserPromptSubmit"], AgentState::Working);
}

#[test]
fn a_session_that_needs_input_is_refused_and_gets_nothing() {
    let approval = AgentState::NeedsInput {
        kind: InputKind::Approval,
    };
    refused_while(&["UserPromptSubmit", "PermissionRequest-Bash"], approval);
}

#[test]
fn a_session_with_bracketed_paste_off_is_refused_and_gets_nothing() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        keep_running(),
    ]));
    let session = s.spawn();
    s.wait_state(session, AgentState::Done);
    s.comment("a.rs", 3, 3, "rename this");

    let refused = s.core.send_review_batch(&s.worktree, Some(session));

    assert!(
        matches!(refused, Err(CoreError::BracketedPasteOff(to)) if to == session),
        "{refused:?}"
    );
    s.core.write(session, b"x").unwrap();
    s.wait_stdin(session, "x");
}

// The Send review picker reads it to say up front which session can take a batch.
#[test]
fn bracketed_paste_says_whether_a_terminal_session_has_it_on() {
    let mut s = setup(done_with_paste_on());
    let on = s.spawn();
    s.wait_state(on, AgentState::Done);
    assert!(s.core.bracketed_paste(on).unwrap());

    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        keep_running(),
    ]));
    let off = s.spawn();
    s.wait_state(off, AgentState::Done);
    assert!(!s.core.bracketed_paste(off).unwrap());
}

#[test]
fn bracketed_paste_fails_for_a_chat_session_which_has_no_terminal() {
    let s = setup(sidecar_done());
    let session = s.spawn_chat(Agent::Claude);

    let read = s.core.bracketed_paste(session);

    assert!(
        matches!(read, Err(CoreError::NoTerminal(id)) if id == session),
        "{read:?}"
    );
}

#[test]
fn with_no_target_the_batch_goes_to_the_most_recently_focused_agent_session() {
    let mut s = setup(done_with_paste_on());
    let first = s.spawn();
    s.wait_state(first, AgentState::Done);
    let second = s.spawn();
    s.wait_state(second, AgentState::Done);
    s.comment("a.rs", 3, 3, "rename this");

    let refused = s.core.send_review_batch(&s.worktree, None);
    assert!(
        matches!(&refused, Err(CoreError::NoAgentSession(w)) if *w == s.worktree),
        "{refused:?}"
    );

    // Focus makes each `Idle`.
    s.core.focus(second);
    s.core.focus(first);
    s.core.send_review_batch(&s.worktree, None).unwrap();

    s.wait_stdin(first, "\x1b[200~a.rs:3\n> line 3\nrename this\x1b[201~\r");
    s.core.write(second, b"x").unwrap();
    s.wait_stdin(second, "x");
}

#[test]
fn sent_comments_become_sent_and_the_next_batch_has_only_new_ones() {
    let mut s = setup(done_with_paste_on());
    let session = s.spawn();
    s.wait_state(session, AgentState::Done);
    let first = s.comment("a.rs", 3, 3, "rename this");
    s.skip_events();

    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    s.wait_comments_changed();
    assert_eq!(s.states(), [(first, CommentState::Sent)]);
    let second = s.comment("b.rs", 5, 5, "add a test");
    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();
    s.wait_stdin(
        session,
        "\x1b[200~a.rs:3\n> line 3\nrename this\x1b[201~\r\
         \x1b[200~b.rs:5\n> line 5\nadd a test\x1b[201~\r",
    );
    assert_eq!(
        s.states(),
        [(first, CommentState::Sent), (second, CommentState::Sent)]
    );
    let refused = s.core.send_review_batch(&s.worktree, Some(session));
    assert!(
        matches!(&refused, Err(CoreError::NothingToSend(w)) if *w == s.worktree),
        "{refused:?}"
    );
}

#[test]
fn a_sent_comment_follows_its_text_until_it_is_resolved() {
    let mut s = setup(done_with_paste_on());
    let session = s.spawn();
    s.wait_state(session, AgentState::Done);
    let sent = s.comment("a.rs", 3, 3, "rename this");
    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    // Adds two lines above it.
    fs::write(
        s.worktree.join("a.rs"),
        format!("new\nnew\n{}", numbered(20)),
    )
    .unwrap();

    let deadline = Instant::now() + TIMEOUT;
    while s.core.comments(&s.worktree).unwrap()[0].start_line != 5 {
        assert!(Instant::now() < deadline, "the sent comment never moved");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(s.states(), [(sent, CommentState::Sent)]);
    s.skip_events();
    s.core.resolve_comment(sent).unwrap();
    s.wait_comments_changed();
    assert_eq!(s.states(), []);
    let resolved = s.core.resolved_comments(&s.worktree).unwrap();
    assert_eq!(resolved[0].id, sent);
}

// Chat mode.

/// The fake sidecar's first turn, for the first message, then it reads until the core is done.
fn sidecar_done() -> Value {
    json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"send": {"type": "turn_end"}},
        {"wait_message": "never"},
    ])
}

fn batch_comment(
    path: &str,
    start_line: u32,
    end_line: u32,
    text: &str,
    body: &str,
) -> BatchComment {
    BatchComment {
        path: PathBuf::from(path),
        start_line,
        end_line,
        text: text.into(),
        body: body.into(),
    }
}

#[test]
fn in_chat_mode_three_comments_on_two_files_reach_the_agent_as_one_message_and_show_as_a_review_batch()
 {
    let mut s = setup(sidecar_done());
    let session = s.spawn_chat(Agent::Claude);
    s.wait_state(session, AgentState::Done);
    s.comment("a.rs", 3, 3, "rename this");
    s.comment("b.rs", 5, 5, "add a test");
    s.comment("a.rs", 10, 11, "split this\ninto two");

    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    assert_eq!(
        s.wait_user_messages(session, 2),
        [
            "hi",
            "a.rs:3\n> line 3\nrename this\n\n\
             b.rs:5\n> line 5\nadd a test\n\n\
             a.rs:10-11\n> line 10\n> line 11\nsplit this\ninto two",
        ]
    );
    let conversation = s.conversation(session);
    assert_eq!(
        conversation.last(),
        Some(&AgentMessage::ReviewBatch {
            comments: vec![
                batch_comment("a.rs", 3, 3, "line 3", "rename this"),
                batch_comment("b.rs", 5, 5, "line 5", "add a test"),
                batch_comment("a.rs", 10, 11, "line 10\nline 11", "split this\ninto two"),
            ]
        })
    );
    assert!(
        !conversation[..conversation.len() - 1]
            .iter()
            .any(|m| matches!(m, AgentMessage::ReviewBatch { .. })),
        "{conversation:?}"
    );
}

#[test]
fn in_chat_mode_control_characters_in_any_part_arrive_as_plain_text() {
    let mut s = setup(sidecar_done());
    let session = s.spawn_chat(Agent::Claude);
    s.wait_state(session, AgentState::Done);
    let path = "x\x1b[201~.rs";
    fs::write(
        s.worktree.join(path),
        "let s = \"\x1b[201~\x03\r\";\tok\n\x7fdone\u{9b}\n",
    )
    .unwrap();
    s.comment(path, 1, 2, "why \x1b[31mred\x00?\tkeep tabs\nand newlines");

    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    assert_eq!(
        s.wait_user_messages(session, 2)[1],
        "x[201~.rs:1-2\n> let s = \"[201~\";\tok\n> done\n\
         why [31mred?\tkeep tabs\nand newlines",
    );
    assert_eq!(
        s.conversation(session).last(),
        Some(&AgentMessage::ReviewBatch {
            comments: vec![batch_comment(
                "x[201~.rs",
                1,
                2,
                "let s = \"[201~\";\tok\ndone",
                "why [31mred?\tkeep tabs\nand newlines",
            )]
        })
    );
}

// Plays `steps` in the fake sidecar after its first message, waits for `state`, and checks a
// batch to the session is refused, marks nothing `Sent`, and never reaches the agent.
fn refused_in_chat_mode_while(steps: &[Value], state: AgentState) {
    let mut all = vec![
        json!({"wait_message": "user_message"}),
        json!({"send": {"type": "turn_start"}}),
    ];
    all.extend_from_slice(steps);
    all.push(json!({"wait_message": "never"}));
    let mut s = setup(Value::Array(all));
    let session = s.spawn_chat(Agent::Claude);
    s.wait_state(session, state);
    let comment = s.comment("a.rs", 3, 3, "rename this");

    let refused = s.core.send_review_batch(&s.worktree, Some(session));

    assert!(
        matches!(
            refused,
            Err(CoreError::AgentNotReady { session: to, state: Some(now) })
                if (to, now) == (session, state)
        ),
        "{refused:?}"
    );
    assert_eq!(s.states(), [(comment, CommentState::Open)]);
    // Messages reach the fake in order, so a batch sent before this would show first.
    s.core.send_message(session, "next").unwrap();
    assert_eq!(s.wait_user_messages(session, 2), ["hi", "next"]);
    assert!(
        !s.conversation(session)
            .iter()
            .any(|m| matches!(m, AgentMessage::ReviewBatch { .. }))
    );
}

#[test]
fn in_chat_mode_a_working_session_is_refused_and_gets_nothing() {
    refused_in_chat_mode_while(&[], AgentState::Working);
}

#[test]
fn in_chat_mode_a_session_that_needs_input_is_refused_and_gets_nothing() {
    let approval = json!({"send": {"type": "approval_request", "id": "toolu_01", "tool": "Bash",
                                   "input": {"command": "npm test"}}});
    let needs_input = AgentState::NeedsInput {
        kind: InputKind::Approval,
    };
    refused_in_chat_mode_while(&[approval], needs_input);
}

#[test]
fn in_chat_mode_sent_comments_become_sent_and_the_next_batch_has_only_new_ones() {
    let mut s = setup(sidecar_done());
    let session = s.spawn_chat(Agent::Claude);
    s.wait_state(session, AgentState::Done);
    let first = s.comment("a.rs", 3, 3, "rename this");
    s.skip_events();

    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    s.wait_comments_changed();
    assert_eq!(s.states(), [(first, CommentState::Sent)]);
    let second = s.comment("b.rs", 5, 5, "add a test");
    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();
    assert_eq!(
        s.wait_user_messages(session, 3),
        [
            "hi",
            "a.rs:3\n> line 3\nrename this",
            "b.rs:5\n> line 5\nadd a test"
        ]
    );
    assert_eq!(
        s.states(),
        [(first, CommentState::Sent), (second, CommentState::Sent)]
    );
    let refused = s.core.send_review_batch(&s.worktree, Some(session));
    assert!(
        matches!(&refused, Err(CoreError::NothingToSend(w)) if *w == s.worktree),
        "{refused:?}"
    );
}

#[test]
fn in_codex_chat_mode_the_batch_starts_one_turn_and_shows_as_a_review_batch() {
    let notify = |method: &str, status: &str| {
        json!({"send": {"method": method, "params": {"threadId": "{{thread_id}}",
               "turn": {"id": "{{turn_id}}", "items": [], "status": status, "error": null}}}})
    };
    let mut s = setup(json!([
        {"wait_message": "turn/start"},
        notify("turn/started", "inProgress"),
        notify("turn/completed", "completed"),
        {"wait_message": "never"},
    ]));
    let session = s.spawn_chat(Agent::Codex);
    s.wait_state(session, AgentState::Done);
    let comment = s.comment("a.rs", 3, 3, "rename this");
    s.comment("b.rs", 5, 5, "add a test");

    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    let starts = s.wait_lines(session, 2, |line| line["method"] == "turn/start");
    let texts: Vec<&Value> = starts
        .iter()
        .map(|l| &l["params"]["input"][0]["text"])
        .collect();
    assert_eq!(
        texts,
        [
            "hi",
            "a.rs:3\n> line 3\nrename this\n\nb.rs:5\n> line 5\nadd a test"
        ]
    );
    assert_eq!(
        s.conversation(session).last(),
        Some(&AgentMessage::ReviewBatch {
            comments: vec![
                batch_comment("a.rs", 3, 3, "line 3", "rename this"),
                batch_comment("b.rs", 5, 5, "line 5", "add a test"),
            ]
        })
    );
    assert_eq!(s.states()[0], (comment, CommentState::Sent));
}

/// The agent's first turn, for the first message, then it exits once it reads the next one,
/// before it starts a turn on it.
fn exits_on_the_second_message(agent: Agent) -> Value {
    match agent {
        Agent::Claude => json!([
            {"wait_message": "user_message"},
            {"send": {"type": "turn_start"}},
            {"send": {"type": "turn_end"}},
            {"wait_message": "user_message"},
            {"exit": 0},
        ]),
        Agent::Codex => {
            let notify = |method: &str, status: &str| {
                json!({"send": {"method": method, "params": {"threadId": "{{thread_id}}",
                       "turn": {"id": "{{turn_id}}", "items": [], "status": status, "error": null}}}})
            };
            json!([
                {"wait_message": "turn/start"},
                notify("turn/started", "inProgress"),
                notify("turn/completed", "completed"),
                {"wait_message": "turn/start"},
                {"exit": 0},
            ])
        }
    }
}

fn a_batch_the_agent_exits_before_starting_on_is_not_delivered_and_its_comments_open_again(
    agent: Agent,
) {
    let mut s = setup(exits_on_the_second_message(agent));
    let session = s.spawn_chat(agent);
    s.wait_state(session, AgentState::Done);
    let comment = s.comment("a.rs", 3, 3, "rename this");

    s.core
        .send_review_batch(&s.worktree, Some(session))
        .unwrap();

    s.wait_exit(session);
    assert_eq!(s.states(), [(comment, CommentState::Open)]);
    let conversation = s.conversation(session);
    assert_eq!(
        conversation[conversation.len() - 2..],
        [
            AgentMessage::ReviewBatch {
                comments: vec![batch_comment("a.rs", 3, 3, "line 3", "rename this")]
            },
            AgentMessage::NotDelivered {
                text: "a.rs:3\n> line 3\nrename this".into()
            },
        ]
    );
}

#[test]
fn in_claude_chat_mode_a_batch_the_agent_never_started_on_puts_its_comments_back_to_open() {
    a_batch_the_agent_exits_before_starting_on_is_not_delivered_and_its_comments_open_again(
        Agent::Claude,
    );
}

#[test]
fn in_codex_chat_mode_a_batch_the_agent_never_started_on_puts_its_comments_back_to_open() {
    a_batch_the_agent_exits_before_starting_on_is_not_delivered_and_its_comments_open_again(
        Agent::Codex,
    );
}

#[test]
fn a_session_switching_mode_is_refused_and_its_comments_stay_open() {
    let mut s = setup(sidecar_done());
    let session = s.spawn_chat(Agent::Claude);
    s.wait_state(session, AgentState::Done);
    let comment = s.comment("a.rs", 3, 3, "rename this");

    let refused = block_on(async {
        // Polled once, so the switch waits for the old process to exit and is still running.
        let mut switch = std::pin::pin!(s.core.set_mode(session, Mode::Terminal));
        let pending =
            std::future::poll_fn(|cx| Poll::Ready(switch.as_mut().poll(cx).is_pending())).await;
        assert!(pending, "set_mode did not wait for the exit");
        s.core.send_review_batch(&s.worktree, Some(session))
    });

    assert!(
        matches!(refused, Err(CoreError::SwitchingMode(to)) if to == session),
        "{refused:?}"
    );
    assert_eq!(s.states(), [(comment, CommentState::Open)]);
}
