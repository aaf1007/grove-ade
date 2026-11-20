//! Agent sessions in chat mode: the agent's messages, the conversation so far, and its frames
//! to the subscribed panel. Claude runs through its sidecar, whose protocol is in
//! sidecar/claude/PROTOCOL.md. Codex runs `codex app-server` (codex.rs).

mod codex;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::mem;
use std::path::Path;
use std::process::{ChildStdout, Command};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Notify, broadcast, watch};

use crate::agent::{Agents, ChatSpec};
use crate::comments::{BatchComment, CommentId};
use crate::session::{Batch, Flushed, flush_batches, start_thread};
use crate::{
    AccountInfo, Agent, AgentState, Core, CoreError, CoreEvent, InputKind, Result, SessionId,
};

/// One message of a chat-mode conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentMessage {
    /// What the user sent.
    User { text: String },
    /// The agent started working on a message.
    TurnStarted {
        /// When the core got it, or when the agent's record says it started. None when a record
        /// does not say.
        time_unix_ms: Option<u64>,
    },
    /// The next piece of the agent's reply text.
    TextDelta { text: String },
    /// The agent called a tool. `id` is the agent's id for the call, which an approval request
    /// for it carries too.
    ToolCall {
        id: String,
        tool: String,
        /// The tool's input, as the agent gave it.
        input: Value,
        /// When the core got it, or when the agent's record says it was made. None when a record
        /// does not say, as Codex's never does.
        time_unix_ms: Option<u64>,
    },
    /// What a tool call returned, by the call's `id`.
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    /// The agent waits for the user to allow or deny a tool call.
    ApprovalRequested {
        request: RequestId,
        tool: String,
        /// The tool's input, as the agent gave it.
        input: Value,
    },
    /// The agent asks the user one or more questions.
    QuestionAsked {
        request: RequestId,
        questions: Vec<Question>,
    },
    /// The user answered a request.
    Answered { request: RequestId, reply: Reply },
    /// The turn ended. Its requests that were still pending ended with it.
    TurnEnded {
        outcome: TurnOutcome,
        /// When the core got it, or when the agent's record says it ended. None when a record
        /// does not say.
        time_unix_ms: Option<u64>,
    },
    /// A review batch the user sent. The agent got it as one message of plain text.
    ReviewBatch { comments: Vec<BatchComment> },
    /// A message the user sent never reached the agent: its process ended before the agent
    /// started a turn on it. `text` is what the agent would have got. A review batch's comments
    /// are open again.
    NotDelivered { text: String },
}

/// A model an agent offers, as the agent lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelInfo {
    /// What `set_model` takes.
    pub id: String,
    pub name: String,
    pub description: String,
    /// The efforts it takes, in the agent's order. Empty when it takes none.
    pub efforts: Vec<String>,
    /// None when the agent does not say.
    pub default_effort: Option<String>,
    /// The agent's own default.
    pub is_default: bool,
}

/// The model and effort an agent session runs with, as `set_model` last got them. Saved with
/// the session, and applied on every launch in either mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    /// A `ModelInfo` id.
    pub model: String,
    /// None leaves the agent's own effort.
    pub effort: Option<String>,
}

/// Where a session's model list stands.
#[derive(Debug, Clone)]
enum Listing {
    Waiting,
    Listed(Vec<ModelInfo>),
    /// The process ended before it listed them.
    Closed,
}

/// The id the sidecar gives an approval request or a question.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One question of a `QuestionAsked`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Question {
    pub question: String,
    /// A short label for it, such as "Library".
    pub header: String,
    pub options: Vec<QuestionOption>,
    /// The user may pick more than one option.
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// The user's answer to a pending request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Approvals only.
    AllowOnce,
    /// Approvals only. The agent stops asking for calls like this one for the rest of the session.
    AllowForSession,
    /// Either kind. `message` tells the agent why, or what to do instead.
    Deny { message: Option<String> },
    /// Questions only. One answer per question, in order: the chosen options' labels, or the
    /// user's own text.
    Answers(Vec<Vec<String>>),
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    /// `interrupt` stopped it.
    Interrupted,
    /// It ended on an error. `kind` is the agent's own name for it, such as "rate_limit".
    Failed {
        kind: String,
        message: String,
    },
}

