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
 * agent session. A folder that is not a git repo has no diff, and the view says so.
 */
export function DiffView({
  bridge,
  worktree,
  base,
  onBase,
  agentStates,
  onSent,
  reveal = null,
  onRevealed,
}: {
  bridge: Bridge;
  worktree: string;
  /** The base the diff shows: the one the shown agent session's workspace keeps. */
  base: DiffBase;
  /** Called with the base to show next: the one the user picked, or HEAD for a revealed file. */
  onBase(base: DiffBase): void;
  /** Each agent session's latest state, by session, for the review batch picker. */
  agentStates: Record<SessionId, AgentStateChanged>;
  /** Called with the agent session a review batch went to. */
  onSent(session: SessionId): void;
  /** A file to show on the HEAD base, by its path in the worktree. */
  reveal?: string | null;
  /** Called once the view took `reveal`, so the next request for the same file shows it again. */
  onRevealed?: () => void;
}) {
  // The files keep the base they came from, so their versions come from that same base. Each load
  // counts up `generation`, which tells a file's versions read from this load from older ones.
  const [diff, setDiff] = useState<{
    base: DiffBase;
    files: FileDiff[];
    generation: number;
    error?: string;
    /** The folder is not a git repo, so it has no diff. */
    notARepo?: boolean;
  }>({
    base,
    files: [],
    generation: 0,
  });
  const loads = useRef(0);
  // Bumped on each DiffChanged for this worktree, to load the diff again.
  const [changes, setChanges] = useState(0);
  // Paths whose sections are collapsed. A collapsed section stays mounted, so its comment boxes
  // keep what was typed in them.
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  // The reveal taken so far. It follows the prop during render, as state that follows a prop does.
  const [taken, setTaken] = useState<string | null>(null);
  if (reveal !== taken) {
    setTaken(reveal);
    if (reveal !== null && collapsed.has(reveal)) setCollapsed(toggled(collapsed, reveal));
  }
  // Each file's section by path. A revealed file waits for a HEAD diff to look for it in, then stays
  // pinned in view while the sections around it take their height, until the user scrolls.
  const sections = useRef(new Map<string, HTMLElement>());
  const revealWaiting = useRef<string | null>(null);
  const pinned = useRef<string | null>(null);
  const revealed = useEffectEvent(() => {
    onBase("Head");
    onRevealed?.();
  });
  useEffect(() => {
    if (reveal === null) return;
    revealWaiting.current = reveal;
    pinned.current = null;
    revealed();
  }, [reveal]);
  // Each text file's last versions loaded, by path. They stay on screen until the next ones arrive,
  // so a refresh never empties an editor.
  const [shown, setShown] = useState<Record<string, Shown>>({});
  // The paths whose sections are near the view, and the load each file is being read for.
  const [near, setNear] = useState<ReadonlySet<string>>(new Set());
  const reading = useRef(new Map<string, number>());
  // The scrolling box, and the list in it, which grows as its sections take their height.
  const fileList = useRef<HTMLDivElement>(null);
  const sectionList = useRef<HTMLUListElement>(null);
  const { comments, error: commentsError } = useComments(bridge, worktree);
  // The comment being typed: the lines picked in its file, and their text then, which it
  // follows as the agent edits the file.
  const [composing, setComposing] = useState<Draft | null>(null);
  const drafts = useRef(0);
  const [picking, setPicking] = useState(false);
  const [resolveError, setResolveError] = useState<string | null>(null);

  // A file not in the HEAD diff is dropped, so it never scrolls the view when it shows up later.
  useEffect(() => {
    const path = revealWaiting.current;
    if (path === null || diff.base !== "Head") return;
    revealWaiting.current = null;
    if (!diff.files.some((f) => f.newPath === path)) return;
    pinned.current = path;
    sections.current.get(path)?.scrollIntoView({ block: "start" });
  }, [reveal, diff]);

  // WKWebView does not anchor scrolling, so a section above the pinned one that takes its height
  // would push it down.
  useEffect(() => {
    const observer = new ResizeObserver(() => {
      if (pinned.current !== null) sections.current.get(pinned.current)?.scrollIntoView({ block: "start" });
    });
    observer.observe(sectionList.current!);
    return () => observer.disconnect();
  }, []);
  const unpin = () => {
    pinned.current = null;
  };

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
      (files) =>
        current && setDiff({ base, files: files ?? [], generation: ++loads.current, notARepo: files === null }),
      (e: unknown) => current && setDiff({ base, files: [], generation: ++loads.current, error: String(e) }),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, base, changes]);

  useEffect(() => {
    const observer = new IntersectionObserver(
      (entries) =>
        setNear((before) => {
          const after = new Set(before);
          for (const { target, isIntersecting } of entries) {
            const path = (target as HTMLElement).dataset.path!;
            if (isIntersecting) after.add(path);
            else after.delete(path);
          }
          return after.size === before.size && [...after].every((path) => before.has(path)) ? before : after;
        }),
      { root: fileList.current, rootMargin: NEAR_VIEW_MARGIN },
    );
    sections.current.forEach((section) => observer.observe(section));
    return () => observer.disconnect();
  }, [diff.files]);

  // Only open sections near the view read their file, each once per load of the diff. The rest read
  // theirs when they open or come near. git already calls a binary file binary, so reading one would
  // load the whole file only to hide it.
  useEffect(() => {
    const { base: from, generation } = diff;
    for (const file of diff.files) {
      const path = file.newPath;
      if (file.binary || collapsed.has(path) || !near.has(path)) continue;
      if (shown[path]?.generation === generation || reading.current.get(path) === generation) continue;
      reading.current.set(path, generation);
      // Each file shows as its own read lands. A read from an older load never replaces a newer one.
      const land = (loaded: Shown) =>
        setShown((before) =>
          (before[path]?.generation ?? 0) > generation ? before : { ...before, [path]: loaded },
        );
      bridge.fileVersions(worktree, from, file).then(
        (versions) => land({ generation, versions }),
        (e: unknown) => land({ generation, error: String(e) }),
      );
    }
  }, [bridge, worktree, diff, collapsed, near, shown]);

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
      <div className="toolbar" hidden={diff.notARepo}>
        <Segmented<DiffBase>
          label="Diff base"
          options={bases}
          value={base}
          onChange={(next) => {
            unpin();
            onBase(next);
          }}
        />
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
      <div
        className="diff-files"
        ref={fileList}
        onWheel={unpin}
        onPointerDown={unpin}
        onKeyDown={unpin}
        onTouchStart={unpin}
      >
        {diff.notARepo && <p className="panel-empty">This folder is not a git repo, so it has no diff.</p>}
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
        <ul aria-label="Changed files" ref={sectionList}>
          {diff.files.map((f) => (
            <li
              key={f.newPath}
              aria-label={f.newPath}
              data-path={f.newPath}
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

/** A file's versions, or why they could not be read, from the diff load `generation`. */
type Shown = { generation: number } & ({ versions: FileVersions } | { error: string });

// How far past the visible list, above and below, a section counts as near the view.
const NEAR_VIEW_MARGIN = "100% 0px";

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
