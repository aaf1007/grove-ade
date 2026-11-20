import { PanelBottom, PanelRight } from "lucide-react";

import "./PanelTabs.css";
import { IconButton } from "./ui/IconButton";

/**
 * The switches for the terminal and the panel area. They end the panel area's tab row while it is
 * open, and the title bar while it is closed.
 */
export function PanelSwitches({
  terminalOpen,
  panelsOpen,
  onToggleTerminal,
  onTogglePanels,
}: {
  /** Whether the selected worktree's terminal is open. null until a worktree's workspace is known. */
  terminalOpen: boolean | null;
  panelsOpen: boolean;
  onToggleTerminal(): void;
  onTogglePanels(): void;
}) {
  return (
    <div className="panel-switches">
      <IconButton
        icon={PanelBottom}
        label="Terminal"
        title="Terminal at the bottom"
        aria-pressed={terminalOpen ?? false}
        disabled={terminalOpen === null}
        onClick={onToggleTerminal}
      />
      <IconButton icon={PanelRight} label="Panel area" aria-pressed={panelsOpen} onClick={onTogglePanels} />
    </div>
  );
}
