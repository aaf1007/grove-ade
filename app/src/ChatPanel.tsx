import { ArrowUp, Columns2, Shield, Square, X } from "lucide-react";
import {
  type Dispatch,
  type KeyboardEvent,
  type SetStateAction,
  useCallback,
  useEffect,
  useEffectEvent,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import type {
  Agent,
  AgentMessage,
  AgentStateChanged,
  BatchComment,
  Bridge,
  Mode,
  Question,
  Reply,
  SessionId,
} from "./bridge";
import { AgentMarkdown } from "./AgentMarkdown";
import { type ModelChoice, Picker, useModelChoice } from "./Picker";
import { AGENT_NAMES } from "./Sidebar";
import { TerminalPanel } from "./TerminalPanel";
import { relative, type ToolCall, ToolCalls } from "./ToolCalls";
import { AgentState } from "./ui/AgentState";
import { Button } from "./ui/Button";
import { IconButton } from "./ui/IconButton";
import { Segmented } from "./ui/Segmented";

// The tools whose calls change files, and the input field naming the file.
const EDIT_TOOLS: Record<string, string> = {
  Edit: "file_path",
  Write: "file_path",
  MultiEdit: "file_path",
  NotebookEdit: "notebook_path",
};

// How close to the bottom, in pixels, the message list counts as following the conversation.
const FOLLOW_SLACK = 40;

const MODES: [Mode, string][] = [
  ["Chat", "Chat"],
  ["Terminal", "Terminal"],
];

/** The agent session's mode as the panel knows it. A switch ends one process and starts another. */
interface ModeSwitch {
  /** null until the core says which mode the session runs in. */
  mode: Mode | null;
  /** Switches so far. Each started a new process, so the view for it starts over. */
  switches: number;
  /** Where a switch on its way goes. */
  pending: Mode | null;
  /** Why the last switch failed. */
  error: string | null;
}

/**
 * An agent panel in the chat area: its header with the Chat/Terminal switch, then the session's
 * conversation in chat mode or the agent's own TUI in terminal mode. A switch resumes the same
 * conversation under the same session id.
 */
export function ChatPanel({
  bridge,
  session,
  agent,
  sessionName,
  worktree,
  shown,
  order,
  state,
  focusRequest,
  onFocus,
  onBlur,
  onOpenDiff,
  onChangeAgent,
  onSplit,
  onHide,
  onMode,
  onModelChoice,
}: {
  bridge: Bridge;
  session: SessionId;
  agent: Agent;
  /** What the session is called, as its row in the sidebar says. The agent's name without it. */
  sessionName?: string;
  worktree: string;
  shown: boolean;
  /** Where the panel sits among the agent panels shown beside it. */
  order?: number;
  /** The session's latest state. None until its first. */
  state: AgentStateChanged | undefined;
  /** Each new number puts focus in the composer once the panel is shown. 0 asks for nothing. */
  focusRequest: number;
  /** Called each time the panel gets focus. */
  onFocus?: () => void;
  /** Called each time the panel loses focus, including when the window does. */
  onBlur?: () => void;
  onOpenDiff(): void;
  /** Called when the user picks another agent before the first message. Settles once it has. */
  onChangeAgent?: (agent: Agent) => Promise<void>;
  /** Starts another agent session in a panel beside this one. No button without it. */
  onSplit?: () => void;
  /** Hides the panel. Its session keeps running. No button without it. */
  onHide?: () => void;
  /** Called with the session's mode once known, and after each switch. */
  onMode?: (mode: Mode) => void;
  /** Called with the session's model and effort in chat mode, once known and on each change. */
  onModelChoice?: (choice: ModelChoice | null) => void;
}) {
  const [modeSwitch, setModeSwitch] = useState<ModeSwitch>({
    mode: null,
    switches: 0,
    pending: null,
    error: null,
  });
  // Kept here, so a switch to terminal mode and back keeps what the user was typing.
  const [draft, setDraft] = useState("");
  const name = AGENT_NAMES[agent];
  const blocked = switchBlocked(state, modeSwitch, name);
  const reportMode = useEffectEvent((mode: Mode) => onMode?.(mode));

  useEffect(() => {
    if (modeSwitch.mode !== null) reportMode(modeSwitch.mode);
  }, [modeSwitch.mode, session]);

  // The session can run in terminal mode, as after a restart or a window reload, so the panel asks
  // the core before it shows either view. A session the core no longer lists has ended, and shows
  // in chat mode, where the switch can start it again.
  useEffect(() => {
    if (modeSwitch.mode !== null) return;
    let current = true;
    bridge.sessions(worktree).then(
      (sessions) => {
        const own = sessions.find((info) => info.session === session);
        const mode = own?.kind === "Agent" ? own.mode : "Chat";
        if (current) setModeSwitch((known) => ({ ...known, mode }));
      },
      (e: unknown) => current && setModeSwitch((known) => ({ ...known, error: String(e) })),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, session, modeSwitch.mode]);

  // A switch ends the session's process but not the session, so the agent state alone says whether
  // it runs. A switch also starts an ended session again.
  function switchTo(mode: Mode) {
    if (blocked || mode === modeSwitch.mode) return;
    setModeSwitch((current) => ({ ...current, pending: mode, error: null }));
    bridge.setMode(session, mode).then(
      () => setModeSwitch(({ switches }) => ({ mode, switches: switches + 1, pending: null, error: null })),
      (e: unknown) => setModeSwitch((current) => ({ ...current, pending: null, error: String(e) })),
    );
  }

  return (
    <section
      className="chat-panel"
      aria-label={`Panel ${session}`}
      hidden={!shown}
      style={{ order }}
      // A click anywhere in the panel focuses it.
      tabIndex={-1}
      onFocus={onFocus}
      onBlur={onBlur}
    >
      <div className="pane-head">
        <b>{sessionName ?? name}</b>
        {state && <AgentState state={state} source={state.source} />}
        <span className="spacer" />
        <Segmented
          label="Mode"
          options={MODES}
          value={modeSwitch.mode}
          disabled={blocked !== null}
          title={blocked ?? undefined}
          onChange={switchTo}
        />
        {onSplit && (
          <IconButton
            icon={Columns2}
            label="Split"
            title="Start another agent session beside this one"
            onClick={onSplit}
          />
        )}
        {onHide && (
          <IconButton
            icon={X}
            label="Hide panel"
            title="Hide this panel. The agent session keeps running."
            onClick={onHide}
          />
        )}
      </div>
      {modeSwitch.error && (
        <p role="alert" className="chat-error">
          {modeSwitch.error}
        </p>
      )}
      {modeSwitch.mode === "Chat" && (
        <ChatView
          key={modeSwitch.switches}
          bridge={bridge}
          session={session}
          agent={agent}
          worktree={worktree}
          shown={shown}
          state={state}
          focusRequest={focusRequest}
          draft={draft}
          onDraft={setDraft}
          resumed={modeSwitch.switches > 0}
          switchingMode={modeSwitch.pending !== null}
          onOpenDiff={onOpenDiff}
          onChangeAgent={onChangeAgent}
          onModelChoice={onModelChoice}
        />
      )}
      {modeSwitch.mode === "Terminal" && (
        <>
          <p className="tui-note">
            The same conversation, resumed in {name}&apos;s own TUI. State now comes from hooks.
          </p>
          <TerminalPanel
            key={modeSwitch.switches}
            bridge={bridge}
            session={session}
            shown={shown}
            focusRequest={focusRequest}
          />
        </>
      )}
    </section>
  );
}

/** Why the Chat/Terminal switch is off, as its tooltip says. null while it is on. */
function switchBlocked(
  state: AgentStateChanged | undefined,
  { mode, pending }: ModeSwitch,
  name: string,
): string | null {
  if (mode === null) return "Finding out which mode the session runs in";
  if (pending) return `Switching to ${pending.toLowerCase()} mode`;
  if (state?.state === "Working") return `${name} is working. Switch modes once the turn ends.`;
  if (state?.state === "NeedsInput") return `${name} is waiting on you. Answer it, then switch modes.`;
  return null;
}

/** Where the panel came back to chat mode from terminal mode. Only the panel adds it. */
const RESUMED = { type: "Resumed" } as const;

/** What the message list holds: the agent session's messages, and where the panel resumed them. */
type ChatMessage = AgentMessage | typeof RESUMED;

/**
 * The conversation of a chat-mode agent session and the composer with the agent and model picker.
 * It follows the conversation while `shown`, and gets the whole of it again each time it is shown.
 */
function ChatView({
  bridge,
  session,
  agent,
  worktree,
  shown,
  state,
  focusRequest,
  draft,
  onDraft: setDraft,
  resumed,
  switchingMode,
  onOpenDiff,
  onChangeAgent,
  onModelChoice,
}: {
  bridge: Bridge;
  session: SessionId;
  agent: Agent;
  worktree: string;
  shown: boolean;
  state: AgentStateChanged | undefined;
  focusRequest: number;
  draft: string;
  onDraft: Dispatch<SetStateAction<string>>;
  /** The conversation came back from terminal mode, so the list says where. */
  resumed: boolean;
  /** A mode switch is on its way, so nothing may be sent. */
  switchingMode: boolean;
  onOpenDiff(): void;
  onChangeAgent?: (agent: Agent) => Promise<void>;
  onModelChoice?: (choice: ModelChoice | null) => void;
}) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  // A switch ends this session once the new one starts, so nothing may be sent to it meanwhile.
  const [changingAgent, setChangingAgent] = useState(false);
  // What the user sent that the core has not echoed back yet, oldest first.
  const [unechoed, setUnechoed] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  // How many messages the conversation had when it came back from terminal mode.
  const resumedAt = useRef<number | null>(null);
  const list = useRef<HTMLDivElement>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  // Sends on their way, oldest first. The failed ones go back into the draft together once all
  // have settled, so they come back in the order they were sent.
  const sending = useRef<{ text: string; settled: boolean; failed: boolean }[]>([]);
  const following = useRef(true);
  const { models, choice, choose, error: modelError } = useModelChoice(bridge, session);
  const reportChoice = useEffectEvent((known: ModelChoice | null) => onModelChoice?.(known));

  useEffect(() => {
    if (models !== null) reportChoice(choice);
  }, [models, choice]);
  const name = AGENT_NAMES[agent];
  const focusedRequest = useRef(0);

  useEffect(() => {
    if (!shown) return;
    let current = true;
    // Frames that come before the history go after it.
    let early: AgentMessage[] | null = [];
    const queued: AgentMessage[] = [];
    let frame = 0;
    function draw() {
      frame = 0;
      const batch = queued.splice(0);
      const echoed = batch.filter((m) => m.type === "User").length;
      setMessages((shown) => [...shown, ...batch]);
      setUnechoed((sent) => sent.slice(echoed));
    }
    bridge
      .subscribe(session, (batch) => {
        if (!current) return;
        if (early) {
          early.push(...batch);
          return;
        }
        // However many frames come, the panel draws once per animation frame.
        queued.push(...batch);
        if (!frame) frame = requestAnimationFrame(draw);
      })
      .then(
        (history) => {
          if (!current) return;
          // Every message sent before the subscribe is in the history.
          if (resumed) resumedAt.current ??= history.length;
          const all: ChatMessage[] = [...history, ...early!];
          if (resumedAt.current !== null) all.splice(resumedAt.current, 0, RESUMED);
          setMessages(all);
          setUnechoed([]);
          early = null;
        },
        (e: unknown) => current && setError(String(e)),
      );
    return () => {
      current = false;
      if (frame) cancelAnimationFrame(frame);
    };
  }, [bridge, session, shown, resumed]);

  const items = useMemo(() => thread(messages, worktree), [messages, worktree]);

  useLayoutEffect(() => {
    const el = list.current;
    if (el && following.current) el.scrollTop = el.scrollHeight;
  }, [items, unechoed]);

  useEffect(() => {
    if (!shown || focusRequest === 0 || focusRequest === focusedRequest.current) return;
    focusedRequest.current = focusRequest;
    composer.current!.focus();
  }, [focusRequest, shown]);

  // The composer grows with the draft up to its CSS max-height, then scrolls. A hidden panel
  // has no size, so it measures again once shown, and again when its width changes the wrapping.
  useLayoutEffect(() => {
    if (shown) fitComposer(composer.current!);
  }, [draft, shown]);

  useEffect(() => {
    const el = composer.current!;
    let width = 0;
    const sizes = new ResizeObserver(([entry]) => {
      if (entry.contentRect.width === width) return;
      width = entry.contentRect.width;
      fitComposer(el);
    });
    sizes.observe(el);
    return () => sizes.disconnect();
  }, []);

  const working = state?.state === "Working";
  // A turn runs from its start until it ends, through any requests it waits on.
  const turnRunning = working || state?.state === "NeedsInput";
  // The first message waits for the model it runs with.
  const canSend = draft.trim() !== "" && !turnRunning && models !== null && !changingAgent && !switchingMode;
  const agentLocked =
    unechoed.length > 0 || messages.some((m) => m.type === "User" || m.type === "ReviewBatch");

  function changeAgent(picked: Agent) {
    if (!onChangeAgent) return;
    setChangingAgent(true);
    void onChangeAgent(picked).finally(() => setChangingAgent(false));
  }

  function send() {
    const text = draft;
    if (!canSend) return;
    setDraft("");
    setUnechoed((sent) => [...sent, text]);
    following.current = true;
    const sent = { text, settled: false, failed: false };
    sending.current.push(sent);
    bridge.sendMessage(session, text).then(
      () => {
        sent.settled = true;
        setError(null);
        restoreFailed();
      },
      (e: unknown) => {
        Object.assign(sent, { settled: true, failed: true });
        setError(String(e));
        restoreFailed();
      },
    );
  }

  // Anything typed since stays, after the messages that failed.
  function restoreFailed() {
    if (!sending.current.every((s) => s.settled)) return;
    const failed = sending.current.filter((s) => s.failed).map((s) => s.text);
    sending.current = [];
    if (failed.length === 0) return;
    setUnechoed((shown) => shown.filter((text) => !failed.includes(text)));
    setDraft((typed) => [...failed, typed].filter(Boolean).join("\n"));
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
    event.preventDefault();
    send();
  }

  // Fails only once the turn or the session has ended, so there is nothing to report.
  function interrupt() {
    bridge.interrupt(session).catch(() => {});
  }

  function answer(request: string, reply: Reply) {
    return bridge.answer(session, request, reply);
  }

  // Stable, so a finished reply is not parsed again while the next one streams.
  const openLink = useCallback(
    (link: string) =>
      void bridge.openLinkInDefaultBrowser(link).then(
        () => setError(null),
        (e: unknown) => setError(String(e)),
      ),
    [bridge],
  );

  return (
    <>
      <div
        className="messages"
        ref={list}
        onScroll={(event) => {
          const el = event.currentTarget;
          following.current = el.scrollHeight - el.scrollTop - el.clientHeight < FOLLOW_SLACK;
        }}
      >
        <div className="thread">
          {items.map((item, i) => (
            <Item
              key={i}
              item={item}
              agentName={name}
              worktree={worktree}
              onOpenDiff={onOpenDiff}
              onAnswer={answer}
              onOpenLink={openLink}
            />
          ))}
          {unechoed.map((text, i) => (
            <div key={`unechoed ${i}`} className="msg-user">
              {text}
            </div>
          ))}
          {/* Mounted for the whole turn, so the count goes on after a request it waited on. */}
          {turnRunning && <WorkingFor shown={working} />}
        </div>
      </div>
      <div className="composer-wrap">
        {(error ?? modelError) && (
          <p role="alert" className="composer-error">
            {error ?? modelError}
          </p>
        )}
        <div className="composer">
          <textarea
            ref={composer}
            aria-label="Message"
            rows={1}
            placeholder={`Ask ${name} anything`}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={onKeyDown}
          />
          <div className="composer-row">
            <Picker
              agent={agent}
              agentLocked={agentLocked}
              models={models}
              choice={choice}
              onAgent={changeAgent}
              onChoose={choose}
            />
            <span className="spacer" />
            {working ? (
              <button className="send stop" aria-label="Interrupt" title="Interrupt the turn" onClick={interrupt}>
                <Square aria-hidden="true" />
              </button>
            ) : (
              <button className="send" aria-label="Send" title="Send" disabled={!canSend} onClick={send}>
                <ArrowUp aria-hidden="true" />
              </button>
            )}
          </div>
        </div>
      </div>
    </>
  );
}

function fitComposer(el: HTMLTextAreaElement) {
  el.style.height = "auto";
  el.style.height = `${el.scrollHeight}px`;
}

/** One thing the message list shows. */
type ThreadItem =
  | { kind: "user"; text: string }
  | { kind: "agent"; text: string }
  | { kind: "tools"; calls: ToolCall[] }
  | { kind: "files"; paths: string[] }
  | { kind: "review"; comments: BatchComment[] }
  | { kind: "notice"; text: string }
  | { kind: "resumed" }
  | { kind: "error"; text: string }
  | { kind: "undelivered"; text: string }
  | RequestItem;

/** An approval or a question. Pending until it has a reply or its turn ends. */
type RequestItem = {
  request: string;
  reply: Reply | null;
  /** Its turn ended with no reply. */
  expired: boolean;
} & ({ kind: "approval"; tool: string; input: unknown } | { kind: "question"; questions: Question[] });

/**
 * What the message list shows for `messages`: the agent's deltas joined, each run of tool calls
 * grouped with their results, and each turn's edited files in a card at its end.
 */
function thread(messages: ChatMessage[], worktree: string): ThreadItem[] {
  const items: ThreadItem[] = [];
  const calls = new Map<string, ToolCall>();
  // The current turn's requests, by id.
  const requests = new Map<string, RequestItem>();
  let edited: string[] = [];
  for (const message of messages) {
    const last = items[items.length - 1];
    switch (message.type) {
      case "User":
        items.push({ kind: "user", text: message.text });
        break;
      case "TurnStarted":
        edited = [];
        break;
      case "TextDelta":
        if (last?.kind === "agent") last.text += message.text;
        else items.push({ kind: "agent", text: message.text });
        break;
      case "ToolCall": {
        const call = { id: message.id, tool: message.tool, input: message.input };
        calls.set(call.id, call);
        if (last?.kind === "tools") last.calls.push(call);
        else items.push({ kind: "tools", calls: [call] });
        const path = editedPath(message.tool, message.input);
        if (path && !edited.includes(path)) edited.push(path);
        break;
      }
      case "ToolResult": {
        const call = calls.get(message.id);
        if (call) call.result = { output: message.output, isError: message.isError };
        break;
      }
      case "ApprovalRequested": {
        const { request, tool, input } = message;
        const item: RequestItem = { kind: "approval", request, tool, input, reply: null, expired: false };
        requests.set(request, item);
        items.push(item);
        break;
      }
      case "QuestionAsked": {
        const { request, questions } = message;
        const item: RequestItem = { kind: "question", request, questions, reply: null, expired: false };
        requests.set(request, item);
        items.push(item);
        break;
      }
      case "Answered": {
        const item = requests.get(message.request);
        if (item) item.reply = message.reply;
        break;
      }
      case "ReviewBatch":
        items.push({ kind: "review", comments: message.comments });
        break;
      case "Resumed":
        items.push({ kind: "resumed" });
        break;
      case "TurnEnded":
        requests.forEach((item) => (item.expired = !item.reply));
        requests.clear();
        if (edited.length > 0) {
          items.push({ kind: "files", paths: edited.map((path) => relative(path, worktree)) });
          edited = [];
        }
        if (message.outcome === "Interrupted") {
          items.push({ kind: "notice", text: "You interrupted the turn." });
        } else if (message.outcome === "Failed") {
          items.push({ kind: "error", text: message.message });
        }
        break;
      case "NotDelivered":
        items.push({ kind: "undelivered", text: message.text });
        break;
    }
  }
  return items;
}

// The file an editing tool call changes, as the agent named it.
function editedPath(tool: string, input: unknown): string | null {
  const field = EDIT_TOOLS[tool];
  if (!field || typeof input !== "object" || input === null) return null;
  const path = (input as Record<string, unknown>)[field];
  return typeof path === "string" ? path : null;
}

function Item({
  item,
  agentName,
  worktree,
  onOpenDiff,
  onAnswer,
  onOpenLink,
}: {
  item: ThreadItem;
  agentName: string;
  worktree: string;
  onOpenDiff(): void;
  onAnswer(request: string, reply: Reply): Promise<void>;
  onOpenLink(link: string): void;
}) {
  switch (item.kind) {
    case "approval":
    case "question":
      return <RequestCard item={item} agentName={agentName} onAnswer={onAnswer} />;
    case "user":
      return <div className="msg-user">{item.text}</div>;
    case "agent":
      return <AgentMarkdown text={item.text} onOpenLink={onOpenLink} />;
    case "tools":
      return <ToolCalls calls={item.calls} worktree={worktree} />;
    case "files": {
      const label = `${item.paths.length} ${item.paths.length === 1 ? "file" : "files"} changed`;
      return (
        <div className="card" role="group" aria-label={label}>
          <div className="card-head">
            <b>{label}</b>
            <span className="spacer" />
            <Button size="xs" onClick={onOpenDiff}>
              Review diff
            </Button>
          </div>
          <ul className="file-rows">
            {item.paths.map((path) => (
              <li key={path} className="file-row">
                {path}
              </li>
            ))}
          </ul>
        </div>
      );
    }
    case "review": {
      const count = item.comments.length;
      return (
        <div className="card" role="group" aria-label="Review batch">
          <div className="card-head">
            <b>Review batch</b>
            <span className="spacer" />
            <span className="state Working">{count === 1 ? "1 comment sent" : `${count} comments sent`}</span>
          </div>
          <ul className="batch-items">
            {item.comments.map((comment, i) => (
              <li key={i} className="batch-item">
                <code>{placeOf(comment)}</code>
                <span>{comment.body}</span>
              </li>
            ))}
          </ul>
        </div>
      );
    }
    case "resumed":
      return (
        <div className="notice-line">
          Back in chat mode. The same conversation resumed from {agentName}&apos;s TUI, and state comes from
          the protocol again.
        </div>
      );
    case "notice":
      return <div className="notice-line">{item.text}</div>;
    case "error":
      return (
        <div className="card err">
          <div className="card-head">
            <b>Turn failed</b>
          </div>
          <div className="card-body">{item.text}</div>
        </div>
      );
    case "undelivered":
      return (
        <div className="card err" role="group" aria-label="Not delivered">
          <div className="card-head">
            <b>Not delivered</b>
          </div>
          <div className="card-body">
            <p>{agentName} ended before it started on this message.</p>
            <p>{item.text}</p>
          </div>
        </div>
      );
  }
}

/** Where a review comment is, as the agent reads it: `path:line` or `path:first-last`. */
function placeOf({ path, startLine, endLine }: BatchComment): string {
  return startLine === endLine ? `${path}:${startLine}` : `${path}:${startLine}-${endLine}`;
}

/**
 * A pending request as a card that answers it, and as a one-line record once answered or expired.
 * A card scrolls into view when it comes, and answers once.
 */
function RequestCard({
  item,
  agentName,
  onAnswer,
}: {
  item: RequestItem;
  agentName: string;
  onAnswer(request: string, reply: Reply): Promise<void>;
}) {
  const [sending, setSending] = useState(false);
  // The core refused the answer: the request ended before the panel heard.
  const [refused, setRefused] = useState(false);
  const card = useRef<HTMLDivElement>(null);
  const expired = item.expired || refused;
  const pending = !item.reply && !expired;

  useLayoutEffect(() => {
    if (pending) card.current!.scrollIntoView({ block: "nearest" });
  }, [pending]);

  function answer(reply: Reply) {
    if (sending) return;
    setSending(true);
    // The card becomes a record when the core's `Answered` comes.
    onAnswer(item.request, reply).catch(() => setRefused(true));
  }

  if (item.reply) return <div className="record">{describeReply(item, item.reply)}</div>;
  if (expired) {
    const asked =
      item.kind === "approval" ? `${agentName} asked to use ${item.tool}` : item.questions[0]?.question;
    return <div className="record expired">Expired: {asked}</div>;
  }
  if (item.kind === "approval") {
    const title = `${agentName} wants to use ${item.tool}`;
    return (
      <div className="ask" role="group" aria-label={title} ref={card}>
        <div className="ask-title">{title}</div>
        <pre className="ask-command">{describeInput(item.input)}</pre>
        <div className="ask-actions">
          <span className="ask-waiting">
            <Shield aria-hidden="true" />
            Waiting for approval
          </span>
          <Button variant="primary" size="xs" disabled={sending} onClick={() => answer({ type: "AllowOnce" })}>
            Allow
          </Button>
          <Button size="xs" disabled={sending} onClick={() => answer({ type: "Deny", message: null })}>
            Deny
          </Button>
          <Button
            variant="ghost"
            size="xs"
            disabled={sending}
            onClick={() => answer({ type: "AllowForSession" })}
          >
            Always allow this session
          </Button>
        </div>
      </div>
    );
  }
  const title = `${agentName} is asking`;
  return (
    <div className="card ask" role="group" aria-label={title} ref={card}>
      <div className="card-head">
        <b>{title}</b>
      </div>
      <QuestionForm request={item.request} questions={item.questions} sending={sending} onAnswer={answer} />
    </div>
  );
}

/**
 * The questions of one request, each with its options and a field for the user's own answer,
 * which takes the place of any option picked.
 */
function QuestionForm({
  request,
  questions,
  sending,
  onAnswer,
}: {
  request: string;
  questions: Question[];
  sending: boolean;
  onAnswer(reply: Reply): void;
}) {
  const [picked, setPicked] = useState<string[][]>(() => questions.map(() => []));
  const [typed, setTyped] = useState<string[]>(() => questions.map(() => ""));
  // In the options' order, whatever order they were picked in.
  const answers = questions.map((q, i) =>
    typed[i].trim()
      ? [typed[i].trim()]
      : q.options.map((o) => o.label).filter((label) => picked[i].includes(label)),
  );
  const complete = answers.every((answer) => answer.length > 0);

  function pick(i: number, label: string, multi: boolean) {
    const was = picked[i];
    const now = !multi ? [label] : was.includes(label) ? was.filter((l) => l !== label) : [...was, label];
    setPicked(picked.map((p, j) => (j === i ? now : p)));
    setTyped(typed.map((t, j) => (j === i ? "" : t)));
  }

  function type(i: number, text: string) {
    setTyped(typed.map((t, j) => (j === i ? text : t)));
    if (text) setPicked(picked.map((p, j) => (j === i ? [] : p)));
  }

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (complete) onAnswer({ type: "Answers", answers });
      }}
    >
      {questions.map((q, i) => (
        <fieldset key={i} className="question" disabled={sending}>
          <legend>
            <span className="question-header">{q.header}</span>
            {q.question}
          </legend>
          {q.options.map((option) => (
            <label key={option.label} className="option">
              <input
                type={q.multiSelect ? "checkbox" : "radio"}
                name={`${request} ${i}`}
                checked={picked[i].includes(option.label)}
                onChange={() => pick(i, option.label, q.multiSelect)}
              />
              <span>
                <b>{option.label}</b>
                {option.description && <span className="option-description">{option.description}</span>}
              </span>
            </label>
          ))}
          <input
            type="text"
            className="own-answer"
            aria-label={`Your own answer to ${q.header}`}
            placeholder="Or type your own answer"
            value={typed[i]}
            onChange={(event) => type(i, event.target.value)}
          />
        </fieldset>
      ))}
      <div className="card-actions">
        <Button type="submit" variant="primary" size="sm" disabled={!complete || sending}>
          Answer
        </Button>
      </div>
    </form>
  );
}

