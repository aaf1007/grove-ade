import { type KeyboardEvent, useEffect, useRef, useState } from "react";

import type { Agent, AgentStateChanged, Bridge, Mode, ReviewComment, SessionId } from "./bridge";
import { AGENT_NAMES } from "./Sidebar";
import { Button } from "./ui/Button";

/** The worktree's open and sent comments, oldest first, loaded again on each `CommentsChanged` for it. */
export function useComments(bridge: Bridge, worktree: string) {
  const [comments, setComments] = useState<ReviewComment[]>([]);
  const [error, setError] = useState<string | null>(null);
  // Bumped on each CommentsChanged for this worktree, to load the comments again.
  const [changes, setChanges] = useState(0);

  useEffect(
    () =>
      bridge.onCommentsChanged((changed) => {
        if (changed === worktree) setChanges((n) => n + 1);
      }),
    [bridge, worktree],
  );

  useEffect(() => {
    // A later load replaces this one, so this one drops its answer.
    let current = true;
    bridge.comments(worktree).then(
      (loaded) => {
        if (!current) return;
        setComments(loaded);
        setError(null);
      },
      (e: unknown) => current && setError(String(e)),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, changes]);

  return { comments, error };
}

/** "line 3" or "lines 3 to 5". */
export function describeLines(startLine: number, endLine: number): string {
  return startLine === endLine ? `line ${startLine}` : `lines ${startLine} to ${endLine}`;
}

/** A review comment with its state, a flag when it may be addressed, and Resolve. */
export function CommentBox({
  comment,
  label,
  onResolve,
}: {
  comment: ReviewComment;
  /** Names the comment, such as "Comment on line 3". */
  label: string;
  onResolve(): void;
}) {
  return (
    <div className="cmt" role="group" aria-label={label}>
      <div className="cmt-head">
        <span>You · {describeLines(comment.startLine, comment.endLine)}</span>
        <span className={`cmt-status ${comment.state}`}>{comment.state}</span>
        {comment.possiblyAddressed && (
          <span className="cmt-flag" title="Its lines are no longer in the file, so the agent may have addressed it.">
            Possibly addressed
          </span>
        )}
        <span className="spacer" />
        <Button size="xs" onClick={onResolve}>
          Resolve
        </Button>
      </div>
      <p className="cmt-body">{comment.body}</p>
    </div>
  );
}

/** The box a new comment is typed into. Escape cancels, Cmd-Enter adds. */
export function NewCommentBox({
  startLine,
  endLine,
  linesChanged,
  onAdd,
  onCancel,
}: {
  startLine: number;
  endLine: number;
  /** The agent changed the picked lines since, so the comment cannot be added to them. */
  linesChanged: boolean;
  /** Rejects with the core's error, which the box shows. */
  onAdd(body: string): Promise<void>;
  onCancel(): void;
}) {
  const [body, setBody] = useState("");
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const lines = describeLines(startLine, endLine);

  // The box is in the page only once Monaco has its zone, after this effect, so focus waits a frame.
  useEffect(() => {
    const frame = requestAnimationFrame(() => field.current?.focus());
    return () => cancelAnimationFrame(frame);
  }, []);

  function add() {
    if (body.trim() === "" || adding || linesChanged) return;
    setAdding(true);
    onAdd(body).catch((e: unknown) => {
      setError(String(e));
      setAdding(false);
    });
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Escape") onCancel();
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      add();
    }
  }

  return (
    <form
      className="cmt"
      aria-label={`New comment on ${lines}`}
      onSubmit={(event) => {
        event.preventDefault();
        add();
      }}
    >
      <div className="cmt-head">
        <span>Comment on {lines}</span>
      </div>
      <textarea
        aria-label="Comment"
        placeholder="What should the agent change?"
        ref={field}
        value={body}
        onChange={(event) => setBody(event.target.value)}
        onKeyDown={onKeyDown}
      />
      {linesChanged && <p role="alert">Those lines changed. Select them again.</p>}
      {error && <p role="alert">{error}</p>}
      <div className="row">
        <Button type="button" size="xs" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" size="xs" disabled={body.trim() === "" || adding || linesChanged}>
          Add comment
        </Button>
      </div>
    </form>
  );
}

/** An agent session a review batch may go to, as the core lists it when the picker opens. */
interface Target {
  session: SessionId;
  agent: Agent;
  mode: Mode;
  /** Terminal mode only: whether bracketed paste is on, or why the core could not say. */
  paste: boolean | string | null;
}