/// Receives a chat-mode session's messages, a frame at a time, in order.
pub type MessageSink = UnboundedSender<Vec<AgentMessage>>;

/// How long a sidecar's exit waits for the core to read what it wrote before it exited. A
/// process it started can hold its stdout open after it exits.
const DRAIN: Duration = Duration::from_millis(500);

/// A chat-mode session's conversation. The reader thread adds to it and the flush task sends the
/// batch.
pub(crate) struct Chat {
    agent: Agent,
    state: Mutex<ChatState>,
    // Wakes the flush task when a message comes for an empty batch.
    arrived: Notify,
    models: watch::Sender<Listing>,
    // True once the conversation so far is read back from the agent's record, or the process
    // ended first. Only a resumed conversation waits for it.
    history_read: watch::Sender<bool>,
}

#[derive(Default)]
struct ChatState {
    // Every message so far. The conversation outlives the process, so a panel opened later
    // still shows it.
    history: Vec<AgentMessage>,
    sink: Option<MessageSink>,
    // Added for the sink and not in a frame yet.
    batch: Vec<AgentMessage>,
    // The reader stopped, so the flush task sends the last frame and ends.
    closed: bool,
    // Requests of the current turn that wait for an answer, oldest first.
    pending: Vec<Pending>,
    // Messages sent that no turn has started on yet, oldest first.
    unstarted: VecDeque<Unstarted>,
    // `set_mode` is ending the process, so no message may go to it.
    ending: bool,
    // Some for Codex, which speaks JSON-RPC.
    app_server: Option<codex::AppServer>,
    // The session's saved choice, which the process gets before anything else.
    first_model: Option<ModelChoice>,
}

/// A message sent that no turn has started on yet.
struct Unstarted {
    text: String,
    // A review batch's comments, which open again if it never reaches the agent.
    comments: Vec<CommentId>,
}

struct Pending {
    request: RequestId,
    // None for an approval, else how many questions it asks.
    questions: Option<usize>,
    // How Codex takes the answer. None for the sidecar.
    reply_to: Option<codex::ReplyTo>,
}

impl ChatState {
    /// The state the pending requests leave the session in. An approval ranks above a question.
    fn waiting(&self) -> AgentState {
        let kind = if self.pending.iter().any(|p| p.questions.is_none()) {
            InputKind::Approval
        } else if !self.pending.is_empty() {
            InputKind::Question
        } else {
            return AgentState::Working;
        };
        AgentState::NeedsInput { kind }
    }

    fn is_pending(&self, request: &RequestId) -> bool {
        self.pending.iter().any(|p| &p.request == request)
    }

    /// Sets the model and effort from the next turn on. Codex holds them for its next
    /// `turn/start`. Fails when the process is gone.
    fn set_model(
        &mut self,
        input: &mpsc::Sender<Vec<u8>>,
        choice: &ModelChoice,
    ) -> std::result::Result<(), mpsc::SendError<Vec<u8>>> {
        let effort = choice.effort.as_deref();
        match &mut self.app_server {
            Some(server) => {
                server.set_model(&choice.model, effort);
                Ok(())
            }
            None => input.send(set_model_line(&choice.model, effort).into_bytes()),
        }
    }
}

impl Chat {
    /// `thread` is the Codex thread to resume. Claude takes the conversation to resume in its
    /// launch flags. `model` is the session's saved choice.
    pub(crate) fn new(agent: Agent, thread: Option<String>, model: Option<ModelChoice>) -> Self {
        let state = ChatState {
            app_server: (agent == Agent::Codex).then(|| codex::AppServer::new(thread)),
            first_model: model,
            ..ChatState::default()
        };
        Self {
            agent,
            state: Mutex::new(state),
            arrived: Notify::default(),
            models: watch::Sender::new(Listing::Waiting),
            history_read: watch::Sender::new(false),
        }
    }

    /// Waits until the conversation so far is read back from the agent's record, or the process
    /// ended first.
    pub(crate) async fn history_read(&self) {
        // Fails only when the sender is gone, and `self` holds it.
        let _ = self.history_read.subscribe().wait_for(|read| *read).await;
    }

