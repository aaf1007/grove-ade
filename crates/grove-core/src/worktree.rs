use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::PoisonError;

use crate::watch::{self, Watch};
use crate::{Core, CoreError, CoreEvent, Result, SessionId};

/// A worktree grove tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    /// None when HEAD is detached.
    pub branch: Option<String>,
    /// The repo's original checkout. grove never removes it.
    pub is_main: bool,
    /// Defaults to the branch, or to the folder name when HEAD is detached.
    pub display_name: String,
    pub issue_link: Option<String>,
    /// What a whole-branch diff compares against. None when the repo has no default branch.
    pub base_branch: Option<String>,
}

/// A worktree to add with `Core::create_worktree`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewWorktree {
    /// A new name makes a branch from the base branch. A local branch, or one only on `origin`,
    /// is checked out.
    pub branch: String,
    /// None means the branch name.
    pub display_name: Option<String>,
    pub issue_link: Option<String>,
    /// None means the repo's default branch: `origin/HEAD`, then `main`, then `master`.
    pub base_branch: Option<String>,
}

/// What `Core::create_worktree` added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedWorktree {
    pub worktree: WorktreeInfo,
    /// The run session running the repo's setup script. None when it has none.
    pub setup: Option<SessionId>,
}

impl Core {
    /// Adds a worktree to the repo whose main worktree is at `repo`, in
    /// `<home>/worktrees/<repo>/<branch>` with `/` replaced by `-`. It runs `git worktree add`,
    /// so the user's git config and hooks apply (ADR 0004). Then it starts the repo's setup
    /// script in it as a run session. A setup that cannot start fails the call, and the
    /// worktree stays added.
    pub async fn create_worktree(&self, repo: &Path, new: NewWorktree) -> Result<AddedWorktree> {
        let unknown = || CoreError::UnknownRepo(repo.to_path_buf());
        if !self.tracked().iter().any(|w| w.is_main && w.path == repo) {
            return Err(unknown());
        }
        let repo_name = repo.file_name().ok_or_else(unknown)?;
        let folder = self
            .worktrees_root
            .join(repo_name)
            .join(new.branch.replace('/', "-"));
        if self.tracked().iter().any(|w| w.path == folder) {
            return Err(CoreError::FolderTaken(folder));
        }
        let dir = repo.to_path_buf();
        let events = self.events.clone();
        let (added, watch) = self
            .runtime
            .spawn_blocking(move || {
                let added = add(&dir, folder, new)?;
                // The worktree is on disk now, so it is tracked even without live updates.
                let watch = Watch::start(&added.path, events);
                Ok::<_, CoreError>((added, watch))
            })
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("run git in", repo)(io::Error::other(e)))??;

