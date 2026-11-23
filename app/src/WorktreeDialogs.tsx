import { useEffect, useState } from "react";

import type { BranchOutcome, Bridge, DeleteCheck, ProfileSuggestion, Worktree } from "./bridge";
import { Actions, Dialog } from "./Dialog";

// The select's value for each choice. The prefix keeps a profile named "none" apart from None.
const UNPICKED = "";
const NONE = "none";
const valueOf = (profile: string | null | undefined) =>
  profile === undefined ? UNPICKED : profile === null ? NONE : `profile:${profile}`;

/**
 * Adds a worktree to the repo whose main worktree is `repo`. The profile starts as the repo's
 * own, else the one grove suggests, else unpicked. Creating sets the repo's profile to the pick.
 */
export function NewWorktreeDialog({ bridge, repo, onCreated, onCancel }: {
  bridge: Bridge;
  repo: Worktree;
  onCreated(worktree: Worktree, startAgent: boolean): void;
  onCancel(): void;
}) {
  const [branch, setBranch] = useState("");
  const [startAgent, setStartAgent] = useState(true);
  const [base, setBase] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [issueLink, setIssueLink] = useState("");
  const [names, setNames] = useState<string[]>([]);
  const [suggestion, setSuggestion] = useState<ProfileSuggestion | null>(null);
  // The repo's profile when the dialog opened. undefined until known.
  const [current, setCurrent] = useState<string | null | undefined>(undefined);
  // undefined until the repo or grove picks one, or the user does. null is None.
  const [profile, setProfile] = useState<string | null | undefined>(undefined);
  const [branchProblem, setBranchProblem] = useState<string | null>(null);
  const [profileProblem, setProfileProblem] = useState<string | null>(null);
  const [profilesError, setProfilesError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let open = true;
    // A profile file that is not valid fails the list. The user can still pick None.
    const listed = bridge.profileNames().catch((e: unknown) => {
      if (open) setProfilesError(String(e));
      return [];
    });
    Promise.all([listed, bridge.suggestProfile(repo.path), bridge.repoProfile(repo.path)]).then(
      ([names, suggested, own]) => {
        if (!open) return;
        setNames(names);
        setSuggestion(suggested);
        setCurrent(own);
        setProfile((picked) => (picked !== undefined ? picked : (own ?? suggested?.profile)));
      },
      (e: unknown) => open && setError(String(e)),
    );
    return () => {
      open = false;
    };
  }, [bridge, repo.path]);

  async function create() {
    const name = branch.trim();
    setBranchProblem(name ? null : "Type a branch name.");
    setProfileProblem(profile === undefined ? "Pick a profile, or None." : null);
    if (!name || profile === undefined || current === undefined) return;
    setBusy(true);
    let profileSet = false;
    try {
      // The profile is set first, since a new worktree's setup comes from it.
      if (profile !== current) {
        await bridge.setRepoProfile(repo.path, profile);
        profileSet = true;
      }
      const created = await bridge.createWorktree(repo.path, {
        branch: name,
        displayName: displayName.trim() || null,
        issueLink: issueLink.trim() || null,
        baseBranch: base.trim() || null,
      });
      setCurrent(profile);
      onCreated(created, startAgent);
    } catch (e) {
      let message = String(e);
      // No worktree was made, so the repo goes back to the profile it had.
      if (profileSet) {
        await bridge.setRepoProfile(repo.path, current).catch((undo: unknown) => {
          message += ` The repo still uses ${profile ?? "no profile"}: ${String(undo)}`;
        });
      }
      setError(message);
    } finally {
      setBusy(false);
    }
  }

  // The pick stays listed when the list failed or no longer has it.
  const choices = profile && !names.includes(profile) ? [...names, profile] : names;
  return (
    <Dialog label="New worktree" onSubmit={create} onCancel={onCancel}>
      <p className="sub">One worktree per task. grove creates the branch if it does not exist.</p>
      <div className="form">
        <div className="field">
          <label htmlFor="nw-branch">Branch name</label>
          <input
            id="nw-branch"
            placeholder="feat-113-file-tree"
            autoComplete="off"
            value={branch}
            onChange={(event) => setBranch(event.target.value)}
          />
          {branchProblem && <span className="error">{branchProblem}</span>}
        </div>
        <div className="row2">
          <div className="field">
            <label htmlFor="nw-base">Base branch</label>
            <input
              id="nw-base"
              placeholder={repo.baseBranch ?? "The repo's default branch"}
              autoComplete="off"
              value={base}
              onChange={(event) => setBase(event.target.value)}
            />
          </div>
          <div className="field">
            <label htmlFor="nw-name">Display name</label>
            <input
              id="nw-name"
              placeholder="Defaults to the branch"
              value={displayName}
              onChange={(event) => setDisplayName(event.target.value)}
            />
          </div>
        </div>
        <div className="field">
          <label htmlFor="nw-issue">Issue link</label>
          <input id="nw-issue" placeholder="#52" value={issueLink} onChange={(event) => setIssueLink(event.target.value)} />
        </div>
        <div className="field">
          <label htmlFor="nw-profile">Profile</label>
          <select
            id="nw-profile"
            value={valueOf(profile)}
            onChange={(event) => {
              const value = event.target.value;
              setProfile(value === NONE ? null : value.slice("profile:".length));
              setProfileProblem(null);
            }}
          >
            {profile === undefined && (
              <option value={UNPICKED} disabled>
                Pick a profile
              </option>
            )}
            {choices.map((name) => (
              <option key={name} value={valueOf(name)}>
                {name}
              </option>
            ))}
            <option value={NONE}>None</option>
          </select>
          {current !== undefined &&
            (suggestion ? (
              <span className="help suggest">
                Suggested: {suggestion.profile}, because the repo has {suggestion.marker}.
              </span>
            ) : (
              <span className="help">No profile matches this repo's files. Pick one, or None.</span>
            ))}
          {profilesError && <span className="error">{profilesError}</span>}
          {profileProblem && <span className="error">{profileProblem}</span>}
        </div>
        <div className="check-row">
          <input
            type="checkbox"
            id="nw-agent"
            checked={startAgent}
            onChange={(event) => setStartAgent(event.target.checked)}
          />
          <label htmlFor="nw-agent">Start Claude Code</label>
        </div>
        {error && <p role="alert" className="error">{error}</p>}
      </div>
      <Actions submit="Create worktree" busy={busy} onCancel={onCancel} />
    </Dialog>
  );
}