    fn state(&self) -> MutexGuard<'_, ChatState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push_locked(&self, s: &mut ChatState, message: AgentMessage) {
        if s.sink.is_some() {
            s.batch.push(message.clone());
            if s.batch.len() == 1 {
                self.arrived.notify_one();
            }
        }
        s.history.push(message);
    }

    /// Sends the messages from now on to `sink`, in place of any earlier sink, and returns the
    /// ones before. Under one lock, so each message is in one of the two exactly once.
    fn subscribe(&self, sink: MessageSink) -> Vec<AgentMessage> {
        let mut s = self.state();
        // The history has the batch, which was for the earlier sink.
        s.batch.clear();
        s.sink = Some(sink);
        s.history.clone()
    }

    /// Queues the user's message for the agent and adds `text` to the conversation, before any
    /// reply can. Codex holds it until its thread starts and the turn before ends.
    fn send(&self, session: SessionId, input: &mpsc::Sender<Vec<u8>>, text: &str) -> Result<()> {
        let shown = AgentMessage::User {
            text: text.to_owned(),
        };
        self.send_as(session, input, text, shown, Vec::new())
    }

    /// `send`, with `shown` in the conversation in place of `text`. `comments` open again if the
    /// message never reaches the agent. Fails once the process has ended, or while `set_mode`
    /// ends it.
    pub(crate) fn send_as(
        &self,
        session: SessionId,
        input: &mpsc::Sender<Vec<u8>>,
        text: &str,
        shown: AgentMessage,
        comments: Vec<CommentId>,
    ) -> Result<()> {
        let mut s = self.state();
        if s.closed {
            return Err(CoreError::SessionNotRunning(session));
        }
        if s.ending {
            return Err(CoreError::SwitchingMode(session));
        }
        let line = match &mut s.app_server {
            None => Some(user_message(text).into_bytes()),
            Some(server) => server.user_message(text),
        };
        if let Some(line) = line
            && input.send(line).is_err()
        {
            return Err(CoreError::SessionNotRunning(session));
        }
        s.unstarted.push_back(Unstarted {
            text: text.to_owned(),
            comments,
        });
        self.push_locked(&mut s, shown);
        Ok(())
    }

    /// Stops sends to the process, which `set_mode` is about to end. False, and sends go on,
    /// while a message sent to it has no turn started yet, as ending it would lose the message.
    pub(crate) fn end_sends(&self) -> bool {
        let mut s = self.state();
        s.ending = s.unstarted.is_empty();
        s.ending
    }

    /// The process ended. Each message it never started a turn on shows as not delivered.
    /// Returns the comments of the review batches among them.
    fn close(&self) -> Vec<CommentId> {
        let mut s = self.state();
        s.closed = true;
        let mut comments = Vec::new();
        for unstarted in mem::take(&mut s.unstarted) {
            let text = unstarted.text;
            self.push_locked(&mut s, AgentMessage::NotDelivered { text });
            comments.extend(unstarted.comments);
        }
        drop(s);
        self.arrived.notify_one();
        self.history_read.send_replace(true);
        self.models.send_if_modified(|listing| {
            let waiting = matches!(listing, Listing::Waiting);
            if waiting {
                *listing = Listing::Closed;
            }
            waiting
        });
        comments
    }
}

impl Batch for Chat {
    async fn wait(&self) {
        self.arrived.notified().await;
    }

    fn flush(&self) -> Flushed {
        let mut guard = self.state();
        let s = &mut *guard;
        let mut flushed = Flushed::Nothing;
        if let Some(sink) = &s.sink
            && !s.batch.is_empty()
        {
            flushed = Flushed::Frame;
            if sink.send(mem::take(&mut s.batch)).is_err() {
                // The receiver is gone, so the sink is detached.
                s.sink = None;
            }
        }
        if s.closed { Flushed::Closed } else { flushed }
    }
}

