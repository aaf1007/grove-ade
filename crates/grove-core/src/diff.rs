//! The diff of a worktree against a diff base, through git2 (ADR 0004). What it must match is in
//! ADR 0012.

mod cli;

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use git2::{
    AttrCheckFlags, AttrValue, Commit, Delta, DiffFindOptions, DiffLineType, DiffOptions,
    ErrorCode, Patch, Repository,
};

use crate::{Core, CoreError, Result, worktree};

type GitResult<T> = std::result::Result<T, git2::Error>;

/// What a diff compares the worktree against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffBase {
    /// All uncommitted changes, staged or not, plus untracked files. The same as `git diff HEAD`.
    Head,
    /// Everything since the branch left the worktree's base branch, committed or not, plus
    /// untracked files. The same as `git diff $(git merge-base <base> HEAD)`, with `origin/<base>`
    /// in place of `<base>` when it exists.
    MergeBase,
}

// A diff base with the worktree's base branch read in, for the blocking side.
pub(crate) enum Start {
    Head,
    MergeBase(String),
}

/// One file's change, in the structured form ADR 0012 compares with `git diff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub status: FileStatus,
    /// Relative to the worktree. The same as `new_path` unless the file was renamed.
    pub old_path: PathBuf,
    pub new_path: PathBuf,
    /// As git decides it. A binary file has no hunks.
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    /// Not in the index and not ignored. `git diff HEAD` leaves it out, so it matches
    /// `git diff --no-index /dev/null <file>` instead.
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's exact bytes, with its newline unless it ends a file that has none.
    pub text: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// A run of a file's lines changed since `HEAD`, for the editor's gutter. Lines are 1-based, in
/// the file as it is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineChange {
    /// Lines `start` to `end` are new.
    Added { start: u32, end: u32 },
    /// Lines `start` to `end` replaced others.
    Modified { start: u32, end: u32 },
    /// Lines were removed after line `after`. 0 means above the first line.
    Deleted { after: u32 },
}

/// Both versions of a file in a diff, as exact bytes, for Monaco. `None` where the file does not
/// exist: the old side of an added or untracked file, the new side of a deleted one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVersions {
    pub old: Option<Vec<u8>>,
    pub new: Option<Vec<u8>>,
    /// git calls the file binary, or a version is not UTF-8. The UI shows it outside Monaco.
    pub binary: bool,
}

impl Core {
    /// The diff of the worktree at `worktree` against `base`, file by file in path order.
    pub async fn diff(&self, worktree: &Path, base: DiffBase) -> Result<Vec<FileDiff>> {
        let start = self.diff_start(worktree, base)?;
        let dir = worktree.to_path_buf();
        self.runtime
            .spawn_blocking(move || diff_from(&dir, &start))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("diff", worktree)(io::Error::other(e)))?
    }

    /// Both versions of `file`, from a diff of the worktree at `worktree` against `base`. Reads
    /// only its status, paths and binary flag.
    pub async fn file_versions(
        &self,
        worktree: &Path,
        base: DiffBase,
        file: &FileDiff,
    ) -> Result<FileVersions> {
        let start = self.diff_start(worktree, base)?;
        let dir = worktree.to_path_buf();
        let file = FileDiff {
            status: file.status,
            old_path: file.old_path.clone(),
            new_path: file.new_path.clone(),
            binary: file.binary,
            hunks: Vec::new(),
        };
        self.runtime
            .spawn_blocking(move || versions(&dir, &start, &file))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("read file versions in", worktree)(io::Error::other(e)))?
    }

    /// The lines of `file`, relative to the worktree at `worktree`, changed since `HEAD`: the
    /// Head diff's hunks for that file, as gutter marks. Empty when the file has no change or is
    /// gone.
    pub async fn line_changes(&self, worktree: &Path, file: &Path) -> Result<Vec<LineChange>> {
        let files = self.diff(worktree, DiffBase::Head).await?;
        Ok(files
            .iter()
            .find(|f| f.new_path == file && f.status != FileStatus::Deleted)
            .map(|f| line_changes_of(&f.hunks))
            .unwrap_or_default())
    }

    // Fails when grove does not track the worktree, or when `MergeBase` has no base branch to use.
    fn diff_start(&self, worktree: &Path, base: DiffBase) -> Result<Start> {
        let tracked = self.tracked();
        let found = tracked
            .iter()
            .find(|w| w.path == worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?;
        match base {
            DiffBase::Head => Ok(Start::Head),
            DiffBase::MergeBase => found
                .base_branch
                .clone()
                .map(Start::MergeBase)
                .ok_or_else(|| CoreError::NoBaseBranch(worktree.to_path_buf())),
        }
    }

    pub(crate) fn check_tracked(&self, worktree: &Path) -> Result<()> {
        if self.tracked().iter().any(|w| w.path == worktree) {
            Ok(())
        } else {
            Err(CoreError::UnknownWorktree(worktree.to_path_buf()))
        }
    }
}

