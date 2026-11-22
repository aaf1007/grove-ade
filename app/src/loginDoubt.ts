import { useEffect, useEffectEvent, useRef, useState } from "react";

import type { Agent, AgentStateChanged, Bridge, SessionId } from "./bridge";

/**
 * Which agents' logins are in doubt. An agent reports a login it has a token for even when the
 * token no longer works, so its report alone cannot be trusted after a turn fails.
 *
 * A login falls in doubt when a panel says a turn of the agent failed to log in, or when a turn
 * fails where no panel follows it (`follows`), since then the window cannot see why. Only a turn
 * of the agent that completes, or a login report that differs from the last one, clears it.
 * Events count in the order they came, though finding a session's agent takes a call to the core.
 */
export function useLoginDoubt(bridge: Bridge, follows: (session: SessionId) => boolean) {
  const [doubted, setDoubted] = useState<Agent[]>([]);
  // Each panel report counted, as "session turn": a panel reports again each time it is shown.
  const counted = useRef(new Set<string>());
  const queue = useRef<Promise<unknown>>(Promise.resolve());

  function inOrder(step: () => unknown) {
    queue.current = queue.current.then(step).catch(() => {});
  }
  const doubt = (agent: Agent) => setDoubted((known) => (known.includes(agent) ? known : [...known, agent]));
  const trust = (agent: Agent) => setDoubted((known) => known.filter((other) => other !== agent));

  const onState = useEffectEvent((change: AgentStateChanged) => {
    // A heuristic can take a screen for a finished or failed turn when it is not one.
    if (change.source === "Heuristic") return;
    const completed = change.state === "Done";
    const failedUnseen = change.state === "Failed" && !follows(change.session);
    if (!completed && !failedUnseen) return;
    inOrder(async () => {
      const agent = (await bridge.agentSessions()).find((record) => record.session === change.session)?.agent;
      if (agent) (completed ? trust : doubt)(agent);
    });
  });

  useEffect(() => bridge.onAgentState(onState), [bridge]);

  useEffect(() => {
    const reported = new Map<Agent, string>();
    return bridge.onAccount(({ agent, account }) => {
      const before = reported.get(agent);
      const now = JSON.stringify(account);
      reported.set(agent, now);
      if (before !== undefined && before !== now) inOrder(() => trust(agent));
    });
  }, [bridge]);

  /** A panel says turn `turn` of `session`, which runs `agent`, failed to log in. */
  function loginFailed(session: SessionId, agent: Agent, turn: number) {
    const key = `${session} ${turn}`;
    if (counted.current.has(key)) return;
    counted.current.add(key);
    inOrder(() => doubt(agent));
  }

  return { doubted, loginFailed };
}
