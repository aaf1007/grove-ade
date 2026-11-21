import { ArrowUp, Columns2, GitBranch, Shield, SlidersHorizontal, Square, X } from "lucide-react";
import {
  type Dispatch,
  type KeyboardEvent,
  type ReactNode,
  type Ref,
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
  AgentChange,
  AgentMessage,
  AgentStateChanged,
  BatchComment,
  Bridge,
  FileDiff,
  Mode,
  Question,
  Reply,
  SessionId,
  Workspace,
} from "./bridge";
import { AgentMarkdown } from "./AgentMarkdown";
import "./ChatPanel.css";
import { AgentMark, type ModelChoice, Picker, useModelChoice } from "./Picker";
import { AGENT_NAMES, byRepo } from "./Sidebar";
import { TerminalPanel } from "./TerminalPanel";
import { DeclinedCall, relative, type ToolCall, ToolCalls } from "./ToolCalls";
import { AgentState } from "./ui/AgentState";
import { Button } from "./ui/Button";
import { IconButton } from "./ui/IconButton";
import { Segmented } from "./ui/Segmented";

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
 * An agent panel in the chat area: its header unless the title bar holds it, then the session's
 * conversation in chat mode or the agent's own TUI in terminal mode, then a strip with the branch,
 * the profile and the Chat/Terminal switch. A switch resumes the same conversation under the same
 * session id.
 */
export function ChatPanel({
  bridge,
  session,
  agent,
  sessionName,
  showHeader = true,
  worktree,
  shown,
  order,
  state,
  focusRequest,
  resumes = 0,
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
  /** Whether the panel draws its header. With one agent panel shown, the title bar holds it instead. */
  showHeader?: boolean;
  worktree: string;
  shown: boolean;
  /** Where the panel sits among the agent panels shown beside it. */
  order?: number;
  /** The session's latest state. None until its first. */
  state: AgentStateChanged | undefined;
  /**
   * How many times the ended session was resumed. Each resume starts a new process, so the panel
   * subscribes or attaches again.
   */
  resumes?: number;
  /** Each new number puts focus in the composer once the panel is shown. 0 asks for nothing. */
  focusRequest: number;
  /** Called each time the panel gets focus. */
  onFocus?: () => void;
  /** Called each time the panel loses focus, including when the window does. */
  onBlur?: () => void;
  /** Opens the Diff panel, on `path` when given. */
  onOpenDiff(path?: string): void;
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
  const { branch, profile } = useWorktreePlace(bridge, worktree);
  // Here, above the views, so a notice nobody answered stays through a mode switch.
  const change = useAgentChange(bridge, session, worktree);
  const notice = change.notice && (
    <AgentChangeNotice
      agent={agent}
      who={sessionName ?? name}
      notice={change.notice}
      busy={change.busy}
      error={change.error}
      canSave={profile !== null}
      onUndo={change.undo}
      onKeep={change.keep}
      onSave={change.save}
    />
  );
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

  const strip = (
    <div className="composer-strip" role="group" aria-label="Branch and profile">
      {branch && (
        <span>
          <GitBranch aria-hidden="true" />
          {branch}
        </span>
      )}
      {profile && (
        <span>
          <SlidersHorizontal aria-hidden="true" />
          {profile}
        </span>
      )}
      <span className="spacer" />
      <Segmented
        label="Mode"
        options={MODES}
        value={modeSwitch.mode}
        disabled={blocked !== null}
        title={blocked ?? undefined}
        onChange={switchTo}
      />
    </div>
  );

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
      {showHeader && (
        <div className="pane-head">
          <b>{sessionName ?? name}</b>
          {state && <AgentState state={state} source={state.source} />}
          <span className="spacer" />
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
      )}
      {modeSwitch.error && (
        <p role="alert" className="chat-error">
          {modeSwitch.error}
        </p>
      )}
      {modeSwitch.mode === "Chat" && (
        <ChatView
          notice={notice}
          strip={strip}
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
          resumes={resumes}
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
            key={`${modeSwitch.switches} ${resumes}`}
            bridge={bridge}
            session={session}
            shown={shown}
            focusRequest={focusRequest}
          />
          <div className="composer-wrap strip-only">
            {notice}
            {strip}
          </div>
        </>
      )}
      {modeSwitch.mode === null && (
        <div className="composer-wrap strip-only">
          {notice}
          {strip}
        </div>
      )}
    </section>
  );
}

/**
 * The worktree's branch and the name of the profile its repo uses, null when it has none. The
 * profile follows changes to the repo's settings and to profiles. Neither shows when listing fails:
 * the details card reports that.
 */