// Opens the repo on every call, so nothing goes stale between calls. It blocks while git2 and git
// work.
pub(crate) fn diff_from(worktree: &Path, start: &Start) -> Result<Vec<FileDiff>> {
    let repo = Repository::open(worktree).map_err(CoreError::git2(worktree))?;
    let from = start_commit(&repo, worktree, start)?;
    let (mut files, routed) = git2_diff(&repo, &from).map_err(CoreError::git2(worktree))?;
    if !routed.paths.is_empty() {
        let from = from.id().to_string();
        files.extend(cli::diff(worktree, &from, &routed.paths, &routed.config)?);
    }
    files.sort_by(|a, b| a.new_path.as_os_str().cmp(b.new_path.as_os_str()));
    Ok(files)
}

// The commit a diff starts from: HEAD, or the merge base of the base branch and HEAD. The base
// branch is `origin/<base>` when it exists, as when a new branch starts from it.
fn start_commit<'r>(repo: &'r Repository, worktree: &Path, start: &Start) -> Result<Commit<'r>> {
    let head = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map_err(CoreError::git2(worktree))?;
    let Start::MergeBase(branch) = start else {
        return Ok(head);
    };
    let base = repo
        .find_reference(&worktree::start_point(worktree, branch)?)
        .and_then(|r| r.peel_to_commit())
        .map_err(CoreError::git2(worktree))?;
    let merge_base = repo
        .merge_base(base.id(), head.id())
        .map_err(|e| match e.code() {
            ErrorCode::NotFound => CoreError::NoMergeBase {
                worktree: worktree.to_path_buf(),
                base: branch.clone(),
            },
            _ => CoreError::git2(worktree)(e),
        })?;
    repo.find_commit(merge_base)
        .map_err(CoreError::git2(worktree))
}

// Files libgit2 cannot diff, which go through the git CLI instead: those whose attributes name a
// `filter` or `diff` driver (ADR 0012), and conflicted ones.
struct Routed {
    paths: Vec<PathBuf>,
    /// `name=value` for each config key of those drivers.
    config: Vec<OsString>,
}

