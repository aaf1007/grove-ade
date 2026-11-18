//! Runs grove's core with no window, for the `verify` skill's headless surface. Start it through
//! `.claude/skills/verify/headless.py`, which builds the run folder this reads:
//!
//! ```text
//! <run>/home/           HOME, so the core's state is <run>/home/.grovecode/
//! <run>/bin/            claude, codex and claude-sidecar, wrappers around fixtures/fake-agents
//! <run>/control.sock    one JSON request per connection, one JSON reply
//! <run>/evidence/       events.log, and session-<id>.log for each session it follows
//! ```
//!
//! A request is `{"cmd": "spawn_agent", ...}`, and the reply `{"ok": ...}` or `{"error": ...}`.
//! Types the core does not serialize come back in their Debug form. `headless.py` lists the
//! commands.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::{env, thread};

use grove_core::{
    Agent, AgentSpec, Core, CoreConfig, CoreEvent, DiffBase, FileDiff, LineKind, Mode,
    NewReviewComment, NewWorktree, PanelSpec, Reply, RequestId, SessionId,
};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::unbounded_channel;

type Failure = Box<dyn std::error::Error>;

fn main() -> Result<(), Failure> {
    let run = PathBuf::from(env::args().nth(1).ok_or("usage: headless <run folder>")?);
    let run = fs::canonicalize(run)?;
    // The owner's real ~/.grovecode must never be touched, so HOME has to be the run's own.
    let home = run.join("home");
    if env::var_os("HOME").map(PathBuf::from) != Some(home.clone()) {
        return Err(format!("HOME must be {}", home.display()).into());
    }
    let evidence = run.join("evidence");
    fs::create_dir_all(&evidence)?;
    // `cargo build --examples` puts this in target/<profile>/examples/, next to the binaries.
    let target = env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .ok_or("no target folder")?
        .to_path_buf();

    let mut cfg = CoreConfig::for_user().ok_or("no home folder")?;
    cfg.claude_command = run.join("bin/claude");
    cfg.codex_command = run.join("bin/codex");
    cfg.claude_sidecar_command = run.join("bin/claude-sidecar");
    cfg.mcp_server = target.join("grove-mcp");
    cfg.hook_relay_source = Some(target.join("grove-hook"));
    // The fakes need no login PATH, and skipping the read keeps every run the same.
    cfg.login_shell = None;
    let socket = cfg.socket.clone();
    let (core, events) = Core::start(cfg)?;
    let core = Arc::new(core);
    let started = Instant::now();
    // Log lines count seconds from `started`; this is the same moment as Unix time.
    let started_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64());
    let log = Logs {
        evidence: evidence.clone(),
        started,
    };

    thread::spawn({
        let log = log.clone();
        let core = Arc::downgrade(&core);
        let mut events = events;
        move || {
            loop {
                match events.blocking_recv() {
                    Ok(event) => {
                        log.line("events.log", &format!("{event:?}"));
                        // An agent's panel request starts a session nobody follows yet.
                        if let CoreEvent::PanelRequested { panel, .. } = event {
                            let session = match panel {
                                PanelSpec::Agent { session, .. }
                                | PanelSpec::Log { session, .. }
                                | PanelSpec::Command { session, .. } => session,
                            };
                            if let Some(core) = core.upgrade() {
                                follow(&core, &log, session);
                            }
                        }
                    }
                    Err(RecvError::Lagged(n)) => log.line("events.log", &format!("Lagged({n})")),
                    Err(RecvError::Closed) => break,
                }
            }
        }
    });

    let control = run.join("control.sock");
    let _ = fs::remove_file(&control);
    let listener = UnixListener::bind(&control)?;
    // headless.py waits for this line before it says the run is ready.
    log.line(
        "events.log",
        &format!(
            "ready wall={started_unix:.3} home={} socket={}",
            home.display(),
            socket.display()
        ),
    );
    // set_mode back to chat mode waits for the history with a timeout, so it needs the time driver.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        let request = read_request(&mut stream);
        let quit = request.as_ref().is_ok_and(|r| r["cmd"] == "quit");
        let reply = match request.and_then(|r| handle(&core, &runtime, &log, &r)) {
            Ok(value) => json!({ "ok": value }),
            Err(error) => json!({ "error": error.to_string() }),
        };
        let _ = writeln!(stream, "{reply}");
        if quit {
            break;
        }
    }
    let _ = fs::remove_file(&control);
    // Dropping the core stops it, which ends every session it started.
    drop(core);
    log.line("events.log", "stopped");
    Ok(())
}

