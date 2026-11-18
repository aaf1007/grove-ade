import type { Bridge } from "./bridge";

/** A file open in the editor: what is on disk, and what the user has typed. */
export interface OpenFile {
  /** Relative to the worktree. */
  path: string;
  /** Not UTF-8, or has a NUL byte. The panel shows it as binary and never saves it. */
  binary: boolean;
  /** The text on disk, as last read or saved. null when what is on disk is not text. */
  saved: string | null;
  /** The text in the editor. */
  text: string;
  /** What is on disk now, when it changed while the file had edits. The user picks which to keep. */
  changedOnDisk: DiskVersion | null;
  /** Why the last read or save failed. */
  error: string | null;
}

/** A file as it is on disk: its text, or null when it is not text. */
export interface DiskVersion {
  text: string | null;
}

export const isEdited = (file: OpenFile) => !file.binary && file.text !== file.saved;

/**
 * The files open in one worktree's editor, in tab order. It knows nothing of the panel or of
 * Monaco, so a language client can follow the same open, edit, save and close later.
 */
export class OpenFiles {
  private files: readonly OpenFile[] = [];
  private readonly listeners = new Set<() => void>();
  // Files being read for `open`, so a double click reads once.
  private readonly opening = new Set<string>();
  // The text each write in flight writes, so a reload that reads it back sees no change on disk.
  private readonly saving = new Map<string, string>();
  // Each file's last write asked for. A save waits for the one before it, so an older text never
  // lands after a newer one.
  private readonly writes = new Map<string, Promise<void>>();
  // Bumped by each reload's read and each write of a file. A read applies only if nothing
  // started since, so a stale read never undoes a newer save. The write's own change on disk
  // brings a fresh reload.
  private readonly epochs = new Map<string, number>();

  constructor(
    private readonly bridge: Bridge,
    private readonly worktree: string,
  ) {}

  /** For `useSyncExternalStore`: a new array after every change. */
  readonly list = () => this.files;

  readonly subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  get(path: string): OpenFile | undefined {
    return this.files.find((file) => file.path === path);
  }

  /** Reads the file and adds it after the open ones. Nothing happens if it is open already. */
  async open(path: string): Promise<void> {
    if (this.get(path) || this.opening.has(path)) return;
    this.opening.add(path);
    try {
      const text = decodeText(await this.bridge.readFile(this.worktree, path));
      this.files = [...this.files, fresh(path, text, null)];
    } catch (e) {
      this.files = [...this.files, fresh(path, "", String(e))];
    } finally {
      this.opening.delete(path);
    }
    this.changed();
  }

  edit(path: string, text: string) {
    this.update(path, (file) => (file.text === text ? file : { ...file, text }));
  }

  /**
   * Writes the editor's text to disk. A save also settles a change on disk in favor of the edits.
   * A file that could not be read and has no edits is never written, so it is not emptied.
   */
  async save(path: string): Promise<void> {
    const file = this.get(path);
    if (!file || file.binary || (file.error !== null && !isEdited(file))) return;
    const { text } = file;
    const before = this.writes.get(path) ?? Promise.resolve();
    const write = before.catch(() => {}).then(() => this.write(path, text));
    this.writes.set(path, write);
    try {
      await write;
    } finally {
      if (this.writes.get(path) === write) this.writes.delete(path);
    }
  }

  /** Drops the file and its edits. The panel asks the user first. */
  close(path: string) {
    this.files = this.files.filter((file) => file.path !== path);
    this.changed();
  }

  /**
   * Reads every open file again. One without edits takes what is on disk. One with edits keeps
   * them, and holds what is on disk in `changedOnDisk` for the user to choose.
   */
  async reload(): Promise<void> {
    await Promise.all(this.files.map((file) => this.reloadOne(file.path)));
  }

  /** Replaces the edits with what is on disk. */
  takeDisk(path: string) {
    this.update(path, (file) => {
      const disk = file.changedOnDisk;
      return disk === null ? file : fresh(path, disk.text, null);
    });
  }

  /** Keeps the edits, so the next save writes them over what is on disk. */
  keepEdits(path: string) {
    this.update(path, (file) => {
      const disk = file.changedOnDisk;
      return disk === null ? file : { ...file, saved: disk.text, changedOnDisk: null };
    });
  }

  private async reloadOne(path: string) {
    const epoch = this.bump(path);
    let disk: string | null;
    try {
      disk = decodeText(await this.bridge.readFile(this.worktree, path));
    } catch (e) {
      const error = String(e);
      if (this.epochs.get(path) === epoch) this.update(path, (file) => ({ ...file, error }));
      return;
    }
    if (this.epochs.get(path) !== epoch) return;
    this.update(path, (file) => {
      const unchanged = file.error ? { ...file, error: null } : file;
      // Still what is known to be on disk: nothing is left to choose.
      if (disk === file.saved || (disk !== null && disk === this.saving.get(path))) {
        return file.changedOnDisk === null ? unchanged : { ...unchanged, changedOnDisk: null };
      }
      if (!isEdited(file)) return fresh(path, disk, null);
      if (disk === file.text) return { ...unchanged, saved: disk, changedOnDisk: null };
      return { ...unchanged, changedOnDisk: { text: disk } };
    });
  }

  private bump(path: string) {
    const epoch = (this.epochs.get(path) ?? 0) + 1;
    this.epochs.set(path, epoch);
    return epoch;
  }

  private async write(path: string, text: string) {
    this.saving.set(path, text);
    this.bump(path);
    try {
      await this.bridge.writeFile(this.worktree, path, text);
      this.bump(path);
      this.update(path, (f) => ({ ...f, saved: text, changedOnDisk: null, error: null }));
    } catch (e) {
      this.update(path, (f) => ({ ...f, error: String(e) }));
      throw e;
    } finally {
      this.saving.delete(path);
    }
  }

  private update(path: string, change: (file: OpenFile) => OpenFile) {
    let changed = false;
    this.files = this.files.map((file) => {
      if (file.path !== path) return file;
      const next = change(file);
      changed ||= next !== file;
      return next;
    });
    if (changed) this.changed();
  }

  private changed() {
    this.listeners.forEach((listener) => listener());
  }
}

/** A file as read: `text` null when it is not text. */
function fresh(path: string, text: string | null, error: string | null): OpenFile {
  return { path, binary: text === null, saved: text, text: text ?? "", changedOnDisk: null, error };
}

/**
 * The file's text, or null when it is not text: not UTF-8, or a NUL byte in its first 8000 bytes,
 * as git decides. A byte order mark stays, so a save writes it back.
 */
export function decodeText(bytes: Uint8Array): string | null {
  if (bytes.subarray(0, 8000).includes(0)) return null;
  try {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return null;
  }
}