fn git2_diff(repo: &Repository, from: &Commit<'_>) -> GitResult<(Vec<FileDiff>, Routed)> {
    let tree = from.tree()?;
    let mut diff =
        repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut untracked_opts()))?;
    // Explicit options and limit, so libgit2 reads neither `diff.renames` nor
    // `diff.renameLimit`. 1000 is git's default limit.
    diff.find_similar(Some(
        DiffFindOptions::new().renames(true).rename_limit(1000),
    ))?;
    let mut files = Vec::new();
    let (mut routed, mut drivers) = (Vec::new(), Vec::new());
    for (i, delta) in diff.deltas().enumerate() {
        let (old_path, new_path) = paths(&delta);
        // `git diff <commit>` has no untracked files to route them to.
        if delta.status() != Delta::Untracked {
            let mut found = drivers_of(repo, &old_path)?;
            if new_path != old_path {
                found.extend(drivers_of(repo, &new_path)?);
            }
            // libgit2 gives a conflicted file no hunks.
            if !found.is_empty() || delta.status() == Delta::Conflicted {
                // The CLI pairs a rename only when it sees both paths.
                if old_path != new_path {
                    routed.push(old_path);
                }
                routed.push(new_path);
                drivers.extend(found);
                continue;
            }
        }
        let status = match delta.status() {
            Delta::Added => FileStatus::Added,
            Delta::Deleted => FileStatus::Deleted,
            Delta::Modified => FileStatus::Modified,
            Delta::Renamed => FileStatus::Renamed,
            Delta::Untracked => FileStatus::Untracked,
            // Copies are never looked for, a typechange comes split into a delete and an add,
            // conflicts went to the CLI above, and the diff options leave out the rest.
            Delta::Conflicted
            | Delta::Copied
            | Delta::Typechange
            | Delta::Unmodified
            | Delta::Ignored
            | Delta::Unreadable => continue,
        };
        files.push(file_diff(&diff, i, status)?);
    }
    files.extend(recreated(repo, &files)?);
    let config = driver_config(repo, &drivers)?;
    Ok((
        files,
        Routed {
            paths: routed,
            config,
        },
    ))
}

fn untracked_opts() -> DiffOptions {
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .indent_heuristic(true);
    opts
}

fn paths(delta: &git2::DiffDelta<'_>) -> (PathBuf, PathBuf) {
    let path = |f: git2::DiffFile<'_>| f.path().map(Path::to_path_buf).unwrap_or_default();
    (path(delta.old_file()), path(delta.new_file()))
}

fn file_diff(diff: &git2::Diff<'_>, i: usize, status: FileStatus) -> GitResult<FileDiff> {
    let patch = Patch::from_diff(diff, i)?;
    let delta = patch
        .as_ref()
        .map(|p| p.delta())
        .or_else(|| diff.get_delta(i));
    let (old_path, new_path) = delta.as_ref().map(paths).unwrap_or_default();
    let binary = delta.is_some_and(|d| d.flags().is_binary());
    let hunks = match &patch {
        Some(p) if !binary => hunks(p)?,
        _ => Vec::new(),
    };
    Ok(FileDiff {
        status,
        old_path,
        new_path,
        binary,
        hunks,
    })
}

// libgit2 joins the index and worktree halves of a diff by path, so a file deleted or renamed away
// in the index but back on disk shows only as deleted or renamed. git also lists it as untracked,
// so this diffs those files again from the index, where they are untracked.
fn recreated(repo: &Repository, files: &[FileDiff]) -> GitResult<Vec<FileDiff>> {
    let Some(workdir) = repo.workdir() else {
        return Ok(Vec::new());
    };
    let mut opts = untracked_opts();
    opts.disable_pathspec_match(true);
    let mut any = false;
    for file in files {
        if matches!(file.status, FileStatus::Deleted | FileStatus::Renamed)
            && fs::symlink_metadata(workdir.join(&file.old_path)).is_ok()
        {
            opts.pathspec(&file.old_path);
            any = true;
        }
    }
    if !any {
        return Ok(Vec::new());
    }
    let diff = repo.diff_index_to_workdir(None, Some(&mut opts))?;
    let mut found = Vec::new();
    for (i, delta) in diff.deltas().enumerate() {
        if delta.status() == Delta::Untracked {
            found.push(file_diff(&diff, i, FileStatus::Untracked)?);
        }
    }
    Ok(found)
}

// The config prefix of each `filter` or `diff` driver that the attributes of `path` name, such as
// `filter.lfs.`. Plain `diff` and `-diff` name none, and libgit2 handles them.
fn drivers_of(repo: &Repository, path: &Path) -> GitResult<Vec<String>> {
    let mut found = Vec::new();
    for attr in ["filter", "diff"] {
        let value = repo.get_attr_bytes(path, attr, AttrCheckFlags::FILE_THEN_INDEX)?;
        let name = match AttrValue::from_bytes(value) {
            AttrValue::String(name) => name.to_owned(),
            AttrValue::Bytes(name) => String::from_utf8_lossy(name).into_owned(),
            _ => continue,
        };
        found.push(format!("{attr}.{name}."));
    }
    Ok(found)
}

