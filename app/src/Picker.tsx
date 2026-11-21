import { Asterisk, Hexagon } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { Agent, Bridge, Model, SessionId } from "./bridge";
import { openedByPointer } from "./motion";
import { AGENT_NAMES } from "./Sidebar";
import { Segmented } from "./ui/Segmented";

const AGENTS: Agent[] = ["Claude", "Codex"];

// The built-in effort for a model whose agent names no default of its own, as Claude's SDK does.
// Profiles give the defaults once #125 lands.
const BUILT_IN_EFFORT = "high";

/** The model and effort a session runs with. `effort` is null for a model that takes none. */
export interface ModelChoice {
  model: Model;
  effort: string | null;
}

/**
 * The session's model list and its current choice: the one the core saved for it, or for a session
 * with none yet, the agent's default model at that model's default effort. `models` is null until
 * the agent lists them, and empty when it lists none, which leaves the agent on its own default.
 */
export function useModelChoice(bridge: Bridge, session: SessionId) {
  // Kept with the session it is for, so a panel that takes another session starts over.
  const [state, setState] = useState<{
    session: SessionId;
    models: Model[];
    choice: ModelChoice | null;
    error: string | null;
  }>();
  const mine = state?.session === session ? state : undefined;

  useEffect(() => {
    let current = true;
    const failed = (e: unknown) => current && setState((s) => s && { ...s, error: String(e) });
    Promise.all([bridge.models(session), bridge.modelChoice(session)]).then(
      ([models, saved]) => {
        if (!current) return;
        // The session's own choice, which the core keeps across mode switches and restarts.
        const savedModel = saved && models.find((m) => m.id === saved.model);
        if (savedModel) {
          const choice = { model: savedModel, effort: saved.effort };
          setState({ session, models, choice, error: null });
          return;
        }
        const model = models.find((m) => m.isDefault) ?? models[0];
        const choice = model ? { model, effort: effortFor(model, null) } : null;
        setState({ session, models, choice, error: null });
        if (choice) bridge.setModel(session, choice.model.id, choice.effort).catch(failed);
      },
      (e: unknown) => current && setState({ session, models: [], choice: null, error: String(e) }),
    );
    return () => {
      current = false;
    };
  }, [bridge, session]);

  function choose(choice: ModelChoice) {
    setState((s) => s && { ...s, choice, error: null });
    bridge
      .setModel(session, choice.model.id, choice.effort)
      .catch((e: unknown) => setState((s) => (s?.session === session ? { ...s, error: String(e) } : s)));
  }

  return { models: mine?.models ?? null, choice: mine?.choice ?? null, choose, error: mine?.error ?? null };
}

/** The effort `model` runs at: `wanted` when it takes it, else its default, else the built-in one. */
function effortFor(model: Model, wanted: string | null): string | null {
  if (wanted && model.efforts.includes(wanted)) return wanted;
  if (model.defaultEffort && model.efforts.includes(model.defaultEffort)) return model.defaultEffort;
  return model.efforts.includes(BUILT_IN_EFFORT) ? BUILT_IN_EFFORT : (model.efforts[0] ?? null);
}

/** What the picker's button says about the model: its name, and its effort when it takes one. */
function describeChoice(models: Model[] | null, choice: ModelChoice | null): { name: string; effort: string | null } {
  if (models === null) return { name: "Loading models", effort: null };
  if (!choice) return { name: "Agent's default model", effort: null };
  return { name: choice.model.name, effort: choice.effort };
}

const MARKS: Record<Agent, typeof Asterisk> = { Claude: Asterisk, Codex: Hexagon };

/** The agent's mark, in the agent's color. Its title names the agent. */
export function AgentMark({ agent }: { agent: Agent }) {
  const Mark = MARKS[agent];
  return (
    <span className={`agent-mark ${agent}`} role="img" aria-label={AGENT_NAMES[agent]} title={AGENT_NAMES[agent]}>
      <Mark aria-hidden="true" />
    </span>
  );
}

/**
 * The button in the composer's footer, with the agent's mark, the model and its effort, and its popover: the agent, until `agentLocked`, then the model
 * and its effort. Picking an agent or a model closes it. Picking an effort leaves it open.
 */
export function Picker({
  agent,
  agentLocked,
  models,
  choice,
  onAgent,
  onChoose,
}: {
  agent: Agent;
  /** The conversation has a message, so the agent is fixed. */
  agentLocked: boolean;
  models: Model[] | null;
  choice: ModelChoice | null;
  onAgent(agent: Agent): void;
  onChoose(choice: ModelChoice): void;
}) {
  const [open, setOpen] = useState(false);
  const [entering, setEntering] = useState(false);
  // Opened while the models load, it keeps one height until it closes, so they fill in below the
  // rows already shown and no row moves under the pointer.
  const [held, setHeld] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const outside = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => event.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("mousedown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  function pickAgent(picked: Agent) {
    setOpen(false);
    if (picked !== agent && !agentLocked) onAgent(picked);
  }

  const { name, effort } = describeChoice(models, choice);

  function pickModel(model: Model) {
    setOpen(false);
    onChoose({ model, effort: effortFor(model, choice?.effort ?? null) });
  }

  return (
    <div className="picker" ref={root}>
      <button
        className="picker-btn"
        aria-label="Agent and model"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => {
          setEntering(!open && openedByPointer());
          setHeld(!open && models === null);
          setOpen(!open);
        }}
      >
        <AgentMark agent={agent} />
        <b>{name}</b>
        {effort && (
          <>
            {" "}
            <span className="picker-effort">{effort}</span>
          </>
        )}
      </button>
      {open && (
        <div
          className={["pop picker-pop", entering && "popover-enter", held && "picker-pop-held"]
            .filter(Boolean)
            .join(" ")}
          role="dialog"
          aria-label="Agent and model"
        >
          <h4>Agent</h4>
          {agentLocked && <p className="note">Fixed after the first message.</p>}
          {AGENTS.map((a) => (
            <button
              key={a}
              className="opt"
              aria-pressed={a === agent}
              disabled={agentLocked && a !== agent}
              onClick={() => pickAgent(a)}
            >
              <span className="grow">{AGENT_NAMES[a]}</span>
            </button>
          ))}
          <h4>Model</h4>
          {models === null && (
            <p className="note" role="status">
              Loading models
            </p>
          )}
          {models?.length === 0 && (
            <p className="note">{AGENT_NAMES[agent]} listed no models, so it uses its own default.</p>
          )}
          {models?.map((model) => (
            <button
              key={model.id}
              className="opt"
              aria-pressed={model.id === choice?.model.id}
              onClick={() => pickModel(model)}
            >
              <span className="grow">
                {model.name}
                {model.description && <span className="why">{model.description}</span>}
              </span>
            </button>
          ))}
          {choice && choice.model.efforts.length > 0 && (
            <>
              <h4>Effort</h4>
              <Segmented
                label="Effort"
                options={choice.model.efforts.map((effort) => [effort, effort] as const)}
                value={choice.effort}
                onChange={(effort) => onChoose({ model: choice.model, effort })}
              />
            </>
          )}
        </div>
      )}
    </div>
  );
}