        // Keep the list repo by repo: the new worktree goes after the last one of its repo.
        let failed = {
            let mut tracked = self.tracked();
            let start = tracked.iter().position(|w| w.is_main && w.path == repo);
            let end = start
                .and_then(|i| {
                    tracked[i + 1..]
                        .iter()
                        .position(|w| w.is_main)
                        .map(|n| i + 1 + n)
                })
                .unwrap_or(tracked.len());
            tracked.insert(end, added.clone());
            match watch {
                Ok(watch) => {
                    self.watches().insert(added.path.clone(), watch);
                    None
                }
                Err(e) => Some(watch::not_started(&added.path, &e)),
            }
        };
        // Once tracked, so a retry finds the worktree.
        if let Some(warning) = failed {
            let _ = self.events.send(CoreEvent::Warning(warning));
        }
        self.save_state().await?;
        let setup = self.start_setup(repo, &added.path)?;
        Ok(AddedWorktree {
            worktree: added,
            setup,
        })
    }

    /// Changes the base branch of the worktree at `worktree`. The branch must exist locally or
    /// on `origin`.
    pub async fn set_base_branch(&self, worktree: &Path, base: String) -> Result<()> {
        // Fails fast when grove does not track it.
        self.update(worktree, |_| ())?;
        let dir = worktree.to_path_buf();
        let checked = base.clone();
        self.runtime
            .spawn_blocking(move || start_point(&dir, &checked).map(|_| ()))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("run git in", worktree)(io::Error::other(e)))??;
        self.update(worktree, |w| w.base_branch = Some(base))?;
        self.save_state().await
    }

    pub async fn set_display_name(&self, worktree: &Path, name: String) -> Result<()> {
        self.update(worktree, |w| w.display_name = name)?;
        self.save_state().await
    }

    /// None removes the link.
    pub async fn set_issue_link(&self, worktree: &Path, link: Option<String>) -> Result<()> {
        self.update(worktree, |w| w.issue_link = link)?;
        self.save_state().await
    }

    /// Starts the watcher of the worktree at `worktree`, after a `WatcherNotStarted` warning. It
    /// does nothing when the worktree has a watcher. Fails when grove does not track the worktree,
    /// while it is being removed, or when the watcher fails to start again, with no second
    /// warning. A watcher it starts sends `DiffChanged` at once, since the diff may have changed
    /// while nothing watched it.
    pub async fn retry_watcher(&self, worktree: &Path) -> Result<()> {
        if !self.check_watcher(worktree, |needed, _| needed)? {
            return Ok(());
        }
        let dir = worktree.to_path_buf();
        let events = self.events.clone();
        let watch = self
            .runtime
            .spawn_blocking(move || Watch::start(&dir, events))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("watch", worktree)(io::Error::other(e)))??;
        // Checked again, since a removal or another retry may have come first.
        let (needed, unused) = self.check_watcher(worktree, |needed, watches| {
            if matches!(needed, Ok(true)) {
                watches.insert(worktree.to_path_buf(), watch);
                (needed, None)
            } else {
                (needed, Some(watch))
            }
        });
        // Dropping a watch joins its thread, so do it off this tokio worker.
        if let Some(watch) = unused {
            self.runtime.spawn_blocking(move || drop(watch));
        }
        if needed? {
            let _ = self.events.send(CoreEvent::DiffChanged {
                worktree: worktree.to_path_buf(),
            });
        }
        Ok(())
    }

    // Calls `f` with whether the worktree at `worktree` needs a watcher, and the watches, with
    // all three locks held. It needs one when it has none. It fails when grove does not track it
    // or it is being removed. Locks `removing`, then `tracked`, then `watches`: a run session
    // that ends locks `removing` and then `tracked`, and `add_repo` locks `tracked` and then
    // `watches`, so another order can deadlock.
    fn check_watcher<T>(
        &self,
        worktree: &Path,
        f: impl FnOnce(Result<bool>, &mut HashMap<PathBuf, Watch>) -> T,
    ) -> T {
        let removing = self.removing.lock().unwrap_or_else(PoisonError::into_inner);
        let tracked = self.tracked();
        let mut watches = self.watches();
        let needed = if !tracked.iter().any(|w| w.path == worktree) {
            Err(CoreError::UnknownWorktree(worktree.to_path_buf()))
        } else if removing.contains(worktree) {
            Err(CoreError::Removing(worktree.to_path_buf()))
        } else {
            Ok(!watches.contains_key(worktree))
        };
        f(needed, &mut watches)
    }

    fn update(&self, worktree: &Path, change: impl FnOnce(&mut WorktreeInfo)) -> Result<()> {
        let mut tracked = self.tracked();
        let found = tracked
            .iter_mut()
            .find(|w| w.path == worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?;
        change(found);
        Ok(())
    }
}

/// Reads the worktrees of the repo that `dir` is in, main worktree first, through the git CLI
/// (ADR 0004). Each gets the repo's default base branch. It blocks while git runs.
pub(crate) fn list(dir: &Path) -> Result<Vec<WorktreeInfo>> {
    let out = git(dir)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .map_err(CoreError::io("run git in", dir))?;
    if !out.status.success() {
        return Err(CoreError::NotARepo {
            path: dir.to_path_buf(),
            reason: stderr(&out),
        });
    }
    let base = default_base(dir)?;
    let mut worktrees = parse(&out.stdout);
    for worktree in &mut worktrees {
        worktree.base_branch = base.clone();
    }
    Ok(worktrees)
}