fn read_request(stream: &mut UnixStream) -> Result<Value, Failure> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

fn handle(
    core: &Arc<Core>,
    runtime: &tokio::runtime::Runtime,
    log: &Logs,
    request: &Value,
) -> Result<Value, Failure> {
    let text = |key: &str| -> Result<String, Failure> {
        request[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{key} must be a string").into())
    };
    let path = |key: &str| text(key).map(PathBuf::from);
    let session = || -> Result<SessionId, Failure> {
        Ok(serde_json::from_value(request["session"].clone())
            .map_err(|_| "session must be a session id")?)
    };
    let mode = |value: &str| -> Result<Mode, Failure> {
        Ok(serde_json::from_value(json!(value)).map_err(|_| "mode is chat or terminal")?)
    };
    let cmd = text("cmd")?;
    Ok(match cmd.as_str() {
        "ping" => json!({ "home": env::var("HOME")?, "pid": std::process::id() }),
        "quit" => json!("stopping"),
        "add_repo" => {
            runtime.block_on(core.add_repo(&path("path")?))?;
            json!("added")
        }
        "worktrees" => {
            let worktrees = runtime.block_on(core.list_worktrees());
            json!(
                worktrees
                    .iter()
                    .map(|w| format!("{w:?}"))
                    .collect::<Vec<_>>()
            )
        }
        "create_worktree" => {
            let new = NewWorktree {
                branch: text("branch")?,
                ..NewWorktree::default()
            };
            let added = runtime.block_on(core.create_worktree(&path("repo")?, new))?;
            json!(added.worktree.path)
        }
        "spawn_agent" => {
            let agent = match text("agent")?.as_str() {
                "claude" => Agent::Claude,
                "codex" => Agent::Codex,
                _ => return Err("agent is claude or codex".into()),
            };
            let spec = AgentSpec {
                agent,
                mode: mode(&text("mode")?)?,
                prompt: request["prompt"].as_str().map(str::to_owned),
                cols: 120,
                rows: 40,
                panel: None,
            };
            let session = core.spawn_agent(&path("worktree")?, spec)?;
            follow(core, log, session);
            json!(session)
        }
        "spawn_shell" => {
            let session = core.spawn_shell(&path("worktree")?, 120, 40)?;
            follow(core, log, session);
            json!(session)
        }
        // A session the core restored at start has nobody following it yet.
        "follow" => {
            follow(core, log, session()?);
            json!("following")
        }
        "send" => {
            core.send_message(session()?, &text("text")?)?;
            json!("sent")
        }
        "answer" => {
            let reply = match &request["reply"] {
                Value::String(s) if s == "allow_once" => Reply::AllowOnce,
                Value::String(s) if s == "allow_session" => Reply::AllowForSession,
                Value::String(s) if s == "deny" => Reply::Deny { message: None },
                Value::Array(_) => {
                    Reply::Answers(serde_json::from_value(request["reply"].clone())?)
                }
                _ => return Err("reply is allow_once, allow_session, deny or [[answers]]".into()),
            };
            core.answer(session()?, RequestId(text("request")?), reply)?;
            json!("answered")
        }
        "interrupt" => {
            core.interrupt(session()?)?;
            json!("interrupted")
        }
        "set_model" => {
            let effort = request["effort"].as_str();
            core.set_model(session()?, &text("model")?, effort)?;
            json!("set")
        }
        "set_mode" => {
            let session = session()?;
            runtime.block_on(core.set_mode(session, mode(&text("mode")?)?))?;
            // The other mode has its own stream, so follow it again.
            follow(core, log, session);
            json!("switched")
        }
        "write" => {
            core.write(session()?, text("text")?.as_bytes())?;
            json!("written")
        }
        "kill" => {
            core.kill(session()?)?;
            json!("killed")
        }
        "diff" => {
            let base = match text("base")?.as_str() {
                "head" => DiffBase::Head,
                "merge_base" => DiffBase::MergeBase,
                _ => return Err("base is head or merge_base".into()),
            };
            let files = runtime.block_on(core.diff(&path("worktree")?, base))?;
            json!(files.iter().map(file_diff).collect::<Vec<_>>())
        }
        "workspace" => json!(format!("{:?}", core.workspace(&path("worktree")?)?)),
        "undo_workspace" => {
            runtime.block_on(core.undo_workspace(&path("worktree")?))?;
            json!("undone")
        }
        "add_comment" => {
            let line = |key: &str| -> Result<u32, Failure> {
                Ok(request[key]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or(format!("{key} must be a line number"))?)
            };
            let new = NewReviewComment {
                path: path("path")?,
                start_line: line("start_line")?,
                end_line: line("end_line")?,
                body: text("body")?,
            };
            let id = runtime.block_on(core.add_comment(&path("worktree")?, new))?;
            json!(format!("{id:?}"))
        }
        "comments" => json!(format!("{:?}", core.comments(&path("worktree")?)?)),
        "send_review_batch" => {
            let to = match request.get("session") {
                Some(_) => Some(session()?),
                None => None,
            };
            core.send_review_batch(&path("worktree")?, to)?;
            json!("sent")
        }
        "profiles" => json!(format!("{:?}", core.profiles()?)),
        "use_profile" => {
            let repo = path("repo")?;
            let mut settings = core.repo_settings(&repo)?;
            settings.profile = Some(text("name")?);
            runtime.block_on(core.set_repo_settings(&repo, settings))?;
            json!("set")
        }
        "suggest_profile" => json!(format!("{:?}", core.suggest_profile(&path("repo")?))),
        "attention" => json!(format!("{:?}", core.attention_queue())),
        "sessions" => json!(format!("{:?}", core.sessions(&path("worktree")?)?)),
        _ => return Err(format!("unknown cmd {cmd}").into()),
    })
}

/// A file's diff as JSON, with each line as text after a `+`, `-` or space, as `git diff` shows it.
fn file_diff(file: &FileDiff) -> Value {
    let hunks: Vec<Value> = file
        .hunks
        .iter()
        .map(|hunk| {
            let lines: Vec<String> = hunk
                .lines
                .iter()
                .map(|line| {
                    let sign = match line.kind {
                        LineKind::Context => ' ',
                        LineKind::Added => '+',
                        LineKind::Removed => '-',
                    };
                    format!("{sign}{}", String::from_utf8_lossy(&line.text))
                })
                .collect();
            json!({
                "old": [hunk.old_start, hunk.old_lines],
                "new": [hunk.new_start, hunk.new_lines],
                "lines": lines,
            })
        })
        .collect();
    json!({
        "status": format!("{:?}", file.status),
        "old_path": file.old_path,
        "new_path": file.new_path,
        "binary": file.binary,
        "hunks": hunks,
    })
}

/// Logs a session's output to `session-<id>.log` from now on: chat messages in their Debug form
/// for a chat-mode agent session, terminal bytes as they come for anything with a PTY.
fn follow(core: &Arc<Core>, log: &Logs, session: SessionId) {
    let name = format!("session-{}.log", json!(session));
    let (messages, mut chat) = unbounded_channel();
    if let Ok(history) = core.subscribe(session, messages) {
        for message in history {
            log.line(&name, &format!("{message:?}"));
        }
        let log = log.clone();
        thread::spawn(move || {
            while let Some(frame) = chat.blocking_recv() {
                for message in frame {
                    log.line(&name, &format!("{message:?}"));
                }
            }
        });
        return;
    }
    let (frames, mut output) = unbounded_channel();
    let Ok(snapshot) = core.attach(session, frames) else {
        log.line(
            "events.log",
            &format!("could not follow session {session:?}"),
        );
        return;
    };
    log.bytes(&name, &snapshot.bytes);
    let log = log.clone();
    let core: Weak<Core> = Arc::downgrade(core);
    thread::spawn(move || {
        while let Some(frame) = output.blocking_recv() {
            log.bytes(&name, &frame.bytes);
            // Unacked bytes pause the session's reader.
            match core.upgrade() {
                Some(core) => core.ack(session, frame.bytes.len()),
                None => break,
            }
        }
    });
}

#[derive(Clone)]
struct Logs {
    evidence: PathBuf,
    started: Instant,
}

impl Logs {
    fn line(&self, file: &str, text: &str) {
        let at = self.started.elapsed().as_secs_f64();
        self.bytes(file, format!("{at:.3} {text}\n").as_bytes());
    }

    fn bytes(&self, file: &str, bytes: &[u8]) {
        let opened: std::io::Result<File> = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.evidence.join(file));
        if let Ok(mut file) = opened {
            let _ = file.write_all(bytes);
        }
    }
}
