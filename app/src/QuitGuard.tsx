import { useEffect, useEffectEvent, useState } from "react";

import type { AgentStateChanged, Bridge, SessionId } from "./bridge";
import { Actions, Dialog } from "./Dialog";

/**
 * Answers Cmd-Q. Quitting ends every session and waits for a delete that runs, so with an agent
 * session `Working` or `NeedsInput`, or a worktree being deleted, it asks first. With neither it
 * quits at once.
 */
export function QuitGuard({ bridge, agentStates, deleting }: {
  bridge: Bridge;
  /** Each agent session's latest state, by session. */
  agentStates: Record<SessionId, AgentStateChanged>;
  /** The display name of each worktree being deleted. */
  deleting: string[];
}) {
  const [asking, setAsking] = useState(false);
  const busy = Object.values(agentStates).filter(
    (change) => change.state === "Working" || change.state === "NeedsInput",
  ).length;

  // It fails only once grove is quitting, so there is nothing to report.
  function quit() {
    setAsking(false);
    bridge.quit().catch(() => {});
  }

  const onQuitRequested = useEffectEvent(() =>
    busy === 0 && deleting.length === 0 ? quit() : setAsking(true),
  );

  useEffect(() => bridge.onQuitRequested(onQuitRequested), [bridge]);

  if (!asking) return null;
  return (
    <Dialog label="Quit grove?" role="alertdialog" onSubmit={quit} onCancel={() => setAsking(false)}>
      {busy > 0 && (
        <p className="sub">
          {busy === 1
            ? "1 agent session is working or waiting on you. Quitting ends it."
            : `${busy} agent sessions are working or waiting on you. Quitting ends them.`}
        </p>
      )}
      {deleting.length > 0 && (
        <p className="sub">
          {deleting.length === 1
            ? `grove is still deleting ${deleting[0]}. Quitting waits until it is done.`
            : `grove is still deleting ${deleting.length} worktrees. Quitting waits until they are done.`}
        </p>
      )}
      <Actions submit="Quit" onCancel={() => setAsking(false)} />
    </Dialog>
  );
}