// With -z every field ends in a NUL. A `worktree` field starts each entry, main worktree first.
fn parse(porcelain: &[u8]) -> Vec<WorktreeInfo> {
    let mut worktrees = Vec::new();
    for field in porcelain.split(|&b| b == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            let path = PathBuf::from(OsStr::from_bytes(path));
            worktrees.push(WorktreeInfo {
                display_name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path,
                branch: None,
                is_main: worktrees.is_empty(),
                issue_link: None,
                base_branch: None,
            });
        } else if let Some(name) = field.strip_prefix(b"branch ")
            && let Some(worktree) = worktrees.last_mut()
        {
            let name = String::from_utf8_lossy(name);
            let name = name.strip_prefix("refs/heads/").unwrap_or(&name);
            worktree.branch = Some(name.to_owned());
            worktree.display_name = name.to_owned();
        }
    }
    worktrees
}

// Runs `git worktree add` for `new` into `folder`. It blocks while git runs.
fn add(repo: &Path, folder: PathBuf, new: NewWorktree) -> Result<WorktreeInfo> {
    let branch = new.branch;
    let valid = run(repo, &["check-ref-format", "--branch", &branch])?;
    if !valid.status.success() {
        return Err(CoreError::InvalidBranchName {
            name: branch,
            reason: stderr(&valid),
        });
    }
    if fs::symlink_metadata(&folder).is_ok() {
        return Err(CoreError::FolderTaken(folder));
    }
    let base = match new.base_branch {
        Some(base) => {
            start_point(repo, &base)?;
            Some(base)
        }
        None => default_base(repo)?,
    };

    let mut cmd = git(repo);
    cmd.args(["worktree", "add", "--quiet"]);
    let remote = format!("refs/remotes/origin/{branch}");
    if has_ref(repo, &format!("refs/heads/{branch}"))? {
        cmd.arg(&folder).arg(&branch);
    } else if has_ref(repo, &remote)? {
        cmd.args(["--track", "-b", &branch])
            .arg(&folder)
            .arg(remote);
    } else {
        let base = base
            .as_deref()
            .ok_or_else(|| CoreError::NoBaseBranch(repo.to_path_buf()))?;
        // No upstream, so the new branch does not track the base branch.
        cmd.args(["--no-track", "-b", &branch])
            .arg(&folder)
            .arg(start_point(repo, base)?);
    }
    let out = cmd.output().map_err(CoreError::io("run git in", repo))?;
    if !out.status.success() {
        return Err(CoreError::Git {
            path: repo.to_path_buf(),
            reason: stderr(&out),
        });
    }
    Ok(WorktreeInfo {
        path: folder,
        display_name: new.display_name.unwrap_or_else(|| branch.clone()),
        branch: Some(branch),
        is_main: false,
        issue_link: new.issue_link,
        base_branch: base,
    })
}

// The repo's default branch: where `origin/HEAD` points, then `main`, then `master`, each
// locally or on `origin`.
fn default_base(repo: &Path) -> Result<Option<String>> {
    let head = run(
        repo,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )?;
    if head.status.success()
        && let Some(name) = String::from_utf8_lossy(&head.stdout)
            .trim()
            .strip_prefix("refs/remotes/origin/")
    {
        return Ok(Some(name.to_owned()));
    }
    for name in ["main", "master"] {
        if find_start(repo, name)?.is_some() {
            return Ok(Some(name.to_owned()));
        }
    }
    Ok(None)
}

// The ref a new branch starts from for `base`: `origin/<base>` when it exists, as the merge base
// uses, else the local branch. None when neither exists.
pub(crate) fn find_start(repo: &Path, base: &str) -> Result<Option<String>> {
    for candidate in [
        format!("refs/remotes/origin/{base}"),
        format!("refs/heads/{base}"),
    ] {
        if has_ref(repo, &candidate)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

pub(crate) fn start_point(repo: &Path, base: &str) -> Result<String> {
    find_start(repo, base)?.ok_or_else(|| CoreError::UnknownBranch {
        repo: repo.to_path_buf(),
        name: base.to_owned(),
    })
}

pub(crate) fn has_ref(repo: &Path, full_name: &str) -> Result<bool> {
    let out = run(repo, &["show-ref", "--verify", "--quiet", full_name])?;
    Ok(out.status.success())
}

pub(crate) fn run(dir: &Path, args: &[&str]) -> Result<Output> {
    git(dir)
        .args(args)
        .output()
        .map_err(CoreError::io("run git in", dir))
}

pub(crate) fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd
}

pub(crate) fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_owned()
}
