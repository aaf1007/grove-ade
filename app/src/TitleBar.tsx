import { NotebookText, PanelLeft } from "lucide-react";

import { DETAILS_CARD_ID } from "./DetailsCard";
import { PanelSwitches } from "./PanelSwitches";
import { IconButton } from "./ui/IconButton";

/**
 * The bar over the chat lane: the selected worktree's repo and name, and the switch that opens the
 * details card. While the panel area is closed, the terminal and panel area switches follow it.
 */
export function TitleBar({
  repo,
  worktree,
  sidebarOpen,
  panelsOpen,
  terminalOpen,
  onToggleSidebar,
  onTogglePanels,
  onToggleTerminal,
  detailsOpen,
  onToggleDetails,
}: {
  /** The selected worktree's repo name. null while none is selected. */
  repo: string | null;
  /** The selected worktree's display name. null while none is selected. */
  worktree: string | null;
  sidebarOpen: boolean;
  panelsOpen: boolean;
  /** Whether the selected worktree's terminal is open. null until a worktree's workspace is known. */
  terminalOpen: boolean | null;
  onToggleSidebar(): void;
  onTogglePanels(): void;
  onToggleTerminal(): void;
  /** Whether the details card is open. null while no worktree is selected. */
  detailsOpen: boolean | null;
  onToggleDetails(): void;
}) {
  return (
    <header className="titlebar">
      <IconButton icon={PanelLeft} label="Sidebar" aria-pressed={sidebarOpen} onClick={onToggleSidebar} />
      {repo && worktree ? (
        <div className="crumbs" title={`${repo} / ${worktree}`}>
          <span className="crumb-repo">
            <span className="repo-badge" aria-hidden="true">
              {repoBadge(repo)}
            </span>
            {repo}
          </span>
          <span className="crumb-sep">/</span>
          <b>{worktree}</b>
        </div>
      ) : (
        <div className="crumbs">No worktree selected</div>
      )}
      <div className="switches">
        <IconButton
          icon={NotebookText}
          label="Details"
          title="Where this worktree is, what runs in it and what changed"
          aria-pressed={detailsOpen ?? false}
          aria-controls={DETAILS_CARD_ID}
          disabled={detailsOpen === null}
          onClick={onToggleDetails}
        />
        {!panelsOpen && (
          <PanelSwitches
            terminalOpen={terminalOpen}
            panelsOpen={panelsOpen}
            onToggleTerminal={onToggleTerminal}
            onTogglePanels={onTogglePanels}
          />
        )}
      </div>
    </header>
  );
}

/** Two letters for a repo: the first letters of its first two words, or its first two letters. */
export function repoBadge(repo: string): string {
  const words = repo.split(/[-_.\s]+/).filter(Boolean);
  const letters = words.length > 1 ? words[0][0] + words[1][0] : repo.slice(0, 2);
  return letters.toUpperCase();
}