function useWorktreePlace(bridge: Bridge, worktree: string) {
  const [found, setFound] = useState<{ worktree: string; repo: string; branch: string | null } | null>(null);
  const [profile, setProfile] = useState<string | null>(null);
  const [settingsChanges, setSettingsChanges] = useState(0);
  const mine = found?.worktree === worktree ? found : null;
  const repo = mine?.repo;

  useEffect(() => {
    let current = true;
    bridge.listWorktrees().then(
      (list) => {
        const worktrees = byRepo(list).find((all) => all.some((w) => w.path === worktree));
        const own = worktrees?.find((w) => w.path === worktree);
        if (current && worktrees && own) setFound({ worktree, repo: worktrees[0].path, branch: own.branch });
      },
      () => {},
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree]);

  useEffect(() => {
    if (repo === undefined) return;
    const changed = () => setSettingsChanges((n) => n + 1);
    const stopProfiles = bridge.onProfilesChanged(changed);
    const stopRepo = bridge.onRepoSettingsChanged((changedRepo) => changedRepo === repo && changed());
    return () => {
      stopProfiles();
      stopRepo();
    };
  }, [bridge, repo]);

  useEffect(() => {
    if (repo === undefined) return;
    let current = true;
    bridge.repoProfile(repo).then(
      (name) => current && setProfile(name),
      () => current && setProfile(null),
    );
    return () => {
      current = false;
    };
  }, [bridge, repo, settingsChanges]);

  return { branch: mine?.branch ?? null, profile: repo === undefined ? null : profile };
}

/** What an agent session changed, as its notice says. */
type ChangeNotice = { kind: "Workspace"; change: string } | { kind: "Profiles" };

/**
 * The latest change this panel's agent session made to its worktree's workspace or to a profile,
 * until the user answers it or a newer change replaces it.
 */
function useAgentChange(bridge: Bridge, session: SessionId, worktree: string) {
  const [shown, setShown] = useState<{ notice: ChangeNotice; of: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const of = `${session} ${worktree}`;

  useEffect(() => {
    let current = true;
    // The latest workspace fetched, and the one from before an agent change on its way.
    let known: Workspace | null = null;
    let before: { workspace: Workspace | null } | null = null;
    let fetches = 0;
    function show(notice: ChangeNotice) {
      setShown({ notice, of: `${session} ${worktree}` });
      setError(null);
    }
    function fetch() {
      const at = ++fetches;
      bridge.workspace(worktree).then(
        (workspace) => {
          if (!current || at !== fetches) return;
          if (before) show({ kind: "Workspace", change: describeChange(before.workspace, workspace) });
          before = null;
          known = workspace;
        },
        () => {},
      );
    }
    fetch();
    const stopWorkspace = bridge.onWorkspaceChanged((changed) => changed === worktree && fetch());
    // The core sends a change's `agent-change` before its `workspace-changed`.
    const stopAgent = bridge.onAgentChange((change: AgentChange) => {
      if (change.by !== session) return;
      if (change.kind === "Profiles") show({ kind: "Profiles" });
      else if (change.worktree === worktree) before = { workspace: known };
    });
    return () => {
      current = false;
      stopWorkspace();
      stopAgent();
    };
  }, [bridge, session, worktree]);

  function answer(call: () => Promise<void>) {
    setBusy(true);
    call()
      .then(
        () => setShown(null),
        (e: unknown) => setError(String(e)),
      )
      .finally(() => setBusy(false));
  }

  return {
    notice: shown?.of === of ? shown.notice : null,
    busy,
    error,
    undo: () => answer(() => bridge.undoWorkspace(worktree)),
    keep: () => setShown(null),
    save: () => answer(() => bridge.saveWorkspaceToProfile(worktree)),
  };
}

/**
 * The agent change notice: the agent's mark, what its session changed, and Undo, Keep and Save to
 * profile for a workspace, or Keep for a profile. It never takes focus.
 */
function AgentChangeNotice({
  agent,
  who,
  notice,
  busy,
  error,
  canSave,
  onUndo,
  onKeep,
  onSave,
}: {
  agent: Agent;
  /** The agent session's name. */
  who: string;
  notice: ChangeNotice;
  /** An answer is on its way. */
  busy: boolean;
  error: string | null;
  /** The repo uses a profile to save to. */
  canSave: boolean;
  onUndo(): void;
  onKeep(): void;
  onSave(): void;
}) {
  const workspace = notice.kind === "Workspace";
  return (
    <div className="agent-change" role="group" aria-label="Agent change">
      <AgentMark agent={agent} />
      <span className="agent-change-text">
        {workspace ? (
          <>
            {who} changed this workspace: <span className="agent-change-what">{notice.change}</span>
          </>
        ) : (
          `${who} changed a profile`
        )}
      </span>
      <span className="agent-change-actions">
        {workspace && (
          <button className="agent-change-action" disabled={busy} onClick={onUndo}>
            Undo
          </button>
        )}
        <button className="agent-change-action" disabled={busy} onClick={onKeep}>
          Keep
        </button>
        {workspace && canSave && (
          <button className="agent-change-action" disabled={busy} onClick={onSave}>
            Save to profile
          </button>
        )}
      </span>
      {error && (
        <p role="alert" className="agent-change-error">
          {error}
        </p>
      )}
    </div>
  );
}

/** What changed from `before` to `after`, such as "opened the Logs panel and closed the terminal". */
function describeChange(before: Workspace | null, after: Workspace): string {
  if (!before) return "changed its layout";
  const parts: string[] = [];
  const opened = after.panels.filter((panel) => !before.panels.includes(panel));
  const closed = before.panels.filter((panel) => !after.panels.includes(panel));
  const panels = (kinds: string[]) => `the ${sentence(kinds)} ${kinds.length === 1 ? "panel" : "panels"}`;
  if (opened.length > 0) parts.push(`opened ${panels(opened)}`);
  if (closed.length > 0) parts.push(`closed ${panels(closed)}`);
  if (after.terminal !== before.terminal) parts.push(`${after.terminal ? "opened" : "closed"} the terminal`);
  for (const agent of ["Claude", "Codex"] as const) {
    const count = (workspace: Workspace) => workspace.chat.filter((p) => p.agent === agent && p.shown).length;
    const more = count(after) - count(before);
    if (more !== 0) {
      const which = AGENT_NAMES[agent];
      const many = Math.abs(more) === 1 ? `a ${which} panel` : `${Math.abs(more)} ${which} panels`;
      parts.push(`${more > 0 ? "added" : "hid"} ${many}`);
    }
  }
  if (opened.length === 0 && after.selected !== null && after.selected !== before.selected) {
    parts.push(`switched to the ${after.selected} tab`);
  }
  return parts.length > 0 ? sentence(parts) : "changed its layout";
}

/** "a", "a and b", "a, b and c". */
function sentence(items: string[]): string {
  return items.length < 2 ? items.join("") : `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
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
  notice,
  strip,
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
  resumes,
  switchingMode,
  onOpenDiff,
  onChangeAgent,
  onModelChoice,
}: {
  /** Goes right above the composer. */
  notice: ReactNode;
  /** Goes right under the composer. */
  strip: ReactNode;
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
  /** Each new number means a new process, so the view subscribes again. */
  resumes: number;
  /** A mode switch is on its way, so nothing may be sent. */
  switchingMode: boolean;
  onOpenDiff(path?: string): void;
  onChangeAgent?: (agent: Agent) => Promise<void>;
  onModelChoice?: (choice: ModelChoice | null) => void;
}) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  // Whether the history has come. Before it does, the panel cannot tell whether an approval waits.
  const [historyLoaded, setHistoryLoaded] = useState(false);
  // A switch ends this session once the new one starts, so nothing may be sent to it meanwhile.
  const [changingAgent, setChangingAgent] = useState(false);
  // What the user sent that the core has not echoed back yet, oldest first.
  const [unechoed, setUnechoed] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  // Requests the core refused an answer to: they ended before the panel heard.
  const [refused, setRefused] = useState<string[]>([]);
  // How many messages the conversation had when it came back from terminal mode.
  const resumedAt = useRef<number | null>(null);
  const list = useRef<HTMLDivElement>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  const approvalCard = useRef<HTMLDivElement>(null);
  // Set when the user answers an approval with focus in its card, until focus moves to the next.
  const focusFollowsApproval = useRef(false);
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
          setHistoryLoaded(true);
          setUnechoed([]);
          early = null;
        },
        (e: unknown) => {
          if (!current) return;
          setError(String(e));
          setHistoryLoaded(true);
        },
      );
    return () => {
      current = false;
      if (frame) cancelAnimationFrame(frame);
      // The next subscribe brings the history again, and a jump waits for it.
      setHistoryLoaded(false);
    };
  }, [bridge, session, shown, resumed, resumes]);

  const items = useMemo(() => thread(messages), [messages]);
  const changes = useHeadDiff(bridge, worktree);

  useLayoutEffect(() => {
    const el = list.current;
    if (el && following.current) el.scrollTop = el.scrollHeight;
  }, [items, unechoed]);

  // A jump lands on the approval that waits, else in the composer.
  useEffect(() => {
    if (!shown || !historyLoaded || focusRequest === 0 || focusRequest === focusedRequest.current) return;
    focusedRequest.current = focusRequest;
    (approvalCard.current ?? composer.current!).focus();
  }, [focusRequest, shown, historyLoaded]);

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
  const waiting = items.filter((item): item is RequestItem => "request" in item && isPending(item, refused));
  // A message sent while an approval waits declines it, and tells the agent what to do instead.
  const declining = waiting.find((item): item is ApprovalItem => item.kind === "approval");
  const question = waiting.find((item): item is QuestionItem => item.kind === "question");
  // After an approval answered from its card, the next approval's card takes focus. Until one
  // comes, focus waits in the composer, and stays there once the user types.
  useEffect(() => {
    if (!focusFollowsApproval.current) return;
    const card = approvalCard.current;
    const at = document.activeElement;
    const waitingInComposer = at === composer.current && composer.current!.value === "";
    if (card) {
      if (at === document.body || waitingInComposer) card.focus();
      focusFollowsApproval.current = false;
    } else if (at === document.body) {
      composer.current!.focus();
    } else if (!waitingInComposer) {
      focusFollowsApproval.current = false;
    }
  }, [declining?.request]);

  // The first message waits for the model it runs with.
  const canSend =
    draft.trim() !== "" &&
    !changingAgent &&
    !switchingMode &&
    (declining !== undefined || (!turnRunning && models !== null));
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
    if (declining) return decline(declining.request, text);
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

  // A refused answer means the request has ended.
  function answer(request: string, reply: Reply) {
    return bridge.answer(session, request, reply).catch((e: unknown) => {
      setRefused((known) => [...known, request]);
      throw e;
    });
  }

  function answerApproval(request: string, reply: Reply) {
    focusFollowsApproval.current = approvalCard.current?.contains(document.activeElement) ?? false;
    return answer(request, reply);
  }

  // The message comes back to the composer if the core refuses it.
  function decline(request: string, text: string) {
    setDraft("");
    following.current = true;
    answer(request, { type: "Deny", message: text }).then(
      () => setError(null),
      (e: unknown) => {
        setError(String(e));
        setDraft((typed) => [text, typed].filter(Boolean).join("\n"));
      },
    );
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
          {items.map((item) => (
            <Item
              key={ITEM_KEYS.get(item)}
              item={item}
              agentName={name}
              worktree={worktree}
              turnRunning={turnRunning}
              refused={refused}
              changes={changes}
              onOpenDiff={onOpenDiff}
              onOpenLink={openLink}
            />
          ))}
          {unechoed.map((text, i) => (
            <div key={`unechoed ${i}`} className="msg-user">
              {text}
            </div>
          ))}
        </div>
      </div>
      <div className="composer-wrap">
        {(error ?? modelError) && (
          <p role="alert" className="composer-error">
            {error ?? modelError}
          </p>
        )}
        {notice}
        {declining && (
          <ApprovalCard
            key={declining.request}
            item={declining}
            agentName={name}
            worktree={worktree}
            onAnswer={answerApproval}
            ref={approvalCard}
          />
        )}
        {question && <QuestionCard key={question.request} item={question} agentName={name} onAnswer={answer} />}
        <div className="composer">
          <textarea
            ref={composer}
            aria-label="Message"
            rows={1}
            placeholder={
              declining ? "Send a message to decline and redirect the agent" : `Ask ${name} anything. @ files, / commands`
            }
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={onKeyDown}
          />
          <div className="composer-row">
            <span className="spacer" />
            <Picker
              agent={agent}
              agentLocked={agentLocked}
              models={models}
              choice={choice}
              onAgent={changeAgent}
              onChoose={choose}
            />
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
        {strip}
      </div>
    </>
  );
}

function fitComposer(el: HTMLTextAreaElement) {
  el.style.height = "auto";
  el.style.height = `${el.scrollHeight}px`;
}

/** One turn's work, as its items share it. */
interface Turn {
  /** Where the turn's items start in the message list. */
  start: number;
  ended: boolean;
  /** Set when the turn starts, or at its first tool call when the conversation has no start for it. */
  head: WorkItem | null;
  /** When the turn started and ended, in milliseconds since the Unix epoch. null when unknown. */
  startedAt: number | null;
  endedAt: number | null;
}

/** The header of a turn's work. */
type WorkItem = { kind: "work"; turn: Turn };

/** One thing the message list shows. */
type ThreadItem =
  | { kind: "user"; text: string }
  | { kind: "agent"; text: string }
  | WorkItem
  | { kind: "tools"; calls: ToolCall[]; turn: Turn }
  /** Where the worktree's changed files show, once its turn ends. Only the last one shows. */
  | { kind: "files"; last: boolean }
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
} & (
  | {
      kind: "approval";
      call: ToolCall;
      /** The input the approval came with. A Codex approval's says why, and its call's says what. */
      asked: unknown;
      turn: Turn;
    }
  | { kind: "question"; questions: Question[] }
);

type ApprovalItem = Extract<RequestItem, { kind: "approval" }>;
type QuestionItem = Extract<RequestItem, { kind: "question" }>;

/**
 * Each item's key. A run of calls can leave the list when an approval takes its last call, so a
 * key by position would hand the next row the state of the one before it.
 */
const ITEM_KEYS = new WeakMap<ThreadItem, number>();

/**
 * What the message list shows for `messages`: the agent's deltas joined, each turn's work under
 * one header, each run of tool calls with their results, and a place for the changed files at the
 * end of each turn. An approval takes the place of its tool call's row, and a question's call has
 * no row.
 */
function thread(messages: ChatMessage[]): ThreadItem[] {
  const items: ThreadItem[] = [];
  // Items get their keys in the order they are made, which stays the same as messages come.
  let made = 0;
  function keyed<T extends ThreadItem>(item: T): T {
    ITEM_KEYS.set(item, made++);
    return item;
  }
  const calls = new Map<string, ToolCall>();
  // The current turn's requests, by id.
  const requests = new Map<string, RequestItem>();
  let lastFiles: Extract<ThreadItem, { kind: "files" }> | null = null;
  let turn: Turn = { start: 0, ended: false, head: null, startedAt: null, endedAt: null };

  // The header goes where the turn started, before anything the agent said first.
  function workStarts() {
    if (turn.head) return;
    turn.head = keyed<WorkItem>({ kind: "work", turn });
    items.splice(turn.start, 0, turn.head);
  }

  // The row of the call a request is about, as the agent sent it before the request.
  function takeCall(id: string): ToolCall | undefined {
    const call = calls.get(id);
    if (!call) return undefined;
    const at = items.findIndex((item) => item.kind === "tools" && item.calls.includes(call));
    if (at < 0) return call;
    const run = items[at] as Extract<ThreadItem, { kind: "tools" }>;
    run.calls = run.calls.filter((other) => other !== call);
    if (run.calls.length === 0) items.splice(at, 1);
    return call;
  }

  for (const message of messages) {
    switch (message.type) {
      case "User":
        items.push(keyed({ kind: "user", text: message.text }));
        break;
      case "TurnStarted":
        turn = { start: items.length, ended: false, head: null, startedAt: message.timeUnixMs, endedAt: null };
        // Every turn has its header, so a turn that only talks says how long it worked too.
        workStarts();
        break;
      case "TextDelta": {
        const last = items[items.length - 1];
        if (last?.kind === "agent") last.text += message.text;
        else items.push(keyed({ kind: "agent", text: message.text }));
        break;
      }
      case "ToolCall": {
        // Its approval came first and shows it already.
        if (calls.has(message.id)) break;
        const call = { id: message.id, tool: message.tool, input: message.input, time: message.timeUnixMs };
        calls.set(call.id, call);
        workStarts();
        const last = items[items.length - 1];
        if (last?.kind === "tools") last.calls.push(call);
        else items.push(keyed({ kind: "tools", calls: [call], turn }));
        break;
      }
      case "ToolResult": {
        const call = calls.get(message.id);
        if (call) call.result = { output: message.output, isError: message.isError };
        break;
      }
      case "ApprovalRequested": {
        const { request, tool, input } = message;
        const call = takeCall(request) ?? { id: request, tool, input };
        calls.set(request, call);
        workStarts();
        const item: RequestItem = { kind: "approval", request, call, asked: input, turn, reply: null, expired: false };
        requests.set(request, item);
        items.push(keyed(item));
        break;
      }
      case "QuestionAsked": {
        const { request, questions } = message;
        takeCall(request);
        const item: RequestItem = { kind: "question", request, questions, reply: null, expired: false };
        requests.set(request, item);
        items.push(keyed(item));
        break;
      }
      case "Answered": {
        const item = requests.get(message.request);
        if (item) item.reply = message.reply;
        break;
      }
      case "ReviewBatch":
        items.push(keyed({ kind: "review", comments: message.comments }));
        break;
      case "Resumed":
        items.push(keyed({ kind: "resumed" }));
        break;
      case "TurnEnded":
        turn.ended = true;
        turn.endedAt = message.timeUnixMs;
        requests.forEach((item) => (item.expired = !item.reply));
        requests.clear();
        lastFiles = keyed({ kind: "files", last: false });
        items.push(lastFiles);
        if (message.outcome === "Interrupted") {
          items.push(keyed({ kind: "notice", text: "You interrupted the turn." }));
        } else if (message.outcome === "Failed") {
          items.push(keyed({ kind: "error", text: message.message }));
        }
        break;
      case "NotDelivered":
        items.push(keyed({ kind: "undelivered", text: message.text }));
        break;
    }
  }
  if (lastFiles) lastFiles.last = true;
  return items;
}

/**
 * The worktree's changed files on the `Head` base, loaded again on each `DiffChanged` for it. null
 * until loaded, and when loading fails: the Diff panel reports that.
 */
function useHeadDiff(bridge: Bridge, worktree: string): FileDiff[] | null {
  const [loaded, setLoaded] = useState<{ worktree: string; files: FileDiff[] | null } | null>(null);
  const [changes, setChanges] = useState(0);

  useEffect(
    () => bridge.onDiffChanged((changed) => changed === worktree && setChanges((n) => n + 1)),
    [bridge, worktree],
  );

  useEffect(() => {
    let current = true;
    bridge.diff(worktree, "Head").then(
      (files) => current && setLoaded({ worktree, files }),
      () => current && setLoaded({ worktree, files: null }),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, changes]);

  return loaded?.worktree === worktree ? loaded.files : null;
}

/** Waiting on the user: no reply yet, the turn still runs, and the core has not refused an answer. */
function isPending(item: RequestItem, refused: string[]): boolean {
  return !item.reply && !item.expired && !refused.includes(item.request);
}

function Item({
  item,
  agentName,
  worktree,
  turnRunning,
  refused,
  changes,
  onOpenDiff,
  onOpenLink,
}: {
  item: ThreadItem;
  agentName: string;
  worktree: string;
  /** The session's latest turn runs, by its agent state. */
  turnRunning: boolean;
  /** Requests the core refused an answer to. */
  refused: string[];
  /** The worktree's changed files. null until known. */
  changes: FileDiff[] | null;
  onOpenDiff(path?: string): void;
  onOpenLink(link: string): void;
}) {
  switch (item.kind) {
    case "work":
      return <WorkHead turn={item.turn} running={turnRunning && !item.turn.ended} />;
    case "tools":
      return <ToolCalls calls={item.calls} worktree={worktree} live={turnRunning && !item.turn.ended} />;
    case "approval":
      return (
        <Approval
          item={item}
          worktree={worktree}
          live={turnRunning && !item.turn.ended}
          pending={isPending(item, refused)}
        />
      );
    case "question":
      // Pending, it is a card above the composer.
      if (isPending(item, refused)) return null;
      if (item.reply) return <div className="record">{describeAnswers(item, item.reply)}</div>;
      return <div className="record expired">Expired: {item.questions[0]?.question}</div>;
    case "user":
      return <div className="msg-user">{item.text}</div>;
    case "agent":
      return <AgentMarkdown text={item.text} onOpenLink={onOpenLink} />;
    case "files":
      if (!item.last || turnRunning || !changes?.length) return null;
      return <ChangedFiles files={changes} onOpenDiff={onOpenDiff} />;
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

/** The worktree's changed files after the last turn, with their counts, each opening the diff. */
function ChangedFiles({ files, onOpenDiff }: { files: FileDiff[]; onOpenDiff(path?: string): void }) {
  const label = `${files.length} changed ${files.length === 1 ? "file" : "files"}`;
  const added = files.reduce((sum, file) => sum + file.added, 0);
  const removed = files.reduce((sum, file) => sum + file.removed, 0);
  return (
    <div className="files-card" role="group" aria-label={label}>
      <div className="files-head">
        <span>{label}</span>
        <Counts added={added} removed={removed} />
        <span className="spacer" />
        <Button size="xs" onClick={() => onOpenDiff()}>
          Review diff
        </Button>
      </div>
      <ul className="files-rows">
        {files.map((file) => (
          <li key={file.newPath}>
            <button type="button" className="files-row" onClick={() => onOpenDiff(file.newPath)}>
              <span className="files-path">{file.newPath}</span>
              <Counts added={file.added} removed={file.removed} />
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

function Counts({ added, removed }: { added: number; removed: number }) {
  return (
    <span className="files-counts">
      <span className="added">+{added}</span>
      <span className="removed">−{removed}</span>
    </span>
  );
}

/**
 * The header of a turn's work: "Working for 1m 3s" while the turn runs, counted each second from
 * its start, and "Worked for 2m 14s" after. Just "Working" or "Worked" when a time is unknown.
 */
function WorkHead({ turn, running }: { turn: Turn; running: boolean }) {
  const now = useNow(running && turn.startedAt !== null);
  const { startedAt, endedAt } = turn;
  let text = running ? "Working" : "Worked";
  if (running && startedAt !== null) text += ` for ${duration(now - startedAt)}`;
  if (!running && startedAt !== null && endedAt !== null) text += ` for ${duration(endedAt - startedAt)}`;
  return (
    <div className="work-head">
      <span className={running ? "shimmer" : undefined}>{text}</span>
    </div>
  );
}

/** The time now, in milliseconds since the Unix epoch, read again each second while `ticking`. */
function useNow(ticking: boolean): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    if (!ticking) return;
    const tick = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tick);
  }, [ticking]);
  return now;
}

/** Where a review comment is, as the agent reads it: `path:line` or `path:first-last`. */
function placeOf({ path, startLine, endLine }: BatchComment): string {
  return startLine === endLine ? `${path}:${startLine}` : `${path}:${startLine}-${endLine}`;
}

/** An approval: the call's row, and under it what the user did. Pending, its card is above the composer. */
function Approval({
  item,
  worktree,
  live,
  pending,
}: {
  item: ApprovalItem;
  worktree: string;
  live: boolean;
  pending: boolean;
}) {
  return (
    <div className="approval">
      {item.reply?.type === "Deny" ? (
        <DeclinedCall call={item.call} worktree={worktree} />
      ) : (
        <ToolCalls calls={[item.call]} worktree={worktree} live={live} />
      )}
      {item.reply ? (
        <div className="approval-line record">{describeApproval(item.reply)}</div>
      ) : (
        !pending && <div className="approval-line record expired">Expired</div>
      )}
    </div>
  );
}

/**
 * A pending approval, as a card above the composer: what the agent wants to do, and the choices.
 * While the card has focus, Y approves and N declines. It answers once.
 */
function ApprovalCard({
  item,
  agentName,
  worktree,
  onAnswer,
  ref,
}: {
  item: ApprovalItem;
  agentName: string;
  worktree: string;
  onAnswer(request: string, reply: Reply): Promise<void>;
  ref: Ref<HTMLDivElement>;
}) {
  const [sending, setSending] = useState(false);
  const { action, target, why } = askedFor(item, worktree);

  function answer(reply: Reply) {
    if (sending) return;
    setSending(true);
    // The card goes when the core's `Answered` comes. A refused answer expires it.
    onAnswer(item.request, reply).catch(() => {});
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const reply = APPROVAL_KEYS[event.key.toLowerCase()];
    if (!reply) return;
    event.preventDefault();
    answer(reply);
  }

  return (
    <div
      ref={ref}
      className="approval-card"
      role="group"
      aria-label={`${agentName} wants to ${action}`}
      tabIndex={-1}
      onKeyDown={onKeyDown}
    >
      <div className="approval-head">
        <Shield aria-hidden="true" />
        <span>
          {agentName} wants to {action}
        </span>
      </div>
      {target && (
        <pre className="approval-target">
          <code>{target}</code>
        </pre>
      )}
      {why && <p className="approval-why">{why}</p>}
      <div className="approval-actions">
        <Button variant="ghost" size="sm" disabled={sending} onClick={() => answer({ type: "Deny", message: null })}>
          Decline
          <kbd aria-hidden="true">N</kbd>
        </Button>
        <Button size="sm" disabled={sending} onClick={() => answer({ type: "AllowForSession" })}>
          Always allow this session
        </Button>
        <Button variant="primary" size="sm" disabled={sending} onClick={() => answer({ type: "AllowOnce" })}>
          Approve
          <kbd aria-hidden="true">Y</kbd>
        </Button>
      </div>
    </div>
  );
}

/** The replies the approval card's keys send. */
const APPROVAL_KEYS: Record<string, Reply> = {
  y: { type: "AllowOnce" },
  n: { type: "Deny", message: null },
};

interface AskedFor {
  action: string;
  target: string | null;
  why: string | null;
}

/**
 * What an approval asks for: the action as the card's headline says it, the command or files it
 * acts on, and why, when the agent says. Requested permissions, and the input of a tool the panel
 * does not know, show whole, since approving grants all of it.
 */
function askedFor(item: ApprovalItem, worktree: string): AskedFor {
  const asked = (item.asked ?? {}) as Record<string, unknown>;
  const call = (item.call.input ?? {}) as Record<string, unknown>;
  const text = (name: string) =>
    [asked[name], call[name]].find((value): value is string => typeof value === "string" && value !== "") ?? null;
  // Claude Code says what a command is for as its description, Codex as its reason.
  const why = text("description") ?? text("reason");
  switch (item.call.tool) {
    case "Bash":
    case "commandExecution":
      return { action: "run a command", target: text("command"), why };
    case "Write":
    case "Edit":
    case "MultiEdit":
    case "NotebookEdit": {
      const path = text("file_path") ?? text("notebook_path");
      if (!path) break;
      const action = item.call.tool === "Write" ? "write a file" : "edit a file";
      return { action, target: relative(path, worktree), why };
    }
    case "fileChange": {
      const changes = Array.isArray(call.changes) ? (call.changes as { path?: unknown }[]) : [];
      const paths = changes.map((change) => change?.path).filter((path): path is string => typeof path === "string");
      if (paths.length === 0) return { action: "edit files", target: null, why };
      const action = paths.length === 1 ? "edit a file" : `edit ${paths.length} files`;
      return { action, target: paths.map((path) => relative(path, worktree)).join("\n"), why };
    }
    // Codex asks for wider sandbox access.
    case "permissions":
      return { action: "change its permissions", target: whole(asked.permissions), why };
  }
  return { action: `use ${item.call.tool}`, target: whole(item.asked), why };
}

/** `value` as indented JSON. null when it has nothing to show. */
function whole(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value === "object" && Object.keys(value).length === 0) return null;
  return JSON.stringify(value, null, 2);
}

/**
 * A pending question, as a card above the composer. Each question of the request shows in turn,
 * with its options and a field for the user's own answer, which takes the place of any option
 * picked. The last one sends every answer, once.
 */
function QuestionCard({
  item,
  agentName,
  onAnswer,
}: {
  item: QuestionItem;
  agentName: string;
  onAnswer(request: string, reply: Reply): Promise<void>;
}) {
  const { request, questions } = item;
  const [at, setAt] = useState(0);
  const [picked, setPicked] = useState<string[][]>(() => questions.map(() => []));
  const [typed, setTyped] = useState<string[]>(() => questions.map(() => ""));
  const [sending, setSending] = useState(false);
  // In the options' order, whatever order they were picked in.
  const answers = questions.map((q, i) =>
    typed[i].trim()
      ? [typed[i].trim()]
      : q.options.map((o) => o.label).filter((label) => picked[i].includes(label)),
  );
  const question = questions[at];
  const last = at === questions.length - 1;

  function pick(label: string) {
    const was = picked[at];
    const now = !question.multiSelect ? [label] : was.includes(label) ? was.filter((l) => l !== label) : [...was, label];
    setPicked(picked.map((p, j) => (j === at ? now : p)));
    setTyped(typed.map((t, j) => (j === at ? "" : t)));
  }

  function type(text: string) {
    setTyped(typed.map((t, j) => (j === at ? text : t)));
    if (text) setPicked(picked.map((p, j) => (j === at ? [] : p)));
  }

  return (
    <div className="question-card" role="group" aria-label={`${agentName} is asking`}>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (answers[at].length === 0 || sending) return;
          if (!last) return setAt(at + 1);
          setSending(true);
          // The card goes when the core's `Answered` comes. A refused answer expires it.
          onAnswer(request, { type: "Answers", answers }).catch(() => {});
        }}
      >
        <fieldset key={at} className="question" disabled={sending}>
          <legend className="question-text">{question.question}</legend>
          {question.options.map((option) => (
            <label key={option.label} className="option">
              <input
                type={question.multiSelect ? "checkbox" : "radio"}
                name={`${request} ${at}`}
                checked={picked[at].includes(option.label)}
                onChange={() => pick(option.label)}
              />
              <span>
                {option.label}
                {option.description && <span className="option-description">{option.description}</span>}
              </span>
            </label>
          ))}
          <input
            type="text"
            className="own-answer"
            aria-label={`Your own answer to ${question.header}`}
            placeholder="Or type your own answer"
            value={typed[at]}
            onChange={(event) => type(event.target.value)}
          />
        </fieldset>
        <div className="question-actions">
          <Button
            type="submit"
            variant="primary"
            size="sm"
            className="question-submit"
            disabled={answers[at].length === 0 || sending}
          >
            {last ? "Submit answer" : "Next question"}
          </Button>
        </div>
      </form>
    </div>
  );
}

/** The record of an answered approval, such as "Approved" or "Declined: use vitest". */
function describeApproval(reply: Reply): string {
  switch (reply.type) {
    case "AllowOnce":
      return "Approved";
    case "AllowForSession":
      return "Approved for this session";
    case "Deny":
      return reply.message ? `Declined: ${reply.message}` : "Declined";
    case "Answers":
      return "Answered";
  }
}

/** The record of an answered question, such as "Answered: vitest". */
function describeAnswers(item: QuestionItem, reply: Reply): string {
  if (reply.type !== "Answers") return "Declined the question";
  const headers = item.questions.length > 1 ? item.questions : null;
  const answers = reply.answers.map((a, i) => (headers ? `${headers[i].header}: ` : "") + a.join(", "));
  return `Answered: ${answers.join("; ")}`;
}

/** A span of time as "45s", "1m 3s" or "1h 2m". */
function duration(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const [hours, minutes] = [Math.floor(seconds / 3600), Math.floor(seconds / 60) % 60];
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds % 60}s`;
  return `${seconds}s`;
}
