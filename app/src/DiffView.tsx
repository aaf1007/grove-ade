import { useEffect, useRef, useState } from "react";

import { fileLines, findLines } from "./anchor";
import type { AgentStateChanged, Bridge, DiffBase, FileDiff, FileVersions, SessionId } from "./bridge";
import { type DiffZone, MonacoDiff } from "./MonacoDiff";
import { CommentBox, describeLines, NewCommentBox, ReviewPicker, useComments } from "./Review";
import { Button } from "./ui/Button";
import { Segmented } from "./ui/Segmented";

/**
 * One worktree's changed files, beside a Monaco diff of the selected one with its review comments
 * under their lines. Clicking line numbers adds a comment, and Send review batch sends the open
 * ones to an agent session.
 */
export function DiffView({
  bridge,
  worktree,
  agentStates,
  onSent,
}: {
  bridge: Bridge;
  worktree: string;
  /** Each agent session's latest state, by session, for the review batch picker. */
  agentStates: Record<SessionId, AgentStateChanged>;
  /** Called with the agent session a review batch went to. */
  onSent(session: SessionId): void;
}) {
  const [base, setBase] = useState<DiffBase>("Head");
  // The files keep the base they came from, so their versions come from that same base.
  const [diff, setDiff] = useState<{ base: DiffBase; files: FileDiff[]; error?: string }>({
    base,
    files: [],
  });
  // Bumped on each DiffChanged for this worktree, to load the diff again.
  const [changes, setChanges] = useState(0);
  const [selected, setSelected] = useState<string | null>(null);
  // The last versions loaded. They stay on screen until the next ones arrive, so a refresh
  // never empties the editor.
  const [shown, setShown] = useState<Shown | null>(null);
  const { comments, error: commentsError } = useComments(bridge, worktree);
  // The comment being typed: the lines picked in the shown file, and their text then, which it
  // follows as the agent edits the file.
  const [composing, setComposing] = useState<Draft | null>(null);
  const drafts = useRef(0);
  const [picking, setPicking] = useState(false);
  const [resolveError, setResolveError] = useState<string | null>(null);

  useEffect(
    () =>
      bridge.onDiffChanged((changed) => {
        if (changed === worktree) setChanges((n) => n + 1);
      }),
    [bridge, worktree],
  );

  useEffect(() => {
    // A later request replaces this one, so this one drops its answer.
    let current = true;
    bridge.diff(worktree, base).then(
      (files) => current && setDiff({ base, files }),
      (e: unknown) => current && setDiff({ base, files: [], error: String(e) }),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, base, changes]);

  const file = diff.files.find((f) => f.newPath === selected) ?? diff.files[0];

  useEffect(() => {
    // git already calls it binary, so reading it would load the whole file only to hide it.
    if (!file || file.binary) return;
    let current = true;
    bridge.fileVersions(worktree, diff.base, file).then(
      (versions) => current && setShown({ path: file.newPath, versions }),
      (e: unknown) => current && setShown({ path: file.newPath, error: String(e) }),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, diff.base, file]);

  const openCount = comments.filter((c) => c.state === "Open").length;
  // The picker closes with the button, so the next open comment does not bring it back unasked.
  if (picking && openCount === 0) setPicking(false);
  // On files not in the diff, or that it shows only as binary.
  const unshown = comments.filter((c) => {
    const changed = diff.files.find((f) => f.newPath === c.path);
    return !changed || changed.binary || (shown?.path === c.path && "versions" in shown && shown.versions.binary);
  });
  // Where the comment being typed is now. null once the agent changed its lines.
  const placed =
    composing && shown?.path === composing.path && "versions" in shown ? placeDraft(composing, shown.versions) : null;

  // Fails only when the comment is already gone, which the next load shows.
  function resolve(id: number) {
    bridge.resolveComment(id).then(
      () => setResolveError(null),
      (e: unknown) => setResolveError(String(e)),
    );
  }

  function startComment(startLine: number, endLine: number) {
    if (!shown || !("versions" in shown)) return;
    const text = fileLines(shown.versions.new ?? "").slice(startLine - 1, endLine).join("\n");
    drafts.current += 1;
    setComposing({ draft: drafts.current, path: shown.path, startLine, endLine, text });
  }

  async function addComment(body: string) {
    if (!composing || !placed) return;
    await bridge.addComment(worktree, { path: composing.path, ...placed, body });
    setComposing(null);
  }

  // The shown file's comments under their last line, and the box for a new one under its lines.
  function zonesOf(path: string): DiffZone[] {
    const zones: DiffZone[] = comments
      .filter((c) => c.path === path)
      .map((c) => ({
        key: `comment ${c.id}`,
        afterLine: c.endLine,
        content: (
          <CommentBox
            comment={c}
            label={`Comment on ${describeLines(c.startLine, c.endLine)}`}
            onResolve={() => resolve(c.id)}
          />
        ),
      }));
    if (composing?.path === path) {
      const { startLine, endLine } = placed ?? composing;
      zones.push({
        // The draft's own, so its box keeps what was typed as its lines move.
        key: `new comment ${composing.draft}`,
        afterLine: endLine,
        content: (
          <NewCommentBox
            startLine={startLine}
            endLine={endLine}
            linesChanged={placed === null}
            onAdd={addComment}
            onCancel={() => setComposing(null)}
          />
        ),
      });
    }
    return zones;
  }

  return (
    <section className="diff-view">
      <div className="toolbar">
        <Segmented<DiffBase> label="Diff base" options={bases} value={base} onChange={setBase} />
        {openCount > 0 && (
          <div className="review">
            <Button variant="primary" size="xs" onClick={() => setPicking(!picking)}>
              Send review batch ({openCount})
            </Button>
            {picking && (
              <ReviewPicker
                bridge={bridge}
                worktree={worktree}
                count={openCount}
                agentStates={agentStates}
                onClose={() => setPicking(false)}
                onSent={onSent}
              />
            )}
          </div>
        )}
      </div>
      <ul aria-label="Changed files">
        {diff.files.map((f) => (
          <li key={f.newPath}>
            <button
              aria-current={f === file}
              title={f.newPath}
              onClick={() => setSelected(f.newPath)}
            >
              {f.newPath}{" "}
              {f.binary ? (
                <span>binary</span>
              ) : (
                <>
                  <span className="added">+{f.added}</span>{" "}
                  <span className="removed">-{f.removed}</span>
                </>
              )}
            </button>
          </li>
        ))}
      </ul>
      {unshown.length > 0 && (
        // Not a ul, which would take the file list's styles.
        <div className="elsewhere" role="list" aria-label="Comments the diff cannot show">
          {unshown.map((c) => (
            <div key={c.id} role="listitem">
              <CommentBox
                comment={c}
                label={`Comment on ${c.path} ${describeLines(c.startLine, c.endLine)}`}
                onResolve={() => resolve(c.id)}
              />
            </div>
          ))}
        </div>
      )}
      <div className="file">
        {diff.error && <p role="alert">{diff.error}</p>}
        {(commentsError ?? resolveError) && <p role="alert">{commentsError ?? resolveError}</p>}
        {file &&
          (file.binary
            ? binaryNotice
            : shown && (
                <Versions
                  shown={shown}
                  zones={zonesOf(shown.path)}
                  onCommentLines={startComment}
                />
              ))}
      </div>
    </section>
  );
}

type Shown = { path: string } & ({ versions: FileVersions } | { error: string });

/** A comment being typed. `draft` tells it from the one before, so a new pick starts afresh. */
interface Draft {
  draft: number;
  path: string;
  startLine: number;
  endLine: number;
  /** The picked lines when they were picked. */
  text: string;
}

/** The draft's lines in `versions`: where its text is now, nearest where it was. null when the text is gone. */
function placeDraft(draft: Draft, versions: FileVersions): { startLine: number; endLine: number } | null {
  if (versions.binary) return null;
  const startLine = findLines(fileLines(versions.new ?? ""), draft.text, draft.startLine);
  return startLine === null ? null : { startLine, endLine: startLine + draft.endLine - draft.startLine };
}

function Versions({
  shown,
  zones,
  onCommentLines,
}: {
  shown: Shown;
  zones: DiffZone[];
  onCommentLines(startLine: number, endLine: number): void;
}) {
  if ("error" in shown) return <p role="alert">{shown.error}</p>;
  if (shown.versions.binary) return binaryNotice;
  return (
    <MonacoDiff
      original={visibleCarriageReturns(shown.versions.old)}
      modified={visibleCarriageReturns(shown.versions.new)}
      path={shown.path}
      onCommentLines={onCommentLines}
      zones={zones}
    />
  );
}

const binaryNotice = <p>Binary file, not shown.</p>;

// A Monaco model keeps one line ending for the whole file and drops each \r, so a change of line
// endings alone would show no diff.
function visibleCarriageReturns(text: string | null): string {
  return (text ?? "").replaceAll("\r", "␍");
}

const bases: [DiffBase, string][] = [
  ["Head", "HEAD"],
  ["MergeBase", "Merge base"],
];
