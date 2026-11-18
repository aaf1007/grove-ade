import { FileDiff, Files, Globe, type LucideIcon, Plus, ScrollText, SlidersHorizontal, X } from "lucide-react";

import type { PanelKind, Workspace } from "./bridge";
import "./PanelTabs.css";
import { IconButton } from "./ui/IconButton";
import { Menu } from "./ui/Menu";
import { MENU_PANELS, type MenuPanel } from "./workspace";

const ICONS: Record<PanelKind, LucideIcon> = {
  Files,
  Diff: FileDiff,
  Browser: Globe,
  Logs: ScrollText,
  Profile: SlidersHorizontal,
};

/** The tab's id. Its panel's id ends in `-panel` in place of `-tab`. */
export function tabId(tab: PanelKind, part: "tab" | "panel") {
  return `${tab.toLowerCase()}-${part}`;
}

/**
 * The panel area's tab strip: a tab per open panel, each with its close button, then the + menu.
 * While the menu is open, each panel's letter opens it, and Up and Down move through its items.
 */
export function PanelTabs({
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
    <div className="ui-panel-tabs" role="tablist" aria-label="Panels">
      {panels.map((tab) => {
        const Icon = ICONS[tab];
        return (
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
              <Icon aria-hidden="true" />
              {tab}
            </button>
            <IconButton icon={X} label={`Close ${tab}`} size="xs" onClick={() => onClose(tab)} />
          </span>
        );
      })}
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
    </div>
  );
}