/// What a line from the agent's process says: a line the sidecar writes, as PROTOCOL.md lists
/// them, or what codex.rs reads from the app server.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FromAgent {
    Account {
        account: Option<AccountInfo>,
    },
    Models {
        models: Vec<ModelInfo>,
    },
    /// `time_unix_ms` is set only in a `history`, from the agent's record. Live, the core
    /// stamps the time it got the message.
    TurnStart {
        #[serde(default)]
        time_unix_ms: Option<u64>,
    },
    TextDelta {
        text: String,
    },
    ToolCall {
        id: String,
        tool: String,
        input: Value,
        #[serde(default)]
        time_unix_ms: Option<u64>,
    },
    ToolResult {
        id: String,
        output: String,
        #[serde(default)]
        is_error: bool,
    },
    ApprovalRequest {
        id: RequestId,
        tool: String,
        input: Value,
        #[serde(skip)]
        reply_to: Option<codex::ReplyTo>,
    },
    QuestionRequest {
        id: RequestId,
        questions: Vec<Question>,
        #[serde(skip)]
        reply_to: Option<codex::ReplyTo>,
    },
    TurnEnd {
        #[serde(default)]
        interrupted: bool,
        error: Option<TurnError>,
        #[serde(default)]
        time_unix_ms: Option<u64>,
    },
    /// The conversation so far, from the agent's own record: `user` messages and the turns'
    /// messages. Sent first when a conversation resumes.
    History {
        messages: Vec<FromAgent>,
    },
    /// What the user sent, in a `history`.
    User {
        text: String,
    },
    /// Codex started or resumed the thread with this id.
    #[serde(skip)]
    Thread {
        id: String,
    },
}

#[derive(Deserialize)]
struct TurnError {
    kind: String,
    message: String,
}