/** Changes a worktree's base branch, which its whole-branch diff compares against. */
export function ChangeBaseDialog({ bridge, worktree, onSaved, onCancel }: {
  bridge: Bridge;
  worktree: Worktree;
  onSaved(): void;
  onCancel(): void;
}) {
  const [base, setBase] = useState(worktree.baseBranch ?? "");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function save() {
    const name = base.trim();
    if (!name) return setError("Type a branch name.");
    setBusy(true);
    try {
      await bridge.setBaseBranch(worktree.path, name);
      onSaved();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog label="Change base branch" onSubmit={save} onCancel={onCancel}>
      <p className="sub">The whole-branch diff of {worktree.displayName} compares against it.</p>
      <div className="form">
        <div className="field">
          <label htmlFor="cb-base">Base branch</label>
          <input id="cb-base" autoComplete="off" value={base} onChange={(event) => setBase(event.target.value)} />
        </div>
        {error && <p role="alert" className="error">{error}</p>}
      </div>
      <Actions submit="Save" busy={busy} onCancel={onCancel} />
    </Dialog>
  );
}

/** "sessions" when the worktree has none listed, else how many: "1 session", "3 sessions". */
function counted(sessionsInWorktree: number): string {
  if (sessionsInWorktree === 0) return "sessions";
  return sessionsInWorktree === 1 ? "1 session" : `${sessionsInWorktree} sessions`;
}

/** Stops tracking a worktree. Its folder and branch stay on disk. */
export function ForgetDialog({ bridge, worktree, sessionsInWorktree, onForgotten, onCancel }: {
  bridge: Bridge;
  worktree: Worktree;
  /** How many agent sessions go with the worktree. The dialog names the number. */
  sessionsInWorktree: number;
  onForgotten(): void;
  onCancel(): void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function forget() {
    setBusy(true);
    try {
      await bridge.forgetWorktree(worktree.path);
      onForgotten();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog label="Forget worktree" onSubmit={forget} onCancel={onCancel}>
      <div className="form">
        <p>
          grove stops tracking {worktree.displayName} and ends its {counted(sessionsInWorktree)}. Its folder
          and branch stay on disk.
        </p>
        {error && <p role="alert" className="error">{error}</p>}
      </div>
      <Actions submit="Forget" busy={busy} onCancel={onCancel} />
    </Dialog>
  );
}

/**
 * Deletes a worktree. The core's check decides what the dialog asks: the worktree's name typed
 * when deleting loses work, and a box to delete the branch too when it is merged.
 */
export function DeleteDialog({ bridge, worktree, sessionsInWorktree, onDeleting, onDeleted, onCancel }: {
  bridge: Bridge;
  worktree: Worktree;
  /** How many agent sessions go with the worktree. The dialog names the number. */
  sessionsInWorktree: number;
  /** The delete started or ended. It keeps running after a Cancel. */
  onDeleting(running: boolean): void;
  onDeleted(outcome: BranchOutcome): void;
  onCancel(): void;
}) {
  const [check, setCheck] = useState<DeleteCheck | null>(null);
  // Each failed delete checks the worktree again, since it changed after the last check.
  const [checks, setChecks] = useState(0);
  const [typed, setTyped] = useState("");
  const [deleteBranch, setDeleteBranch] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let open = true;
    bridge.deleteCheck(worktree.path).then(
      (found) => open && setCheck(found),
      (e: unknown) => open && setError(String(e)),
    );
    return () => {
      open = false;
    };
  }, [bridge, worktree.path, checks]);

  const losesWork = check !== null && (check.uncommitted || check.unpushed > 0);

  async function remove() {
    if (!check || (losesWork && typed !== worktree.displayName)) return;
    setBusy(true);
    onDeleting(true);
    try {
      onDeleted(await bridge.deleteWorktree(worktree.path, losesWork, deleteBranch && check.merged));
    } catch (e) {
      setError(String(e));
      setChecks((n) => n + 1);
    } finally {
      setBusy(false);
      onDeleting(false);
    }
  }

  return (
    <Dialog label="Delete worktree" onSubmit={remove} onCancel={onCancel}>
      <div className="form">
        <p>
          grove ends the {counted(sessionsInWorktree)} of {worktree.displayName} and removes its folder.
        </p>
        {!check && !error && <p className="sub">Checking for unsaved work…</p>}
        {losesWork && (
          <div className="field">
            <p>It has {unsaved(check)}. Deleting loses them.</p>
            <label htmlFor="dw-name">Type {worktree.displayName} to confirm</label>
            <input
              id="dw-name"
              autoComplete="off"
              autoFocus
              value={typed}
              onChange={(event) => setTyped(event.target.value)}
            />
          </div>
        )}
        {check?.branch &&
          (check.merged ? (
            <div className="check-row">
              <input
                id="dw-branch"
                type="checkbox"
                checked={deleteBranch}
                onChange={(event) => setDeleteBranch(event.target.checked)}
              />
              <label htmlFor="dw-branch">
                Also delete branch {check.branch}, merged into {worktree.baseBranch}
              </label>
            </div>
          ) : (
            <p className="sub">
              Branch {check.branch} stays.
              {worktree.baseBranch && ` It is not merged into ${worktree.baseBranch}.`}
            </p>
          ))}
        {error && <p role="alert" className="error">{error}</p>}
      </div>
      <Actions
        submit={check && "Delete"}
        busy={busy}
        disabled={losesWork && typed !== worktree.displayName}
        onCancel={onCancel}
      />
    </Dialog>
  );
}

// What deleting would lose, as the core says it: "uncommitted changes and 2 commits on no remote".
function unsaved({ uncommitted, unpushed }: DeleteCheck): string {
  const commits = unpushed === 0 ? [] : [unpushed === 1 ? "1 commit on no remote" : `${unpushed} commits on no remote`];
  return [...(uncommitted ? ["uncommitted changes"] : []), ...commits].join(" and ");
}