// Every config key under `prefixes`, as git2 reads the config. The git CLI runs with the user's
// config hidden, so this is how it finds a driver set there, such as git-lfs's `filter.lfs.*`.
fn driver_config(repo: &Repository, prefixes: &[String]) -> GitResult<Vec<OsString>> {
    let mut pairs = Vec::new();
    if prefixes.is_empty() {
        return Ok(pairs);
    }
    let config = repo.config()?;
    let mut entries = config.entries(None)?;
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let name = entry.name_bytes();
        if prefixes.iter().any(|p| name.starts_with(p.as_bytes())) {
            let mut pair = OsStr::from_bytes(name).to_os_string();
            // A key with no value means true, and `-c <name>` says the same.
            if entry.has_value() {
                pair.push("=");
                pair.push(OsStr::from_bytes(entry.value_bytes()));
            }
            pairs.push(pair);
        }
    }
    Ok(pairs)
}

fn hunks(patch: &Patch<'_>) -> GitResult<Vec<Hunk>> {
    (0..patch.num_hunks())
        .map(|h| {
            let (hunk, count) = patch.hunk(h)?;
            let mut lines = Vec::with_capacity(count);
            for l in 0..count {
                let line = patch.line_in_hunk(h, l)?;
                let kind = match line.origin_value() {
                    DiffLineType::Context => LineKind::Context,
                    DiffLineType::Addition => LineKind::Added,
                    DiffLineType::Deletion => LineKind::Removed,
                    // The rest mark a missing newline at the end of the file. The line before
                    // already lacks its `\n`.
                    _ => continue,
                };
                lines.push(DiffLine {
                    kind,
                    text: line.content().to_vec(),
                });
            }
            Ok(Hunk {
                old_start: hunk.old_start(),
                old_lines: hunk.old_lines(),
                new_start: hunk.new_start(),
                new_lines: hunk.new_lines(),
                lines,
            })
        })
        .collect()
}

fn versions(worktree: &Path, start: &Start, file: &FileDiff) -> Result<FileVersions> {
    // The paths may come back from the UI, so nothing may lead out of the worktree.
    for path in [&file.old_path, &file.new_path] {
        check_inside(path)?;
    }
    let old = match file.status {
        FileStatus::Added | FileStatus::Untracked => None,
        _ => Some(start_blob(worktree, start, &file.old_path)?),
    };
    let new = match file.status {
        FileStatus::Deleted => None,
        _ => Some(worktree_file(worktree, &file.new_path)?),
    };
    let utf8 = |v: &Option<Vec<u8>>| v.as_deref().is_none_or(|b| std::str::from_utf8(b).is_ok());
    Ok(FileVersions {
        binary: file.binary || !utf8(&old) || !utf8(&new),
        old,
        new,
    })
}

/// Fails unless `path` is relative and has no `..`, so joined to a worktree it stays inside.
pub(crate) fn check_inside(path: &Path) -> Result<()> {
    if path.components().all(|c| matches!(c, Component::Normal(_))) {
        Ok(())
    } else {
        Err(CoreError::OutsideWorktree(path.to_path_buf()))
    }
}

fn start_blob(worktree: &Path, start: &Start, path: &Path) -> Result<Vec<u8>> {
    let repo = Repository::open(worktree).map_err(CoreError::git2(worktree))?;
    let from = start_commit(&repo, worktree, start)?;
    let read = || {
        let blob = from
            .tree()?
            .get_path(path)?
            .to_object(&repo)?
            .peel_to_blob()?;
        Ok(blob.content().to_vec())
    };
    read().map_err(CoreError::git2(worktree))
}

