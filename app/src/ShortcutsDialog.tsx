import { Dialog } from "./Dialog";
import { keyHint, SHORTCUTS } from "./shortcuts";
import { Button } from "./ui/Button";

/** Every shortcut in the list, under its group in the list's order. */
export function ShortcutsDialog({ onClose }: { onClose(): void }) {
  const groups = [...new Set(SHORTCUTS.map((shortcut) => shortcut.group))];
  return (
    <Dialog label="Keyboard shortcuts" onSubmit={onClose} onCancel={onClose}>
      <div className="shortcut-groups">
        {groups.map((group) => (
          <section key={group} aria-label={group}>
            <h4>{group}</h4>
            {SHORTCUTS.filter((shortcut) => shortcut.group === group).map((shortcut) => (
              <div key={shortcut.id} className="shortcut-row">
                <span>{shortcut.label}</span>
                <kbd>{keyHint(shortcut.keys)}</kbd>
              </div>
            ))}
          </section>
        ))}
      </div>
      <div className="dialog-actions">
        <Button type="submit" variant="primary" size="sm">
          Done
        </Button>
      </div>
    </Dialog>
  );
}
