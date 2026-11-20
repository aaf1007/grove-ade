import { Plus, X } from "lucide-react";

import type { PanelKind, Workspace } from "./bridge";
import "./PanelTabs.css";
import { PanelSwitches } from "./PanelSwitches";
import { IconButton } from "./ui/IconButton";
import { Menu } from "./ui/Menu";
import { MENU_PANELS, type MenuPanel } from "./workspace";

/** The tab's id. Its panel's id ends in `-panel` in place of `-tab`. */
export function tabId(tab: PanelKind, part: "tab" | "panel") {
  return `${tab.toLowerCase()}-${part}`;
}

/**
 * The panel area's top row: a tab per open panel, each with its close button, then the + menu and
 * the terminal and panel area switches. While the menu is open, each panel's letter opens it, and
 * Up and Down move through its items. With no worktree selected, the row holds only the switches.
 * It sits in the window's top row, so its empty part drags the window.
 */
export function PanelTabs({
  workspace,
  onSelect,
  onClose,
  onOpen,
  onToggleTerminal,
  onTogglePanels,
}: {
  /** The selected worktree's workspace. null while none is selected. */
  workspace: Workspace | null;
  onSelect(tab: PanelKind): void;
  onClose(tab: PanelKind): void;
  onOpen(panel: MenuPanel): void;
  onToggleTerminal(): void;
  onTogglePanels(): void;
}) {
  return (
    <div className="ui-panel-tabs" data-tauri-drag-region="deep">
      {workspace && <Tabs workspace={workspace} onSelect={onSelect} onClose={onClose} onOpen={onOpen} />}
      <PanelSwitches
        terminalOpen={workspace?.terminal ?? null}
        panelsOpen
        onToggleTerminal={onToggleTerminal}
        onTogglePanels={onTogglePanels}
      />
    </div>
  );
}

function Tabs({
  workspace,
  onSelect,
  onClose,
  onOpen,
}: {
  workspace: Workspace;
  onSelect(tab: PanelKind): void;
  onClose(tab: PanelKind): void;
  onOpen(panel: MenuPanel): void;
}) {
  const { panels, selected } = workspace;
  return (
    <>
      <div className="ui-panel-tablist" role="tablist" aria-label="Panels">
        {panels.map((tab) => (
          <span key={tab} className="ui-panel-tab" role="presentation">
            <button
              type="button"
              role="tab"
              aria-selected={selected === tab}
              aria-controls={tabId(tab, "panel")}
              id={tabId(tab, "tab")}
              tabIndex={selected === tab ? 0 : -1}
              onClick={() => onSelect(tab)}
              onKeyDown={(event) => {
                const step = { ArrowRight: 1, ArrowLeft: -1 }[event.key];
                if (step === undefined) return;
                event.preventDefault();
                const next = panels[(panels.indexOf(tab) + step + panels.length) % panels.length];
                onSelect(next);
                document.getElementById(tabId(next, "tab"))?.focus();
              }}
            >
              {tab}
            </button>
            <IconButton icon={X} label={`Close ${tab}`} size="xs" onClick={() => onClose(tab)} />
          </span>
        ))}
      </div>
      <Menu
        label="Open a panel"
        icon={Plus}
        heading="Open a panel"
        items={MENU_PANELS.map(({ panel, letter }) => ({
          key: panel,
          label: panel,
          hint: panel === "Terminal" ? "Opens at the bottom" : undefined,
          checked: panel === "Terminal" ? workspace.terminal : panels.includes(panel),
          shortcut: letter,
          onSelect: () => onOpen(panel),
        }))}
      />
    </>
  );
}
