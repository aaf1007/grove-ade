//! Drives one Claude chat-mode session through the core from a terminal, for the manual
//! acceptance tests until the agent panel lands (#116).
//!
//! ```text
//! cargo build -p grove-hook
//! cargo run -p grove-core --example chat -- <repo> <claude-sidecar>
//! ```
//!
//! Each line you type is a message. While a request waits, `n` denies the oldest one. For an
//! approval `y` allows it once. For a question the line is the answers: `;` between questions,
//! `,` between the options picked. `/stop` interrupts the turn. Ctrl-D ends the session.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{env, fs, thread};

use grove_core::{
    Agent, AgentMessage, AgentSpec, Core, CoreConfig, CoreError, CoreEvent, Mode, Reply, RequestId,
};
use tokio::sync::mpsc::unbounded_channel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let [repo, sidecar] = args.as_slice() else {
        return Err("usage: chat <repo> <claude-sidecar>".into());
    };
    let repo = fs::canonicalize(repo)?;
    // A home of its own leaves the app's state alone. macOS caps the socket path at 104 bytes.
    let home = tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")?;
    let mut cfg = CoreConfig::new(home.path());
    cfg.login_shell = Some(env::var_os("SHELL").map_or("/bin/zsh".into(), PathBuf::from));
    cfg.claude_sidecar_command = fs::canonicalize(sidecar)?;
    // `cargo build -p grove-hook` puts it one folder above this example.
    cfg.hook_relay_source = env::current_exe()?
        .parent()
        .and_then(|examples| examples.parent())
        .map(|target| target.join("grove-hook"))
        .filter(|relay| relay.exists());
    let (core, mut events) = Core::start(cfg)?;
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    runtime.block_on(core.add_repo(&repo))?;
    let worktree = runtime
        .block_on(core.list_worktrees())
        .into_iter()
        .map(|w| w.path)
        .find(|path| *path == repo)
        .ok_or("not a repo's main worktree")?;
    let spec = AgentSpec {
        agent: Some(Agent::Claude),
        mode: Mode::Chat,
        prompt: None,
        cols: 120,
        rows: 40,
    };
    let session = core.spawn_agent(&worktree, spec)?;

    thread::spawn(move || {
        while let Ok(event) = events.blocking_recv() {
            if let CoreEvent::Account { .. } | CoreEvent::AgentState { .. } = event {
                eprintln!("\n[{event:?}]");
            }
        }
    });
    let pending = Arc::new(Mutex::new(Vec::<Pending>::new()));
    let (sink, mut frames) = unbounded_channel();
    core.subscribe(session, sink)?;
    thread::spawn({
        let pending = pending.clone();
        move || {
            while let Some(frame) = frames.blocking_recv() {
                for message in frame {
                    show(&message, &pending);
                }
            }
        }
    });

    // Its Answered message comes a frame later, so the next line could pick it again.
    let answer = |p: Pending, reply| -> Result<(), CoreError> {
        core.answer(session, p.request.clone(), reply)?;
        if let Ok(mut pending) = pending.lock() {
            pending.retain(|q| q.request != p.request);
        }
        Ok(())
    };
    for line in io::stdin().lock().lines() {
        let line = line?;
        let oldest = pending.lock().map_err(|_| "poisoned")?.first().cloned();
        let result = match (line.as_str(), oldest) {
            ("/stop", _) => core.interrupt(session),
            ("n", Some(p)) => answer(p, Reply::Deny { message: None }),
            ("y", Some(p)) if !p.question => answer(p, Reply::AllowOnce),
            (text, Some(p)) if p.question => answer(p, answers(text)),
            (text, _) => core.send_message(session, text),
        };
        if let Err(error) = result {
            eprintln!("[{error}]");
        }
    }
    core.kill(session)?;
    Ok(())
}

#[derive(Clone)]
struct Pending {
    request: RequestId,
    question: bool,
}

/// `vitest; unit, integration` as one answer per question.
fn answers(line: &str) -> Reply {
    let picks = |answer: &str| {
        answer
            .split(',')
            .map(|pick| pick.trim().to_owned())
            .collect()
    };
    Reply::Answers(line.split(';').map(picks).collect())
}

fn show(message: &AgentMessage, pending: &Mutex<Vec<Pending>>) {
    let Ok(mut pending) = pending.lock() else {
        return;
    };
    match message {
        AgentMessage::TextDelta { text } => print!("{text}"),
        AgentMessage::ApprovalRequested { request, tool, .. } => {
            pending.push(Pending {
                request: request.clone(),
                question: false,
            });
            println!("\n[allow {tool}? y/n]");
        }
        AgentMessage::QuestionAsked { request, .. } => {
            pending.push(Pending {
                request: request.clone(),
                question: true,
            });
            println!("\n[{message:?}]\n[type the answers, or n]");
        }
        AgentMessage::Answered { request, .. } => pending.retain(|p| &p.request != request),
        AgentMessage::TurnEnded { .. } => {
            pending.clear();
            println!("\n[{message:?}]");
        }
        _ => println!("\n[{message:?}]"),
    }
    let _ = io::stdout().flush();
}
