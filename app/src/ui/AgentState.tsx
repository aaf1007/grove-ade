import {
  Circle,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CircleDot,
  type LucideIcon,
  MessageCircleQuestionMark,
  Shield,
} from "lucide-react";

import type { AgentState as State, StateSource } from "../bridge";
import "./AgentState.css";

/** How each agent state looks: its word, which GLOSSARY.md names, its icon and its color class. */
const LOOKS = {
  Working: { word: "Working", icon: CircleDashed },
  Approval: { word: "Approval", icon: Shield },
  Question: { word: "Question", icon: MessageCircleQuestionMark },
  Failed: { word: "Failed", icon: CircleAlert },
  Done: { word: "Done", icon: CircleCheck },
  Idle: { word: "Idle", icon: CircleDot },
  Exited: { word: "Exited", icon: Circle },
} satisfies Record<string, { word: string; icon: LucideIcon }>;

/** The state's word: the NeedsInput kind for NeedsInput, the state's name otherwise. */
export function stateWord(state: State): string {
  return LOOKS[lookOf(state)].word;
}

function lookOf(state: State): keyof typeof LOOKS {
  return state.state === "NeedsInput" ? state.kind : state.state;
}

/**
 * An agent session's state as an icon and a word, in the state's color. Pass `source` to show
 * where the state came from: the pane head does, the sidebar does not. Pass `word` to show it in
 * place of the state's own, as the sidebar does for a running setup script.
 */
export function AgentState({ state, source, word }: { state: State; source?: StateSource; word?: string }) {
  const look = lookOf(state);
  const { icon: Icon } = LOOKS[look];
  return (
    <span className={`ui-agent-state ui-agent-state-${look.toLowerCase()}`}>
      <Icon aria-hidden="true" />
      <span>{word ?? LOOKS[look].word}</span>
      {source && <span className="ui-agent-state-source"> · {source}</span>}
    </span>
  );
}
