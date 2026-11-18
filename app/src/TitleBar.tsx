import { Info, PanelBottom, PanelLeft, PanelRight, SunMoon } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { DETAILS_CARD_ID } from "./DetailsCard";
import { folderName } from "./Sidebar";
import { Button } from "./ui/Button";
import { IconButton } from "./ui/IconButton";
import { Segmented } from "./ui/Segmented";
import { type Appearance, PALETTES, THEMES } from "./theme";

/** The key that focuses the head of the attention queue, as `isJumpKey` in App.tsx reads it. */
const JUMP_KEY_HINT = "⌘J";

/**
 * The slim bar above the regions: the selected worktree, the attention queue's count, and the
 * buttons that shape the window.
 */
export function TitleBar({
  worktree,
  waiting,
  onJump,
  sidebarOpen,
  panelsOpen,
  terminalOpen,
  onToggleSidebar,
  onTogglePanels,
  onToggleTerminal,
  appearance,
  onAppearance,
  detailsOpen,
  onToggleDetails,
}: {
  worktree: string | null;
  /** How many agent sessions are in the attention queue. */
  waiting: number;
  /** Focuses the head of the attention queue, as the key does. */
  onJump(): void;
  sidebarOpen: boolean;
  panelsOpen: boolean;
  /** Whether the selected worktree's terminal is open. null until a worktree's workspace is known. */
  terminalOpen: boolean | null;
  onToggleSidebar(): void;
  onTogglePanels(): void;
  onToggleTerminal(): void;
  appearance: Appearance;
  onAppearance(update: (appearance: Appearance) => Appearance): void;
  /** Whether the Details card is open. null while no worktree is selected. */
  detailsOpen: boolean | null;
  onToggleDetails(): void;
}) {
  return (
    <header className="titlebar">
      <IconButton icon={PanelLeft} label="Sidebar" aria-pressed={sidebarOpen} onClick={onToggleSidebar} />
      <div className="crumbs" title={worktree ?? undefined}>
        {worktree ? <b>{folderName(worktree)}</b> : <span>No worktree selected</span>}
      </div>
      <div className="spacer" />
      <Button
        variant="ghost"
        size="xs"
        className={waiting > 0 ? "attn waiting" : "attn"}
        title="Focus the head of the attention queue"
        onClick={onJump}
      >
        {waiting > 0 ? `${waiting} waiting` : "Nothing waiting"}
        <kbd>{JUMP_KEY_HINT}</kbd>
      </Button>
      <AppearanceMenu appearance={appearance} onAppearance={onAppearance} />
      <IconButton
        icon={Info}
        label="Details"
        title="Where this worktree is, what runs in it and what changed"
        aria-expanded={detailsOpen ?? false}
        aria-controls={DETAILS_CARD_ID}
        disabled={detailsOpen === null}
        onClick={onToggleDetails}
      />
      <IconButton
        icon={PanelBottom}
        label="Terminal"
        title="Terminal at the bottom"
        aria-pressed={terminalOpen ?? false}
        disabled={terminalOpen === null}
        onClick={onToggleTerminal}
      />
      <IconButton icon={PanelRight} label="Panel area" aria-pressed={panelsOpen} onClick={onTogglePanels} />
    </header>
  );
}

function AppearanceMenu({
  appearance,
  onAppearance,
}: {
  appearance: Appearance;
  onAppearance(update: (appearance: Appearance) => Appearance): void;
}) {
  const [open, setOpen] = useState(false);
  const menu = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);

  // A click outside or Escape closes it. When focus is in the menu, Escape puts it back on the
  // button, since the focused option goes away.
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!menu.current?.contains(event.target as Node)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (menu.current?.contains(document.activeElement)) button.current?.focus();
      setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  return (
    <div className="appearance" ref={menu}>
      <IconButton
        ref={button}
        icon={SunMoon}
        label="Appearance"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      />
      {open && (
        <div className="pop">
          <h4>Palette</h4>
          <Segmented
            label="Palette"
            options={PALETTES}
            value={appearance.palette}
            onChange={(palette) => onAppearance((a) => ({ ...a, palette }))}
          />
          <h4>Theme</h4>
          <Segmented
            label="Theme"
            options={THEMES}
            value={appearance.theme}
            onChange={(theme) => onAppearance((a) => ({ ...a, theme }))}
          />
        </div>
      )}
    </div>
  );
}
