import { ChevronDown, ChevronRight } from "lucide-react";
import { useEffect, useEffectEvent, useRef, useState } from "react";

import { fileLines, findLines } from "./anchor";
import type { AgentStateChanged, Bridge, DiffBase, FileDiff, FileVersions, SessionId } from "./bridge";
import { type DiffZone, MonacoDiff } from "./MonacoDiff";
import { CommentBox, describeLines, NewCommentBox, ReviewPicker, useComments } from "./Review";
import { Button } from "./ui/Button";
import { Segmented } from "./ui/Segmented";

/**
 * One worktree's changed files, each in a section with a Monaco diff and its review comments under
 * their lines. Clicking line numbers adds a comment, and Send review batch sends the open ones to an
 * agent session.
 */
export function DiffView({
  bridge,
  worktree,
  agentStates,
  onSent,
  opened,
  reveal = null,
  onRevealed,
}: {
  bridge: Bridge;
  worktree: string;
  /** Each agent session's latest state, by session, for the review batch picker. */
  agentStates: Record<SessionId, AgentStateChanged>;
  /** Called with the agent session a review batch went to. */
  onSent(session: SessionId): void;
  /** The base the Diff was last asked to open on. Each new request switches to its base. */
  opened?: DiffOpened;
  /** A file to show on the HEAD base, by its path in the worktree. */
  reveal?: string | null;
  /** Called once the view took `reveal`, so the next request for the same file shows it again. */
  onRevealed?: () => void;
}) {
  const [base, setBase] = useState<DiffBase>(opened?.base ?? "Head");
  const [appliedRequest, setAppliedRequest] = useState(opened?.request);
  if (opened && opened.request !== appliedRequest) {
    setAppliedRequest(opened.request);
    setBase(opened.base);
  }
  // The files keep the base they came from, so their versions come from that same base.
  const [diff, setDiff] = useState<{ base: DiffBase; files: FileDiff[]; error?: string }>({
    base,
    files: [],
  });
  // Bumped on each DiffChanged for this worktree, to load the diff again.
  const [changes, setChanges] = useState(0);
  // Paths whose sections are collapsed. A collapsed section stays mounted, so its comment boxes
  // keep what was typed in them.
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  // The reveal taken so far. It follows the prop during render, as state that follows a prop does.
  const [taken, setTaken] = useState<string | null>(null);
  if (reveal !== taken) {
    setTaken(reveal);
    if (reveal !== null) {
      setBase("Head");
      if (collapsed.has(reveal)) setCollapsed(toggled(collapsed, reveal));
    }
  }
  // Each file's section by path, and the revealed file to scroll to once its section is there.
  const sections = useRef(new Map<string, HTMLElement>());
  const scrollTo = useRef<string | null>(null);
  const revealed = useEffectEvent(() => onRevealed?.());
  useEffect(() => {
    if (reveal === null) return;
    scrollTo.current = reveal;
    revealed();
  }, [reveal]);
  // Each text file's last versions loaded, by path. They stay on screen until the next ones arrive,
  // so a refresh never empties an editor.
  const [shown, setShown] = useState<Record<string, Shown>>({});
  const { comments, error: commentsError } = useComments(bridge, worktree);
  // The comment being typed: the lines picked in its file, and their text then, which it
  // follows as the agent edits the file.
  const [composing, setComposing] = useState<Draft | null>(null);
  const drafts = useRef(0);
  const [picking, setPicking] = useState(false);
  const [resolveError, setResolveError] = useState<string | null>(null);

  useEffect(() => {
    const section = scrollTo.current === null ? undefined : sections.current.get(scrollTo.current);
    if (!section) return;
    section.scrollIntoView({ block: "start" });
    scrollTo.current = null;
  }, [reveal, diff]);

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

  useEffect(() => {
    // git already calls these binary, so reading one would load the whole file only to hide it.
    const texts = diff.files.filter((f) => !f.binary);
    let current = true;
    void Promise.all(
      texts.map((f) =>
        bridge.fileVersions(worktree, diff.base, f).then(
          (versions): Shown => ({ versions }),
          (e: unknown): Shown => ({ error: String(e) }),
        ),
      ),
    ).then((loaded) => current && setShown(Object.fromEntries(texts.map((f, i) => [f.newPath, loaded[i]]))));
    return () => {
      current = false;
    };
  }, [bridge, worktree, diff]);

  const openCount = comments.filter((c) => c.state === "Open").length;
  // The picker closes with the button, so the next open comment does not bring it back unasked.
  if (picking && openCount === 0) setPicking(false);
  // On files not in the diff, or that it shows only as binary.
  const unshown = comments.filter((c) => {
    const changed = diff.files.find((f) => f.newPath === c.path);
    const loaded = shown[c.path];
    return !changed || changed.binary || (loaded && "versions" in loaded && loaded.versions.binary);
  });
  // Where the comment being typed is now. null once the agent changed its lines.
  const composingIn = composing && shown[composing.path];
  const placed =
    composing && composingIn && "versions" in composingIn ? placeDraft(composing, composingIn.versions) : null;

  // Fails only when the comment is already gone, which the next load shows.
  function resolve(id: number) {
    bridge.resolveComment(id).then(
      () => setResolveError(null),
      (e: unknown) => setResolveError(String(e)),
    );
  }

  function startComment(path: string, startLine: number, endLine: number) {
    const loaded = shown[path];
    if (!loaded || !("versions" in loaded)) return;
    const text = fileLines(loaded.versions.new ?? "").slice(startLine - 1, endLine).join("\n");
    drafts.current += 1;
    setComposing({ draft: drafts.current, path, startLine, endLine, text });
  }

  async function addComment(body: string) {
    if (!composing || !placed) return;
    await bridge.addComment(worktree, { path: composing.path, ...placed, body });
    setComposing(null);
  }

  // A file's comments under their last line, and the box for a new one under its lines.
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
      <div className="diff-files">
        {diff.error && <p role="alert">{diff.error}</p>}
        {(commentsError ?? resolveError) && <p role="alert">{commentsError ?? resolveError}</p>}
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
        <ul aria-label="Changed files">
          {diff.files.map((f) => (
            <li
              key={f.newPath}
              aria-label={f.newPath}
              ref={(section) => {
                if (!section) return;
                sections.current.set(f.newPath, section);
                return () => {
                  sections.current.delete(f.newPath);
                };
              }}
            >
              <h3>
                <button
                  aria-expanded={!collapsed.has(f.newPath)}
                  title={f.newPath}
                  onClick={() => setCollapsed(toggled(collapsed, f.newPath))}
                >
                  {collapsed.has(f.newPath) ? <ChevronRight aria-hidden="true" /> : <ChevronDown aria-hidden="true" />}
                  <span className="path">{f.newPath}</span>{" "}
                  {f.binary ? (
                    <span>binary</span>
                  ) : (
                    <>
                      <span className="added">+{f.added}</span>{" "}
                      <span className="removed">-{f.removed}</span>
                    </>
                  )}
                </button>
              </h3>
              <div className="file" hidden={collapsed.has(f.newPath)}>
                {f.binary
                  ? binaryNotice
                  : shown[f.newPath] && (
                      <Versions
                        path={f.newPath}
                        shown={shown[f.newPath]}
                        zones={zonesOf(f.newPath)}
                        onCommentLines={(startLine, endLine) => startComment(f.newPath, startLine, endLine)}
                      />
                    )}
              </div>
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}

type Shown = { versions: FileVersions } | { error: string };

/** A comment being typed. `draft` tells it from the one before, so a new pick starts afresh. */
/** An ask to show the Diff on `base`. A new `request` number asks again. */
export interface DiffOpened {
  base: DiffBase;
  request: number;
}

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
  path,
  shown,
  zones,
  onCommentLines,
}: {
  path: string;
  shown: Shown;
  zones: DiffZone[];
  onCommentLines(startLine: number, endLine: number): void;
}) {
  if ("error" in shown) return <p role="alert">{shown.error}</p>;
  if (shown.versions.binary) return binaryNotice;
  return (
    <MonacoDiff
      original={shown.versions.old === null ? null : visibleCarriageReturns(shown.versions.old)}
      modified={visibleCarriageReturns(shown.versions.new)}
      path={path}
      onCommentLines={onCommentLines}
      zones={zones}
    />
  );
}

/** `set` with `path` taken out if it was in, or put in if it was not. */
function toggled(set: ReadonlySet<string>, path: string): ReadonlySet<string> {
  const next = new Set(set);
  if (!next.delete(path)) next.add(path);
  return next;
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