impl Core {
    /// Starts the agent's chat-mode process for session `session`, with `chat` holding its
    /// conversation. `prompt` is queued as the first message before the process starts, so it
    /// cannot be lost.
    pub(crate) fn start_chat(
        &self,
        session: SessionId,
        worktree: &Path,
        spec: ChatSpec,
        env: &[(&str, OsString)],
        chat: Arc<Chat>,
        prompt: Option<&str>,
    ) -> Result<()> {
        let log = File::create(&spec.stderr).map_err(CoreError::io("create", &spec.stderr))?;
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args).stderr(log);
        let (input, input_rx) = mpsc::channel();
        // The receiver is right here, so these always queue.
        {
            let mut s = chat.state();
            if let Some(server) = &mut s.app_server {
                let _ = input.send(server.opening());
            }
            if let Some(choice) = s.first_model.take() {
                let _ = s.set_model(&input, &choice);
            }
        }
        if let Some(text) = prompt {
            // The writer waits for the process, so this queues.
            let _ = chat.send(session, &input, text);
        }
        let (stdout_tx, stdout_rx) = mpsc::channel::<ChildStdout>();
        // Dropped once the reader has stopped.
        let (read_tx, read_rx) = mpsc::channel::<()>();
        start_thread("grove-chat-reader", {
            let chat = chat.clone();
            let agents = self.agents.clone();
            let events = self.events.clone();
            let comments = self.comments.clone();
            let worktree = worktree.to_path_buf();
            // Codex's reader writes the handshake and its replies.
            let input = input.clone();
            move || {
                if let Ok(stdout) = stdout_rx.recv() {
                    read_messages(stdout, session, &chat, &agents, &events, &input);
                }
                comments.reopen(&worktree, &chat.close());
                // Last, so the exit comes after the comments are open again.
                drop(read_tx);
            }
        })?;
        // Started after the reader, which ends it.
        self.runtime
            .spawn(flush_batches(chat, self.sessions.flush_window()));
        let stdout =
            self.start_piped(session, worktree, cmd, env, (input, input_rx), move || {
                let _ = read_rx.recv_timeout(DRAIN);
            })?;
        // Only fails if the reader is gone, and it waits for this.
        let _ = stdout_tx.send(stdout);
        Ok(())
    }

    /// Sends the session's messages to `sink` from now on, in place of any earlier sink, and
    /// returns every message before the first of them. After the session's process exits it
    /// returns the whole conversation.
    pub fn subscribe(&self, session: SessionId, sink: MessageSink) -> Result<Vec<AgentMessage>> {
        Ok(self.agents.chat(session)?.subscribe(sink))
    }

    /// Sends the user's message to the agent, which starts a turn.
    pub fn send_message(&self, session: SessionId, text: &str) -> Result<()> {
        let chat = self.agents.chat(session)?;
        let input = self.input(session)?;
        chat.send(session, &input, text)?;
        self.agents.title_from(session, text, &self.events);
        Ok(())
    }

    /// Answers a pending approval request or question. The session goes back to `Working`, or
    /// stays `NeedsInput` while another request of the turn waits. A request that is not
    /// pending, or a reply that does not fit it, changes nothing.
    pub fn answer(&self, session: SessionId, request: RequestId, reply: Reply) -> Result<()> {
        let chat = self.agents.chat(session)?;
        // Taken before the chat's lock, so the two never nest.
        let input = self.input(session)?;
        let mut s = chat.state();
        let index = s
            .pending
            .iter()
            .position(|p| p.request == request)
            .ok_or_else(|| CoreError::NoPendingRequest {
                session,
                request: request.clone(),
            })?;
        check_reply(s.pending[index].questions, &reply).map_err(|reason| {
            CoreError::WrongReply {
                request: request.clone(),
                reason,
            }
        })?;
        let line = match &s.pending[index].reply_to {
            None => answer_line(&request, &reply).into_bytes(),
            Some(reply_to) => reply_to.line(&reply),
        };
        if input.send(line).is_err() {
            return Err(CoreError::SessionNotRunning(session));
        }
        s.pending.remove(index);
        chat.push_locked(&mut s, AgentMessage::Answered { request, reply });
        // Under the chat's lock, so a turn end the answer lets through comes after it.
        self.agents.on_protocol(session, s.waiting(), &self.events);
        Ok(())
    }

    /// The models the agent offers. The future waits until the agent has listed them, and fails
    /// if its process ends first. It borrows nothing, so a caller can let go of the core while
    /// it waits.
    pub fn models(
        &self,
        session: SessionId,
    ) -> Result<impl Future<Output = Result<Vec<ModelInfo>>> + Send + use<>> {
        let mut listing = self.agents.chat(session)?.models.subscribe();
        Ok(async move {
            let listed = listing
                .wait_for(|listing| !matches!(listing, Listing::Waiting))
                .await
                .map(|listing| listing.clone());
            match listed {
                Ok(Listing::Listed(models)) => Ok(models),
                // Closed, or the chat and its sender are gone.
                _ => Err(CoreError::SessionNotRunning(session)),
            }
        })
    }

    /// Sets the model, and the effort when given, from the next turn on, and saves them with the
    /// session. The running turn, if any, keeps its own.
    pub fn set_model(&self, session: SessionId, model: &str, effort: Option<&str>) -> Result<()> {
        let chat = self.agents.chat(session)?;
        let input = self.input(session)?;
        let choice = ModelChoice {
            model: model.to_owned(),
            effort: effort.map(str::to_owned),
        };
        chat.state()
            .set_model(&input, &choice)
            .map_err(|_| CoreError::SessionNotRunning(session))?;
        self.agents.choose_model(session, choice);
        Ok(())
    }

    /// The model and effort the session runs with, as `set_model` last saved them. None for a
    /// session that has no choice yet, which runs on its agent's default.
    pub fn model_choice(&self, session: SessionId) -> Result<Option<ModelChoice>> {
        self.agents.model_choice(session)
    }

    /// Asks the agent to stop the turn. The session becomes `Idle` when the agent reports the
    /// turn ended. With Codex, nothing happens while no turn runs.
    pub fn interrupt(&self, session: SessionId) -> Result<()> {
        let chat = self.agents.chat(session)?;
        let input = self.input(session)?;
        let line = match &mut chat.state().app_server {
            None => Some(format!("{}\n", json!({"type": "interrupt"})).into_bytes()),
            Some(server) => server.interrupt(),
        };
        match line {
            Some(line) => input
                .send(line)
                .map_err(|_| CoreError::SessionNotRunning(session)),
            None => Ok(()),
        }
    }
}

/// The line that sends the user's message to the sidecar.
fn user_message(text: &str) -> String {
    format!("{}\n", json!({"type": "user_message", "text": text}))
}

