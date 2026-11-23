import { X } from "lucide-react";
import { type KeyboardEvent, type Ref, useEffect, useRef, useState } from "react";

import { AgentMarkdown } from "./AgentMarkdown";
import type { AgentMessage, Bridge, SessionId } from "./bridge";
import { isInputMethodKey } from "./shortcuts";
import "./SideQuestion.css";
import { IconButton } from "./ui/IconButton";

/** The side question the popover shows, and what has come of its answer. */
interface Asked {
  question: string;
  answer: string;
  /** Why the fork gave no answer, or stopped before the answer was whole. */
  failure: string | null;
  /** Whether more of the answer can still come. */
  running: boolean;
}

function withMessage(asked: Asked, message: AgentMessage): Asked {
  switch (message.type) {
    case "TextDelta":
      return { ...asked, answer: asked.answer + message.text };
    case "TurnEnded":
      return { ...asked, running: false, failure: message.outcome === "Failed" ? message.message : null };
    default:
      return asked;
  }
}

/**
 * The side question popover over the composer. Enter asks the agent in a fork of the session's
 * conversation, and the answer streams in here and never joins the conversation. Each question
 * forks the conversation again, so it knows nothing of the side question before it. Esc closes the
 * popover. Closing it, or taking it off the screen, ends the fork.
 */
export function SideQuestion({
  ref,
  bridge,
  session,
  agentName,
  onOpenLink,
  onClose,
}: {
  /** The input, which takes focus when the popover opens. */
  ref: Ref<HTMLInputElement>;
  bridge: Bridge;
  session: SessionId;
  agentName: string;
  onOpenLink(link: string): void;
  onClose(): void;
}) {
  const [draft, setDraft] = useState("");
  const [asked, setAsked] = useState<Asked | null>(null);
  // Stops drawing the question on its way and, with `endFork`, ends its fork in the core.
  const leave = useRef<(endFork: boolean) => void>(() => {});

  useEffect(() => () => leave.current(true), []);

  function ask() {
    const question = draft.trim();
    if (question === "") return;
    // The core ends the earlier fork itself when the new question takes its place.
    leave.current(false);
    setDraft("");
    setAsked({ question, answer: "", failure: null, running: true });
    let current = true;
    let frame = 0;
    const queued: AgentMessage[] = [];
    function draw() {
      frame = 0;
      const batch = queued.splice(0);
      setAsked((shown) => shown && batch.reduce(withMessage, shown));
    }
    const sideQuestion = bridge.askSideQuestion(session, question, (batch) => {
      if (!current) return;
      queued.push(...batch);
      // However many frames come, the popover draws once per animation frame.
      if (!frame) frame = requestAnimationFrame(draw);
    });
    sideQuestion.catch((e: unknown) => {
      if (current) setAsked((shown) => shown && { ...shown, failure: String(e), running: false });
    });
    leave.current = (endFork) => {
      current = false;
      if (frame) cancelAnimationFrame(frame);
      // A question the core refused has no fork to end.
      if (endFork) sideQuestion.then((id) => bridge.endSideQuestion(session, id)).catch(() => {});
    };
  }

  // These fire only with focus in the popover. A key an input method took is not the popover's.
  function onKeyDown(event: KeyboardEvent) {
    if (isInputMethodKey(event.nativeEvent)) return;
    if (event.key === "Escape") {
      // Taken here, so the panel's Esc never stops the turn from the popover.
      event.preventDefault();
      onClose();
    } else if (event.key === "Enter" && !event.shiftKey && event.target instanceof HTMLInputElement) {
      event.preventDefault();
      ask();
    }
  }

  return (
    <div className="side-question" role="dialog" aria-label="Side question" onKeyDown={onKeyDown}>
      <div className="side-question-head">
        <b>Side question</b>
        <span>Stays out of the conversation</span>
        <IconButton icon={X} size="xs" label="Close side question" onClick={onClose} />
      </div>
      {asked && (
        <div className="side-question-thread">
          <p className="side-question-asked">{asked.question}</p>
          {asked.running && asked.answer === "" && <p role="status">Asking {agentName}</p>}
          {asked.answer !== "" && (
            <div className="side-question-answer">
              <AgentMarkdown text={asked.answer} onOpenLink={onOpenLink} />
            </div>
          )}
          {asked.failure !== null && <p role="alert">{asked.failure}</p>}
        </div>
      )}
      <input
        ref={ref}
        // The user opened the popover to type here.
        autoFocus
        aria-label="Side question"
        placeholder={`Ask ${agentName} on the side. Enter sends, Esc closes`}
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
      />
    </div>
  );
}