/** What an approval asks to run: a command as is, any other input as JSON. */
function describeInput(input: unknown): string {
  const command = (input as { command?: unknown } | null)?.command;
  return typeof command === "string" ? command : JSON.stringify(input, null, 2);
}

/** The one-line record of an answered request, such as "Allowed Bash once". */
function describeReply(item: RequestItem, reply: Reply): string {
  const asked = item.kind === "approval" ? item.tool : "the question";
  switch (reply.type) {
    case "AllowOnce":
      return `Allowed ${asked} once`;
    case "AllowForSession":
      return `Allowed ${asked} for this session`;
    case "Deny":
      return `Denied ${asked}`;
    case "Answers": {
      const headers = item.kind === "question" && item.questions.length > 1 ? item.questions : null;
      const answers = reply.answers.map((a, i) => (headers ? `${headers[i].header}: ` : "") + a.join(", "));
      return `Answered: ${answers.join("; ")}`;
    }
  }
}

/**
 * How long the turn has gone on since the panel saw it start, counted each second. It counts while
 * hidden too, so it is right the moment it shows again.
 */
function WorkingFor({ shown }: { shown: boolean }) {
  const [since] = useState(Date.now);
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const tick = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick);
  }, []);
  return shown && <div className="working-for">Working for {duration(now - since)}</div>;
}

/** A span of time as "45s", "1m 3s" or "1h 2m". */
function duration(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const [hours, minutes] = [Math.floor(seconds / 3600), Math.floor(seconds / 60) % 60];
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds % 60}s`;
  return `${seconds}s`;
}