/**
 * Picks the agent session a review batch goes to: the worktree's most recently focused one by
 * default. Each session that cannot take it now says why. Escape closes it.
 */
export function ReviewPicker({
  bridge,
  worktree,
  count,
  agentStates,
  onClose,
  onSent,
}: {
  bridge: Bridge;
  worktree: string;
  /** How many comments are open. */
  count: number;
  agentStates: Record<SessionId, AgentStateChanged>;
  onClose(): void;
  onSent(session: SessionId): void;
}) {
  // null until the core has listed them.
  const [listed, setListed] = useState<{ targets: Target[]; focusedLast: SessionId | null } | null>(null);
  const [picked, setPicked] = useState<SessionId | null>(null);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialog = useRef<HTMLDivElement>(null);

  // Once, so a later render leaves focus on the option the user moved to.
  useEffect(() => dialog.current?.focus(), []);

  useEffect(() => {
    let current = true;
    Promise.all([bridge.sessions(worktree), bridge.lastFocusedAgent(worktree)])
      .then(async ([sessions, focusedLast]) => {
        const targets = await Promise.all(
          sessions.flatMap((s) =>
            s.kind === "Agent" && s.running
              ? [
                  (s.mode === "Terminal"
                    ? bridge.bracketedPaste(s.session).catch((e: unknown) => String(e))
                    : Promise.resolve(null)
                  ).then((paste): Target => ({ session: s.session, agent: s.agent, mode: s.mode, paste })),
                ]
              : [],
          ),
        );
        if (current) setListed({ targets, focusedLast });
      })
      .catch((e: unknown) => current && setError(String(e)));
    return () => {
      current = false;
    };
  }, [bridge, worktree]);

  const targets = listed?.targets ?? [];
  const focusedLast = targets.some((t) => t.session === listed?.focusedLast) ? listed!.focusedLast : null;
  const pick = picked ?? focusedLast ?? targets.find((t) => !blocker(t, agentStates[t.session]))?.session ?? null;
  const pickedTarget = targets.find((t) => t.session === pick);
  const canSend = !!pickedTarget && !blocker(pickedTarget, agentStates[pickedTarget.session]) && !sending;

  function send() {
    if (!canSend || pick === null) return;
    setSending(true);
    bridge.sendReviewBatch(worktree, pick).then(
      () => {
        onSent(pick);
        onClose();
      },
      (e: unknown) => {
        setError(String(e));
        setSending(false);
      },
    );
  }

  return (
    <div
      className="pop review-pop"
      role="dialog"
      aria-label="Send review batch"
      tabIndex={-1}
      ref={dialog}
      onKeyDown={(event) => event.key === "Escape" && onClose()}
    >
      <h4>Send {count === 1 ? "1 comment" : `${count} comments`} to</h4>
      <p className="note">Defaults to the agent session you focused last.</p>
      {listed && targets.length === 0 && <p className="note">No agent session in this worktree yet.</p>}
      {targets.map((target) => {
        const why = blocker(target, agentStates[target.session]);
        const name = `${AGENT_NAMES[target.agent]} · ${target.mode.toLowerCase()} mode`;
        const note = why ?? (target.session === focusedLast ? "Focused last" : null);
        return (
          <button
            key={target.session}
            className="opt"
            aria-label={note ? `${name}. ${note}` : name}
            aria-pressed={target.session === pick && !why}
            disabled={why !== null}
            onClick={() => setPicked(target.session)}
          >
            <span className="grow">
              {name}
              {note && <span className="why">{note}</span>}
            </span>
          </button>
        );
      })}
      {error && (
        <p role="alert" className="pop-error">
          {error}
        </p>
      )}
      <div className="pop-actions">
        <Button size="xs" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="primary" size="xs" disabled={!canSend} onClick={send}>
          Send
        </Button>
      </div>
    </div>
  );
}

/** Why the session cannot take a review batch now, or null when it can. Only Idle or Done takes one. */
function blocker({ mode, paste }: Target, change: AgentStateChanged | undefined): string | null {
  if (!change) return "No state yet: only an Idle or Done session takes a review batch";
  if (change.state === "Working") return "Working: wait for the turn to end";
  if (change.state === "NeedsInput") return `NeedsInput: answer its ${change.kind.toLowerCase()} first`;
  if (change.state !== "Idle" && change.state !== "Done") {
    return `${change.state}: only an Idle or Done session takes a review batch`;
  }
  if (mode === "Terminal" && paste !== true) {
    return typeof paste === "string" ? paste : "Bracketed paste is off in its TUI";
  }
  return null;
}
