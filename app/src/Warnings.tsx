import { useEffect, useState } from "react";

import type { Bridge, Warning, WarningSent } from "./bridge";
import { Button } from "./ui/Button";

/** The core's warnings, each in a bar until the user dismisses it or a retry fixes it. */
export function Warnings({ bridge }: { bridge: Bridge }) {
  // By id, oldest first. A dismissed one stays as null, so it does not come back.
  const [warnings, setWarnings] = useState<Record<number, WarningSent | null>>({});

  useEffect(
    () =>
      bridge.onWarning((warning) =>
        setWarnings((known) => (warning.id in known ? known : { ...known, [warning.id]: warning })),
      ),
    [bridge],
  );

  const dismiss = (id: number) => setWarnings((known) => ({ ...known, [id]: null }));

  async function retry(warning: WarningSent & { kind: "WatcherNotStarted" }) {
    try {
      await bridge.retryWatcher(warning.worktree);
      dismiss(warning.id);
    } catch (error) {
      // It stays, with the new cause, unless the user dismissed it meanwhile.
      setWarnings((known) =>
        known[warning.id] ? { ...known, [warning.id]: { ...warning, reason: String(error) } } : known,
      );
    }
  }

  return Object.values(warnings).map(
    (warning) =>
      warning && (
        <div key={warning.id} className="notice" role="status">
          <p>{message(warning)}</p>
          {warning.kind === "WatcherNotStarted" && (
            <Button size="xs" onClick={() => void retry(warning)}>
              Retry
            </Button>
          )}
          <Button size="xs" onClick={() => dismiss(warning.id)}>
            Dismiss
          </Button>
        </div>
      ),
  );
}

function message(warning: Warning) {
  switch (warning.kind) {
    case "LoginShellEnv":
      return `grove could not read your login shell's environment, so sessions may not find claude or codex: ${warning.reason}.`;
    case "CodexHooksNotTrusted":
      return `Codex is not running grove's hooks in ${warning.worktree}, so grove guesses the session's state. Approve the hooks in Codex's /hooks.`;
    case "SessionNotRestored":
      return `grove could not bring back a session in ${warning.worktree}, so it is gone: ${warning.reason}.`;
    case "WatcherNotStarted":
      return `grove could not watch ${warning.worktree}, so its diff does not update live: ${warning.reason}.`;
  }
}
