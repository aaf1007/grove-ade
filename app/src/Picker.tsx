import { Asterisk, Hexagon } from "lucide-react";
import {
  type FocusEvent,
  type KeyboardEvent,
  type Ref,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";

import type { Agent, Bridge, Model, SessionId } from "./bridge";
import { openedByPointer } from "./motion";
import { isInputMethodKey, matchesKeys, shortcut } from "./shortcuts";
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

/** A list in the popover that a shortcut can open it on. */
export type PickerList = "model" | "effort";

/** What the picker's owner can ask of it. */
export interface PickerHandle {
  /**
   * Opens the popover on `list`, or moves an open one to it. Focus goes to the list's current
   * item. A digit 1 to 9 picks the item at that place in the list and closes the popover.
   */
  openOn(list: PickerList): void;
  /** Closes the popover. Says whether it was open. */
  close(): boolean;
}

// A selector for each list's buttons in the popover.
const LIST_ITEMS: Record<PickerList, string> = {
  model: "button[data-picker-list=model]",
  effort: "[role=group][aria-label=Effort] button",
};

/**
 * The button in the composer's footer, with the agent's mark, the model and its effort, and its popover: the agent, until `agentLocked`, then the model
 * and its effort. Picking an agent or a model closes it. Picking an effort leaves it open, unless
 * a digit picked it. The picker takes Escape and digits only while focus is in it, and the popover
 * closes when focus leaves the picker for another place.
 */
export function Picker({
  agent,
  agentLocked,
  models,
  choice,
  onAgent,
  onChoose,
  ref,
}: {
  agent: Agent;
  /** The conversation has a message, so the agent is fixed. */
  agentLocked: boolean;
  models: Model[] | null;
  choice: ModelChoice | null;
  onAgent(agent: Agent): void;
  onChoose(choice: ModelChoice): void;
  ref?: Ref<PickerHandle>;
}) {
  const [open, setOpen] = useState(false);
  // The list a digit picks from. None after a click opened the popover.
  const [list, setList] = useState<PickerList | null>(null);
  const [entering, setEntering] = useState(false);
  // Opened while the models load, it keeps one height until it closes, so they fill in below the
  // rows already shown and no row moves under the pointer.
  const [held, setHeld] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  // What had focus when a shortcut opened the popover. It gets focus back.
  const opener = useRef<HTMLElement | null>(null);

  useImperativeHandle(ref, () => ({
    openOn(wanted) {
      const focused = document.activeElement;
      if (focused instanceof HTMLElement && !pop.current?.contains(focused)) opener.current = focused;
      if (!open) {
        setEntering(openedByPointer());
        setHeld(models === null);
      }
      setList(wanted);
      setOpen(true);
    },
    close: () => {
      if (open) close();
      return open;
    },
  }));

  // The popover takes its focused item away with it, so focus goes back to the opener, or to the
  // button.
  function close() {
    if (pop.current?.contains(document.activeElement)) {
      (opener.current?.isConnected ? opener.current : button.current)?.focus();
    }
    setOpen(false);
  }

  function pickAgent(picked: Agent) {
    close();
    if (picked !== agent && !agentLocked) onAgent(picked);
  }

  const { name, effort } = describeChoice(models, choice);

  function pickModel(model: Model) {
    close();
    onChoose({ model, effort: effortFor(model, choice?.effort ?? null) });
  }

  // A place the list does not have picks nothing.
  function pickItem(from: PickerList, place: number) {
    if (from === "model") {
      const model = models?.at(place);
      if (model) pickModel(model);
      return;
    }
    const picked = choice?.model.efforts.at(place);
    if (!choice || picked === undefined) return;
    close();
    onChoose({ model: choice.model, effort: picked });
  }

  // Only keys pressed with focus in the picker. It marks the ones it takes, so nothing behind it
  // acts on them.
  function onKeyDown(event: KeyboardEvent) {
    if (!open || isInputMethodKey(event.nativeEvent)) return;
    if (event.key === "Escape") {
      event.preventDefault();
      close();
    } else if (list && matchesKeys(shortcut("pickItem").keys, event.nativeEvent)) {
      event.preventDefault();
      pickItem(list, Number(event.key) - 1);
    }
  }

  // Focus that goes to another place leaves the popover behind, so it closes. Two moves are not
  // that: focus that only drops, as when the window loses it, and focus that goes to something
  // around the picker, where WebKit puts it on a click on the picker's own button.
  function onBlur(event: FocusEvent) {
    const next = event.relatedTarget;
    if (!(next instanceof Node) || root.current!.contains(next) || next.contains(root.current)) return;
    setOpen(false);
  }

  useEffect(() => {
    if (!open) return;
    const outside = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", outside);
    return () => document.removeEventListener("mousedown", outside);
  }, [open]);

  // Runs again once the models are in, so a popover opened while they load lands on its list.
  const loaded = models !== null;
  useEffect(() => {
    if (!open || !list) return;
    const items = LIST_ITEMS[list];
    const current =
      pop.current!.querySelector<HTMLElement>(`${items}[aria-pressed=true]`) ??
      pop.current!.querySelector<HTMLElement>(items);
    (current ?? pop.current!).focus();
  }, [open, list, loaded]);

  return (
    <div className="picker" ref={root} onKeyDown={onKeyDown} onBlur={onBlur}>
      <button
        className="picker-btn"
        aria-label="Agent and model"
        aria-haspopup="dialog"
        aria-expanded={open}
        ref={button}
        onClick={() => {
          setEntering(!open && openedByPointer());
          setHeld(!open && models === null);
          opener.current = null;
          setList(null);
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
          ref={pop}
          tabIndex={-1}
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
              data-picker-list="model"
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
