import { useEffect, useEffectEvent, useId, useMemo, useRef, useState } from "react";

import { type Bridge, type RunSession, type TextProblem, textProblemOf } from "./bridge";
import { MonacoEditor } from "./MonacoEditor";
import { byRepo, folderName } from "./Sidebar";
import { Button } from "./ui/Button";

// The select's value for each choice. The prefix keeps a profile named "none" apart from None.
const NONE = "none";
const valueOf = (profile: string | null) => (profile === null ? NONE : `profile:${profile}`);

/**
 * The Profile panel of one worktree: the profile its repo uses, as its TOML file, with a picker to
 * use another, then the repo's overrides as TOML. Each saves through the core, which points at the
 * line of any error. A change to a profile or to the repo's settings, made anywhere, reloads both,
 * and unsaved edits stay, with a notice. Above them, the repo's run sessions, each with a Start
 * button.
 */
export function ProfilePanel({
  bridge,
  worktree,
  onStartRun,
}: {
  bridge: Bridge;
  worktree: string;
  /** Starts the run session of that name in this worktree. */
  onStartRun: (name: string) => void;
}) {
  const pickerId = useId();
  // The repo's main worktree. undefined until known.
  const [repo, setRepo] = useState<string | undefined>(undefined);
  const [names, setNames] = useState<string[]>([]);
  // The profile the repo uses, as grove last said. undefined until known.
  const [repoUses, setRepoUses] = useState<string | null | undefined>(undefined);
  // The profile the panel shows: the repo's, or the one it used while it has unsaved edits.
  const [profile, setProfile] = useState<string | null | undefined>(undefined);
  const [path, setPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Why the profiles could not be listed: a profile file that is not valid, by file and line.
  const [listError, setListError] = useState<string | null>(null);
  // Bumped on each ProfilesChanged and each change to the repo's settings, to load everything again.
  const [changes, setChanges] = useState(0);
  // The repo's run sessions as grove last listed them, and what they were listed for. A change
  // lists them again, and until then Start waits, since a name may now run another command.
  const runSessionsFor = JSON.stringify([repo, repoUses, changes]);
  const [runSessionsListed, setRunSessionsListed] = useState<{ for: string; runs: RunSession[] }>({
    for: "",
    runs: [],
  });
  const runSessions = runSessionsListed.runs;
  const runSessionsCurrent = runSessionsListed.for === runSessionsFor;

  useEffect(() => bridge.onProfilesChanged(() => setChanges((n) => n + 1)), [bridge]);
  useEffect(
    () => bridge.onRepoSettingsChanged((changed) => changed === repo && setChanges((n) => n + 1)),
    [bridge, repo],
  );

  useEffect(() => {
    let current = true;
    bridge.listWorktrees().then(
      (list) => {
        const main = byRepo(list).find((worktrees) => worktrees.some((w) => w.path === worktree))?.[0];
        if (current) setRepo(main?.path);
      },
      (e: unknown) => current && setError(String(e)),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree]);

  useEffect(() => {
    if (repo === undefined) return;
    let current = true;
    bridge.repoProfile(repo).then(
      (own) => current && setRepoUses(own),
      (e: unknown) => current && setError(String(e)),
    );
    // One file that is not valid fails the list, but the repo's own profile can still show.
    bridge.profileNames().then(
      (listed) => {
        if (!current) return;
        setNames(listed);
        setListError(null);
      },
      (e: unknown) => current && setListError(String(e)),
    );
    return () => {
      current = false;
    };
  }, [bridge, repo, changes]);

  useEffect(() => {
    if (repo === undefined) return;
    let current = true;
    bridge.runSessions(repo).then(
      (listed) => current && setRunSessionsListed({ for: runSessionsFor, runs: listed }),
      (e: unknown) => current && setError(String(e)),
    );
    return () => {
      current = false;
    };
  }, [bridge, repo, runSessionsFor]);

  const profileText = useTomlText(
    profile ?? null,
    changes,
    async () => {
      const file = await bridge.profileText(profile!);
      setPath(file.path);
      return file.text;
    },
    (text) => bridge.setProfileText(profile!, text),
    true,
  );
  // The repo's profile changed. The panel follows once the profile it shows has no unsaved edits.
  const moved = repoUses !== undefined && repoUses !== profile;
  if (moved && !profileText.edited) {
    setProfile(repoUses);
    setPath(null);
  }
  const overrides = useTomlText(
    repo ?? null,
    changes,
    () => bridge.repoOverridesText(repo!),
    (text) => bridge.setRepoOverridesText(repo!, text),
    false,
  );

  async function pick(value: string) {
    if (repo === undefined) return;
    const picked = value === NONE ? null : value.slice("profile:".length);
    try {
      await bridge.setRepoProfile(repo, picked);
      setRepoUses(picked);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  if (repo === undefined || profile === undefined) {
    return error ? <p role="alert">{error}</p> : null;
  }
  const choices = profile === null || names.includes(profile) ? names : [...names, profile];
  return (
    <div className="profile">
      <div className="profile-head">
        <b>{profile ?? "No profile"}</b>
        {profile !== null && path && <span>{path}</span>}
        <span>Used by {folderName(repo)}</span>
        <span className="spacer" />
        <label htmlFor={pickerId}>Profile</label>
        <select
          id={pickerId}
          value={valueOf(profile)}
          disabled={profileText.edited}
          title={profileText.edited ? "Save or discard your edits to the profile first." : undefined}
          onChange={(e) => void pick(e.target.value)}
        >
          {choices.map((name) => (
            <option key={name} value={valueOf(name)}>
              {name}
            </option>
          ))}
          <option value={NONE}>None</option>
        </select>
      </div>
      {[error, listError].map(
        (shown) =>
          shown && (
            <p key={shown} role="alert" className="ed-error">
              {shown}
            </p>
          ),
      )}
      <section className="profile-runs" aria-label="Run sessions">
        <div className="ed-path">
          <span>Run sessions</span>
          <span className="hint">Each starts in this worktree, with its output in the terminal panel.</span>
        </div>
        {runSessions.length === 0 ? (
          <p className="hint">No run sessions. Add a [[run_session]] table to the profile or the overrides.</p>
        ) : (
          <ul>
            {runSessions.map((run) => (
              <li key={run.name}>
                <b>{run.name}</b> <code>{run.command}</code>{" "}
                <Button
                  size="xs"
                  aria-label={`Start ${run.name}`}
                  disabled={!runSessionsCurrent}
                  onClick={() => onStartRun(run.name)}
                >
                  Start
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>
      {profile === null ? (
        <p className="panel-empty">grove uses no profile for this repo. Pick one to see and edit it.</p>
      ) : (
        <TomlSection
          title="Profile file"
          file="profile.toml"
          worktree={worktree}
          changedNotice="The profile changed on disk. Your edits are kept."
          notice={
            moved
              ? `This repo now uses ${repoUses ?? "no profile"}. Save or discard your edits to ${profile} to switch to it.`
              : null
          }
          toml={profileText}
        />
      )}
      <TomlSection
        title="Overrides"
        hint="Each value here wins over the profile's. Comments are not kept."
        file="overrides.toml"
        worktree={worktree}
        changedNotice="The overrides changed. Your edits are kept."
        toml={overrides}
      />
    </div>
  );
}

/** One TOML text the panel edits, as grove has it and as the user typed it. */
interface TomlState {
  /** What the text belongs to: a profile's name, or a repo. */
  owner: string | null;
  /** As grove last gave or took it. null until loaded. */
  saved: string | null;
  /** In the editor. */
  text: string;
  /** What grove has now, when it changed while the text had edits. */
  changedOnDisk: string | null;
  /** Why the last save was refused, or the last load failed. */
  problem: TextProblem | null;
}

type TomlText = TomlState & {
  edited: boolean;
  edit(text: string): void;
  save(): Promise<void>;
  discard(): void;
  loadDisk(): void;
  keepEdits(): void;
};

const fresh = (owner: string | null): TomlState => ({
  owner,
  saved: null,
  text: "",
  changedOnDisk: null,
  problem: null,
});

/**
 * Loads `owner`'s text, again on each bump of `changes`, and saves it. A new owner starts over.
 * Text grove gives back that the user has edited goes to `changedOnDisk` for the user to pick.
 * `keptAsTyped` says grove keeps a save byte for byte, so other text after a save is a change on
 * disk. Otherwise grove keeps it in its own form, and the text after a save is that form.
 */
function useTomlText(
  owner: string | null,
  changes: number,
  load: () => Promise<string>,
  store: (text: string) => Promise<void>,
  keptAsTyped: boolean,
): TomlText {
  const [state, setState] = useState(() => fresh(owner));
  // The text of the save in flight, so its own change coming back is not a change on disk.
  const saving = useRef<string | null>(null);
  // Only the answer to the latest load counts, since an older one can arrive after it.
  const loads = useRef(0);
  // The latest save. A load waits for it, and for any save started meanwhile, so the change a save
  // made, which grove can announce before the save returns, never reads as one made elsewhere.
  // It gives the text grove has after it, as far as the panel knows, or null if it does not know.
  const lastSave = useRef<Promise<string | null>>(Promise.resolve(null));
  const current = state.owner === owner ? state : fresh(owner);
  if (current !== state) setState(current);

  // Each answer applies only to the owner it was asked for.
  const loadNow = useEffectEvent(async () => {
    const ask = ++loads.current;
    const asked = owner;
    const apply = (change: (s: TomlState) => TomlState) =>
      ask === loads.current && setState((s) => (s.owner === asked ? change(s) : s));
    for (let waited = null; waited !== lastSave.current; ) {
      waited = lastSave.current;
      await waited;
    }
    try {
      const loaded = await load();
      apply((s) => took(s, loaded, saving.current));
    } catch (e) {
      apply((s) => ({ ...s, problem: { line: null, message: String(e) } }));
    }
  });

  useEffect(() => {
    if (owner !== null) void loadNow();
  }, [owner, changes]);

  const edited = current.saved !== null && current.text !== current.saved;
  return {
    ...current,
    edited,
    edit: (text) => setState((s) => ({ ...s, text })),
    save() {
      if (!edited) return Promise.resolve();
      const text = current.text;
      const apply = (change: (s: TomlState) => TomlState) =>
        setState((s) => (s.owner === owner ? change(s) : s));
      // A save writes only over the text the panel showed: what the save before it left, or with none
      // in flight, the saved text or the change the notice showed.
      const queued = saving.current !== null;
      const shown = current.changedOnDisk ?? current.saved;
      saving.current = text;
      // Saves run one at a time, so grove stores them in order and the last one made wins.
      const previous = lastSave.current;
      const next = (async () => {
        const left = await previous;
        const base = queued ? left : shown;
        try {
          const before = await load();
          if (before !== base && before !== text) {
            // Changed elsewhere since: keep the edits and show the change.
            apply((s) => ({ ...s, changedOnDisk: before }));
            return base;
          }
          await store(text);
          const loaded = await load();
          const kept = keptAsTyped ? text : loaded;
          apply((s) =>
            took({ ...s, saved: kept, text: s.text === text ? kept : s.text, changedOnDisk: null, problem: null }, loaded, null),
          );
          return kept;
        } catch (e) {
          apply((s) => ({ ...s, problem: textProblemOf(e) }));
          return base;
        } finally {
          // A later save may be in flight, with its own text.
          if (saving.current === text) saving.current = null;
        }
      })();
      lastSave.current = next;
      return next.then(() => {});
    },
    discard: () => setState((s) => ({ ...s, text: s.saved ?? "", changedOnDisk: null, problem: null })),
    loadDisk: () =>
      setState((s) => ({ ...s, saved: s.changedOnDisk, text: s.changedOnDisk ?? s.text, changedOnDisk: null, problem: null })),
    keepEdits: () => setState((s) => ({ ...s, saved: s.changedOnDisk, changedOnDisk: null })),
  };
}

// `state` once grove gave `loaded`: the save in flight coming back, nothing new, new text for a
// clean editor, or a change for the user to pick.
function took(state: TomlState, loaded: string, saving: string | null): TomlState {
  if (loaded === saving || loaded === state.saved) return { ...state, saved: loaded, changedOnDisk: null };
  if (state.saved !== null && state.text !== state.saved) return { ...state, changedOnDisk: loaded };
  return { ...state, saved: loaded, text: loaded, changedOnDisk: null, problem: null };
}

function TomlSection({
  title,
  hint,
  file,
  worktree,
  changedNotice,
  notice,
  toml,
}: {
  title: string;
  hint?: string;
  /** Something the section waits on, such as a profile switch. */
  notice?: string | null;
  /** Names the editor's model, which is never a file in the worktree. */
  file: string;
  worktree: string;
  changedNotice: string;
  toml: TomlText;
}) {
  const open = useMemo(() => [file], [file]);
  return (
    <section
      className="profile-section"
      aria-label={title}
      onKeyDown={(e) => {
        if (!e.metaKey || e.key.toLowerCase() !== "s") return;
        e.preventDefault();
        void toml.save();
      }}
    >
      <div className="ed-path">
        <span>{title}</span>
        {hint && <span className="hint">{hint}</span>}
        <span className="spacer" />
        {toml.edited && (
          <>
            <span>Unsaved</span>
            <Button size="xs" onClick={toml.discard}>
              Discard
            </Button>
          </>
        )}
        <Button size="xs" disabled={!toml.edited} onClick={() => void toml.save()}>
          Save
        </Button>
      </div>
      {notice && (
        <div className="ed-notice" role="alert">
          <span>{notice}</span>
        </div>
      )}
      {toml.changedOnDisk !== null && (
        <div className="ed-notice" role="alert">
          <span>{changedNotice}</span>
          <Button size="xs" onClick={toml.loadDisk}>
            Load the disk version
          </Button>
          <Button size="xs" onClick={toml.keepEdits}>
            Keep my edits
          </Button>
        </div>
      )}
      {toml.problem && (
        <p role="alert" className="ed-error">
          {toml.problem.line !== null && `Line ${toml.problem.line}: `}
          {toml.problem.message}
        </p>
      )}
      <div className="ed-body">
        {/* The "profile:" prefix keeps its model apart from the Files panel's for the worktree. */}
        <MonacoEditor
          worktree={`profile:${worktree}`}
          path={file}
          text={toml.text}
          open={open}
          onChange={toml.edit}
          lineChanges={NO_LINES}
          problemLine={toml.problem?.line}
        />
      </div>
    </section>
  );
}

const NO_LINES: never[] = [];