/// The current version of `file`, relative to the worktree at `worktree`. A symlink's version is
/// its target, as git stores it, so the file may link anywhere. Fails unless the folders on the
/// way resolve inside the worktree, so a symlinked folder cannot lead out of it.
pub(crate) fn worktree_file(worktree: &Path, file: &Path) -> Result<Vec<u8>> {
    check_inside(file)?;
    let path = worktree.join(file);
    if let Some(folder) = path.parent() {
        let root = fs::canonicalize(worktree).map_err(CoreError::io("resolve", worktree))?;
        let real = fs::canonicalize(folder).map_err(CoreError::io("resolve", folder))?;
        if !real.starts_with(&root) {
            return Err(CoreError::OutsideWorktree(file.to_path_buf()));
        }
    }
    let meta = fs::symlink_metadata(&path).map_err(CoreError::io("read", &path))?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(&path).map_err(CoreError::io("read the link", &path))?;
        Ok(target.into_os_string().into_vec())
    } else {
        fs::read(&path).map_err(CoreError::io("read", &path))
    }
}

// Each run of removed and added lines becomes one change: added lines alone are Added, removed
// lines alone are Deleted, and both together are Modified.
fn line_changes_of(hunks: &[Hunk]) -> Vec<LineChange> {
    let mut changes = Vec::new();
    for hunk in hunks {
        // git starts a hunk that leaves no lines at the line before it.
        let mut next = hunk.new_start + u32::from(hunk.new_lines == 0);
        let (mut removed, mut added) = (0, 0);
        let mut end_run = |next: u32, removed: &mut u32, added: &mut u32| {
            let (start, end) = (next - *added, next.saturating_sub(1));
            match (*removed, *added) {
                (0, 0) => {}
                (_, 0) => changes.push(LineChange::Deleted { after: end }),
                (0, _) => changes.push(LineChange::Added { start, end }),
                _ => changes.push(LineChange::Modified { start, end }),
            }
            (*removed, *added) = (0, 0);
        };
        for line in &hunk.lines {
            match line.kind {
                LineKind::Removed => removed += 1,
                LineKind::Added => {
                    added += 1;
                    next += 1;
                }
                LineKind::Context => {
                    end_run(next, &mut removed, &mut added);
                    next += 1;
                }
            }
        }
        end_run(next, &mut removed, &mut added);
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    // A hunk from its lines: `+` added, `-` removed, anything else context.
    fn hunk(old_start: u32, new_start: u32, kinds: &str) -> Hunk {
        let lines: Vec<DiffLine> = kinds
            .chars()
            .map(|c| DiffLine {
                kind: match c {
                    '+' => LineKind::Added,
                    '-' => LineKind::Removed,
                    _ => LineKind::Context,
                },
                text: b"x\n".to_vec(),
            })
            .collect();
        let count = |skip| lines.iter().filter(|l| l.kind != skip).count() as u32;
        Hunk {
            old_start,
            old_lines: count(LineKind::Added),
            new_start,
            new_lines: count(LineKind::Removed),
            lines,
        }
    }

    #[test]
    fn line_changes_follow_the_hunks_in_the_file_as_it_is_now() {
        let hunks = [
            // Line 2 replaced, then lines 4 and 5 new.
            hunk(1, 1, " -+ ++ "),
            // Two lines gone after line 10, then two replaced by three.
            hunk(9, 10, " -- --+++ "),
        ];

        assert_eq!(
            line_changes_of(&hunks),
            vec![
                LineChange::Modified { start: 2, end: 2 },
                LineChange::Added { start: 4, end: 5 },
                LineChange::Deleted { after: 10 },
                LineChange::Modified { start: 12, end: 14 },
            ]
        );
    }

    #[test]
    fn a_deletion_at_the_top_or_of_every_line_sits_after_line_0() {
        // git starts a hunk that leaves no lines at the line before it.
        assert_eq!(
            line_changes_of(&[hunk(1, 0, "--")]),
            vec![LineChange::Deleted { after: 0 }]
        );
        assert_eq!(
            line_changes_of(&[hunk(1, 1, "- ")]),
            vec![LineChange::Deleted { after: 0 }]
        );
    }

    #[test]
    fn a_new_file_is_added_from_its_first_line_to_its_last() {
        assert_eq!(
            line_changes_of(&[hunk(0, 1, "+++")]),
            vec![LineChange::Added { start: 1, end: 3 }]
        );
    }
}