/// The line that sets the sidecar's model and effort. A missing effort stays as it is.
fn set_model_line(model: &str, effort: Option<&str>) -> String {
    let line = match effort {
        Some(effort) => json!({"type": "set_model", "model": model, "effort": effort}),
        None => json!({"type": "set_model", "model": model}),
    };
    format!("{line}\n")
}

/// Why `reply` does not fit a request asking `questions` (None for an approval), if it does not.
fn check_reply(questions: Option<usize>, reply: &Reply) -> std::result::Result<(), String> {
    match (questions, reply) {
        (_, Reply::Deny { .. }) | (None, Reply::AllowOnce | Reply::AllowForSession) => Ok(()),
        (Some(asked), Reply::Answers(answers)) if answers.len() == asked => Ok(()),
        (Some(asked), Reply::Answers(answers)) => Err(format!(
            "it asks {asked} questions, and the reply has {} answers",
            answers.len()
        )),
        (None, Reply::Answers(_)) => Err("an approval takes allow or deny".into()),
        (Some(_), _) => Err("a question takes answers or deny".into()),
    }
}

/// The line that sends the user's answer to the sidecar.
fn answer_line(request: &RequestId, reply: &Reply) -> String {
    let id = &request.0;
    let line = match reply {
        Reply::AllowOnce => json!({"type": "answer", "id": id, "reply": "allow_once"}),
        Reply::AllowForSession => json!({"type": "answer", "id": id, "reply": "allow_session"}),
        Reply::Deny { message: None } => json!({"type": "answer", "id": id, "reply": "deny"}),
        Reply::Deny {
            message: Some(message),
        } => json!({"type": "answer", "id": id, "reply": "deny", "message": message}),
        Reply::Answers(answers) => {
            json!({"type": "answer", "id": id, "reply": "answers", "answers": answers})
        }
    };
    format!("{line}\n")
}

/// Reads the agent's lines until its stdout closes, and writes Codex's replies to `input`. A
/// line that is not a protocol message is skipped.
fn read_messages(
    stdout: ChildStdout,
    session: SessionId,
    chat: &Chat,
    agents: &Agents,
    events: &broadcast::Sender<CoreEvent>,
    input: &mpsc::Sender<Vec<u8>>,
) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        // States are set under the chat's lock, so they come in the order of the messages,
        // also against `answer`.
        let mut s = chat.state();
        let incoming: Vec<FromAgent> = match &mut s.app_server {
            None => serde_json::from_slice(&line).ok().into_iter().collect(),
            Some(server) => {
                let (incoming, writes) = server.read(&line);
                for write in writes {
                    // The writer stops only once the process has exited.
                    let _ = input.send(write);
                }
                incoming
            }
        };
        for message in incoming {
            settle(&mut s, &message);
            apply(session, chat, &mut s, message, agents, events);
        }
    }
}

/// Drops the messages the agent has now started on from the ones still waiting. The sidecar
/// starts a turn on the oldest. By the end of a turn it has answered each message sent before,
/// or it starts the next turn on it at once (endTurn in sidecar/claude/src/sidecar.ts). Codex's
/// app server knows how many of its messages still wait.
fn settle(s: &mut ChatState, message: &FromAgent) {
    let waiting = match (&s.app_server, message) {
        (Some(server), _) => server.unstarted(),
        (None, FromAgent::TurnStart { .. }) => s.unstarted.len().saturating_sub(1),
        (None, FromAgent::TurnEnd { .. }) => 0,
        (None, _) => return,
    };
    let started = s.unstarted.len().saturating_sub(waiting);
    s.unstarted.drain(..started);
}

/// Adds what the agent said to the conversation and sets the state it leaves the session in.
fn apply(
    session: SessionId,
    chat: &Chat,
    s: &mut ChatState,
    message: FromAgent,
    agents: &Agents,
    events: &broadcast::Sender<CoreEvent>,
) {
    let (message, state) = match message {
        FromAgent::Account { account } => {
            // Having no receivers is fine.
            let _ = events.send(CoreEvent::Account {
                agent: chat.agent,
                account,
            });
            return;
        }
        FromAgent::Thread { id } => {
            agents.on_thread(session, id);
            return;
        }
        FromAgent::Models { models } => {
            chat.models.send_replace(Listing::Listed(models));
            return;
        }
        // The record sets no state: the session stays as it was when it switched.
        FromAgent::History { messages } => {
            for message in messages {
                if let Some(message) = recorded(message) {
                    // A session with no title yet takes it from its record, as one saved
                    // before grove kept titles.
                    if let AgentMessage::User { text } = &message {
                        agents.title_from(session, text, events);
                    }
                    chat.push_locked(s, message);
                }
            }
            chat.history_read.send_replace(true);
            return;
        }
        FromAgent::User { text } => (AgentMessage::User { text }, None),
        FromAgent::TurnStart { .. } => (
            AgentMessage::TurnStarted {
                time_unix_ms: now_unix_ms(),
            },
            Some(AgentState::Working),
        ),
        FromAgent::TextDelta { text } => (AgentMessage::TextDelta { text }, None),
        FromAgent::ToolCall {
            id, tool, input, ..
        } => (
            AgentMessage::ToolCall {
                id,
                tool,
                input,
                time_unix_ms: now_unix_ms(),
            },
            None,
        ),
        FromAgent::ToolResult {
            id,
            output,
            is_error,
        } => (
            AgentMessage::ToolResult {
                id,
                output,
                is_error,
            },
            None,
        ),
        FromAgent::ApprovalRequest {
            id,
            tool,
            input,
            reply_to,
        } => {
            if s.is_pending(&id) {
                return;
            }
            s.pending.push(Pending {
                request: id.clone(),
                questions: None,
                reply_to,
            });
            let message = AgentMessage::ApprovalRequested {
                request: id,
                tool,
                input,
            };
            (message, Some(s.waiting()))
        }
        FromAgent::QuestionRequest {
            id,
            questions,
            reply_to,
        } => {
            if s.is_pending(&id) {
                return;
            }
            s.pending.push(Pending {
                request: id.clone(),
                questions: Some(questions.len()),
                reply_to,
            });
            let message = AgentMessage::QuestionAsked {
                request: id,
                questions,
            };
            (message, Some(s.waiting()))
        }
        FromAgent::TurnEnd {
            interrupted, error, ..
        } => {
            s.pending.clear();
            let (outcome, state) = turn_end(interrupted, error);
            let message = AgentMessage::TurnEnded {
                outcome,
                time_unix_ms: now_unix_ms(),
            };
            (message, Some(state))
        }
    };
    chat.push_locked(s, message);
    if let Some(state) = state {
        agents.on_protocol(session, state, events);
    }
}

/// The time now, in milliseconds since the Unix epoch. None only on a clock set before it.
fn now_unix_ms() -> Option<u64> {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since_epoch.as_millis()).ok()
}

/// How a turn ended, and the state that leaves the session in.
fn turn_end(interrupted: bool, error: Option<TurnError>) -> (TurnOutcome, AgentState) {
    match (interrupted, error) {
        (true, _) => (TurnOutcome::Interrupted, AgentState::Idle),
        (false, Some(TurnError { kind, message })) => {
            (TurnOutcome::Failed { kind, message }, AgentState::Failed)
        }
        (false, None) => (TurnOutcome::Completed, AgentState::Done),
    }
}

/// A message of a `history` as the conversation shows it. None for one a record never holds.
fn recorded(message: FromAgent) -> Option<AgentMessage> {
    Some(match message {
        FromAgent::User { text } => AgentMessage::User { text },
        FromAgent::TurnStart { time_unix_ms } => AgentMessage::TurnStarted { time_unix_ms },
        FromAgent::TextDelta { text } => AgentMessage::TextDelta { text },
        FromAgent::ToolCall {
            id,
            tool,
            input,
            time_unix_ms,
        } => AgentMessage::ToolCall {
            id,
            tool,
            input,
            time_unix_ms,
        },
        FromAgent::ToolResult {
            id,
            output,
            is_error,
        } => AgentMessage::ToolResult {
            id,
            output,
            is_error,
        },
        FromAgent::TurnEnd {
            interrupted,
            error,
            time_unix_ms,
        } => AgentMessage::TurnEnded {
            outcome: turn_end(interrupted, error).0,
            time_unix_ms,
        },
        _ => return None,
    })
}
