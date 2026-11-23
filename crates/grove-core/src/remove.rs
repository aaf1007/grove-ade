use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::broadcast::{self, error::RecvError};

use crate::watch::{self, Watch};
use crate::worktree::{find_start, git, has_ref, run, stderr, untrack};
use crate::{Core, CoreError, CoreEvent, Result, SessionId, WorktreeInfo};

// How long removing a worktree or deleting an agent session waits for its killed sessions to
// exit.
pub(crate) const EXIT_WAIT: Duration = Duration::from_secs(5);

/// How `Core::remove_worktree` removes a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOpts {
    /// Stop tracking the worktree and leave the disk alone. Refused for a main worktree.
    Forget,
    /// Forget the repo whose main worktree this is, with every worktree of it.
    ForgetRepo,
    /// End the worktree's sessions, run the repo's cleanup script in it, run
    /// `git worktree remove` and stop tracking it. It checks before it ends anything and again
    /// after the cleanup script, which may commit. A failed check or cleanup stops the delete and
    /// leaves the worktree in place. Refused for a main worktree.
    Delete {
        /// Delete it even with uncommitted changes or commits on no remote.
        confirmed: bool,
        /// Delete its branch too. Refused, before anything is removed, when the branch is not
        /// merged into the base branch.
        delete_branch: bool,
    },
}

/// What deleting a worktree would lose, and whether its branch may go with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteCheck {
    /// Uncommitted changes, untracked files included.
    pub uncommitted: bool,
    /// Commits on no remote and not in the base branch.
    pub unpushed: usize,
    /// None when HEAD is detached.
    pub branch: Option<String>,
    /// The branch is merged into the base branch, so Delete may delete it too.
    pub merged: bool,
}

/// What `Core::remove_worktree` did with the worktree's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchOutcome {
    /// Forget leaves the branch alone, and a detached HEAD has none.
    Untouched,
    /// Delete keeps the branch. Deleting it is offered only when it is merged into the base branch.
    Kept { name: String, merged: bool },
    /// Delete deleted the merged branch, as asked.
    Deleted { name: String },
    /// Delete removed the worktree, but `git branch -D` then failed, so the branch is still there.
    NotDeleted { name: String, reason: String },
}

impl Core {
    /// Removes the worktree at `worktree` as `opts` says. Fails at once with `Removing` while
    /// another call removes it, or for `ForgetRepo`, any worktree of the repo.
    pub async fn remove_worktree(
        &self,
        worktree: &Path,
        opts: RemoveOpts,
    ) -> Result<BranchOutcome> {
        // One lock for both, so a removal in between cannot leave the worktree without a repo.
        let (found, repo) = {
            let tracked = self.tracked();
            let found = tracked.iter().find(|w| w.path == worktree).cloned();
            (found, repo_of(&tracked, worktree))
        };
        match (opts, found) {
            (RemoveOpts::ForgetRepo, Some(main)) if main.is_main => {
                let paths = self.repo_worktrees(worktree);
                self.forget(paths).await?;
                Ok(BranchOutcome::Untouched)
            }
            (RemoveOpts::ForgetRepo, _) => Err(CoreError::UnknownRepo(worktree.to_path_buf())),
            (_, None) => Err(CoreError::UnknownWorktree(worktree.to_path_buf())),
            (_, Some(main)) if main.is_main => Err(CoreError::MainWorktree(main.path)),
            (RemoveOpts::Forget, Some(_)) => {
                self.forget(vec![worktree.to_path_buf()]).await?;
                Ok(BranchOutcome::Untouched)
            }
            (
                RemoveOpts::Delete {
                    confirmed,
                    delete_branch,
                },
                Some(linked),
            ) => {
                let repo =
                    repo.ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?;
                self.delete(linked, repo, confirmed, delete_branch).await
            }
        }
    }

    async fn delete(
        &self,
        worktree: WorktreeInfo,
        repo: PathBuf,
        confirmed: bool,
        delete_branch: bool,
    ) -> Result<BranchOutcome> {
        let path = worktree.path.clone();
        let removing = self.mark_removing(vec![path.clone()])?;
        // Only this one is ended, not one of a worktree added at the same path meanwhile.
        let setup = self.setup(&path).map(|setup| setup.session);
        // Every check comes before anything is ended or removed.
        let (checked, dir) = (worktree.clone(), repo.clone());
        self.blocking(&path, move || {
            check_delete(&checked, &dir, confirmed, delete_branch)
        })
        .await?;

        let cleanup = self.cleanup(&repo);
        self.end_sessions(vec![path.clone()]).await?;
        // The task owns the rest and the mark, so dropping this future cannot leave the worktree
        // removed from disk but still tracked and unmarked.
        let (tracked, settings, watches, comments, agents, events, state_file) = (
            self.worktrees.clone(),
            self.repo_settings.clone(),
            self.watches.clone(),
            self.comments.clone(),
            self.agents.clone(),
            self.events.clone(),
            self.state_file.clone(),
        );
        let warnings = self.warnings.clone();
        let folder = path.clone();
        self.blocking(&path, move || {
            let mut removing = removing;
            // Stop the watcher first, so the removal sends no `DiffChanged`.
            drop(lock(&watches).remove(&folder));
            let removed = (|| {
                // A session may have committed before it ended, so check again.
                let mut branch = check_delete(&worktree, &repo, confirmed, delete_branch)?;
                // With the worktree's sessions ended.
                if let Some(cleanup) = &cleanup {
                    cleanup.run(&folder)?;
                    // So may the cleanup script.
                    branch = check_delete(&worktree, &repo, confirmed, delete_branch)?;
                }
                remove(&repo, &folder, confirmed)?;
                Ok(branch)
            })();
            let branch = match removed {
                Ok(branch) => branch,
                Err(e) => {
                    // Watch it again, as when it was added.
                    let failed = match Watch::start(&folder, events.clone()) {
                        Ok(watch) => {
                            lock(&watches).insert(folder.clone(), watch);
                            None
                        }
                        Err(start) => Some(watch::not_started(&folder, &start)),
                    };
                    // Unmarked first, so a retry finds the worktree.
                    removing.release(|| match failed {
                        Some(warning) => warnings.raise(warning),
                        None => warnings.clear_watcher(&folder),
                    });
                    return Err(e);
                }
            };
            // Before it leaves the list, so a worktree added at the same path keeps its own.
            warnings.clear_watcher(&folder);
            comments.drop_worktree(&folder);
            let dropped = agents.drop_records_in(std::slice::from_ref(&folder));
            // The worktree is gone by now, so a folder that stays is only left behind.
            let _ = agents.remove_session_folders(&dropped.sessions);
            if !dropped.sessions.is_empty() {
                let _ = events.send(CoreEvent::AgentSessionsChanged);
            }
            // A subagent that ran here leaves the workspace of its top parent elsewhere, and
            // one that runs elsewhere may show in another workspace now.
            for session in dropped.workspaces_changed {
                let _ = events.send(CoreEvent::WorkspaceChanged { session, by: None });
            }
            untrack(&mut lock(&tracked), |path| path == folder);
            let outcome = match branch {
                None => BranchOutcome::Untouched,
                Some((name, merged)) if !delete_branch => BranchOutcome::Kept { name, merged },
                Some((name, _)) => {
                    // Checked as merged above. -D skips git's own check, which is against HEAD.
                    // The worktree is gone by now, so a failure here is reported, not raised.
                    match git_ok(&repo, &["branch", "-D", &name]) {
                        Ok(_) => BranchOutcome::Deleted { name },
                        Err(e) => BranchOutcome::NotDeleted {
                            name,
                            reason: e.to_string(),
                        },
                    }
                }
            };
            state_file.save(&tracked, &settings)?;
            Ok(outcome)
        })
        .await
        .inspect(|_| {
            if let Some(setup) = setup {
                self.end_setup(&path, setup);
            }
            self.drop_stopped_runs(|w, _| w == path);
            self.drop_panels_in(&path);
        })
    }

    /// Fails unless `worktree` is tracked and not being removed. A new session checks this under
    /// the lock that `end_sessions` lists sessions with, so none starts unseen.
    pub(crate) fn check_startable(&self, worktree: &Path) -> Result<()> {
        if self.removing().contains(worktree) {
            return Err(CoreError::Removing(worktree.to_path_buf()));
        }
        self.check_tracked(worktree)
    }

    // Marks `worktrees` as being removed until the returned guard drops, even if the removal
    // fails or its future is dropped. Fails, and marks none, when one is marked already.
    fn mark_removing(&self, worktrees: Vec<PathBuf>) -> Result<Removing> {
        let mut removing = self.removing();
        if let Some(taken) = worktrees.iter().find(|w| removing.contains(*w)) {
            return Err(CoreError::Removing(taken.clone()));
        }
        removing.extend(worktrees.iter().cloned());
        drop(removing);
        Ok(Removing {
            marks: self.removing.clone(),
            worktrees,
        })
    }

    fn removing(&self) -> MutexGuard<'_, HashSet<PathBuf>> {
        lock(&self.removing)
    }

    // Kills every session in `worktrees` and waits until their `SessionExited` is sent, so every
    // receiver has it before the worktree goes. Then it waits until every agent session there
    // has ended: the core applies a process exit to its agent session after `SessionExited`,
    // and a record dropped in between would never get its last state. Fails if any of this
    // is not done after EXIT_WAIT.
    async fn end_sessions(&self, worktrees: Vec<PathBuf>) -> Result<()> {
        // Subscribed before listing, so no exit is missed between the two.
        let exits = self.events.subscribe();
        // Asked before any is killed, and it covers one whose process has already exited.
        let agents_ended = self.agents.session_ends_in(&worktrees);
        let left: HashSet<SessionId> = self.sessions.in_worktrees(&worktrees).into_iter().collect();
        for &session in &left {
            // It fails only when the session already exited. Its event still comes.
            let _ = self.kill(session);
        }
        let running = self.sessions.is_running();
        // The caller's runtime may have no timer, so wait on the core's.
        let waited = self
            .runtime
            .spawn(async move {
                let deadline = tokio::time::Instant::now() + EXIT_WAIT;
                let all_ended = async {
                    for ended in agents_ended {
                        // Closed means its record is gone, which leaves nothing to end.
                        let _ = ended.await;
                    }
                };
                wait_for_exits(exits, left, running, EXIT_WAIT).await
                    && tokio::time::timeout_at(deadline, all_ended).await.is_ok()
            })
            .await;
        match waited {
            Ok(true) => Ok(()),
            // A panic in the wait task leaves the same doubt as a timeout.
            Ok(false) | Err(_) => Err(CoreError::SessionsStillRunning(
                worktrees.into_iter().next().unwrap_or_default(),
            )),
        }
    }

    // Runs `f` off the tokio workers. `path` names what it works on if the task panics.
    async fn blocking<T: Send + 'static>(
        &self,
        path: &Path,
        f: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.runtime
            .spawn_blocking(f)
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("run git in", path)(io::Error::other(e)))?
    }

    // The main worktree at `main` and every worktree of its repo, which follow it in the list.
    fn repo_worktrees(&self, main: &Path) -> Vec<PathBuf> {
        let tracked = self.tracked();
        let mut repo = tracked.iter().skip_while(|w| w.path != main);
        let first = repo.next().map(|w| w.path.clone());
        first
            .into_iter()
            .chain(repo.take_while(|w| !w.is_main).map(|w| w.path.clone()))
            .collect()
    }

    // Stops tracking `paths`, stops their watchers and saves the state file. It returns once no
    // watcher can send another `DiffChanged` for them.
    async fn forget(&self, paths: Vec<PathBuf>) -> Result<()> {
        let _removing = self.mark_removing(paths.clone())?;
        self.end_sessions(paths.clone()).await?;
        // Before they leave the list, so a worktree added at the same path keeps its own.
        for path in &paths {
            if let Some(setup) = self.setup(path) {
                self.end_setup(path, setup.session);
            }
            self.drop_stopped_runs(|w, _| w == path);
            self.drop_panels_in(path);
            self.comments.drop_worktree(path);
            self.warnings.clear_watcher(path);
        }
        let dropped = self.agents.drop_records_in(&paths);
        let sessions = dropped.sessions;
        untrack(&mut self.tracked(), |path| paths.iter().any(|p| p == path));
        // Settings are keyed by main worktree, so this drops those of a forgotten repo.
        self.settings().retain(|repo, _| !paths.contains(repo));
        let stopped: Vec<_> = {
            let mut watches = self.watches();
            paths.iter().filter_map(|p| watches.remove(p)).collect()
        };
        // Dropping a watch joins its thread, so do it off this tokio worker.
        let first = paths.first().cloned().unwrap_or_default();
        let (state_file, tracked, settings, agents) = (
            self.state_file.clone(),
            self.worktrees.clone(),
            self.repo_settings.clone(),
            self.agents.clone(),
        );
        if !sessions.is_empty() {
            let _ = self.events.send(CoreEvent::AgentSessionsChanged);
        }
        // A subagent that ran there leaves the workspace of its top parent elsewhere, and one
        // that runs elsewhere may show in another workspace now.
        for owner in dropped.workspaces_changed {
            self.workspace_changed(owner);
        }
        self.blocking(&first, move || {
            drop(stopped);
            // The worktrees are forgotten by now, so a folder that stays is only left behind.
            let _ = agents.remove_session_folders(&sessions);
            state_file.save(&tracked, &settings)
        })
        .await
    }
}

impl Core {
    /// What deleting the worktree at `worktree` would lose and whether its branch is merged, as
    /// Delete finds them. It changes nothing, and Delete checks again. Refused for a main
    /// worktree.
    pub async fn delete_check(&self, worktree: &Path) -> Result<DeleteCheck> {
        let unknown = || CoreError::UnknownWorktree(worktree.to_path_buf());
        let (found, repo) = {
            let tracked = self.tracked();
            let found = tracked.iter().find(|w| w.path == worktree).cloned();
            (found, repo_of(&tracked, worktree))
        };
        let found = found.ok_or_else(unknown)?;
        if found.is_main {
            return Err(CoreError::MainWorktree(found.path));
        }
        let repo = repo.ok_or_else(unknown)?;
        self.blocking(worktree, move || {
            let (uncommitted, unpushed) = unsaved(&found)?;
            // As when Delete keeps a branch, a failed check means not merged.
            let merged = found.branch.as_deref().is_some_and(|branch| {
                merged(&repo, branch, found.base_branch.as_deref()).unwrap_or(false)
            });
            Ok(DeleteCheck {
                uncommitted,
                unpushed,
                branch: found.branch,
                merged,
            })
        })
        .await
    }
}

struct Removing {
    marks: Arc<Mutex<HashSet<PathBuf>>>,
    worktrees: Vec<PathBuf>,
}

impl Removing {
    // Publish the outcome before a competing retry or removal can take the mark.
    fn release(&mut self, publish: impl FnOnce()) {
        let mut removing = lock(&self.marks);
        for worktree in self.worktrees.drain(..) {
            removing.remove(&worktree);
        }
        publish();
    }
}

impl Drop for Removing {
    fn drop(&mut self) {
        self.release(|| {});
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// The main worktree of the repo `worktree` is in, or None when `worktree` is not tracked. The
// tracked list goes repo by repo.
pub(crate) fn repo_of(tracked: &[WorktreeInfo], worktree: &Path) -> Option<PathBuf> {
    let i = tracked.iter().position(|w| w.path == worktree)?;
    tracked[..=i]
        .iter()
        .rev()
        .find(|w| w.is_main)
        .map(|w| w.path.clone())
}

// Whether every session in `left` sent `SessionExited` within `within`. A receiver that fell
// behind may have missed some, so it then stops waiting for those `running` says have ended.
pub(crate) async fn wait_for_exits(
    mut exits: broadcast::Receiver<CoreEvent>,
    mut left: HashSet<SessionId>,
    running: impl Fn(SessionId) -> bool,
    within: Duration,
) -> bool {
    let all_exited = async {
        while !left.is_empty() {
            match exits.recv().await {
                Ok(CoreEvent::SessionExited { session, .. }) => {
                    left.remove(&session);
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => left.retain(|&session| running(session)),
                // The core owns the sender, so it is stopping.
                Err(RecvError::Closed) => return false,
            }
        }
        true
    };
    tokio::time::timeout(within, all_exited)
        .await
        .unwrap_or(false)
}

// Whether `branch` is merged into `base`, as the merge base sees it: `origin/<base>` when it
// exists. It blocks while git runs.
fn merged(repo: &Path, branch: &str, base: Option<&str>) -> Result<bool> {
    let Some(base) = base.map(|b| find_start(repo, b)).transpose()?.flatten() else {
        return Ok(false);
    };
    let out = run(
        repo,
        &[
            "merge-base",
            "--is-ancestor",
            &format!("refs/heads/{branch}"),
            &base,
        ],
    )?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(CoreError::Git {
            path: repo.to_path_buf(),
            reason: stderr(&out),
        }),
    }
}

// Refuses a Delete that would lose work it was not `confirmed` for, or delete a branch not merged
// into the base branch. Returns the branch, if any, and whether it is merged. It blocks while git
// runs.
fn check_delete(
    worktree: &WorktreeInfo,
    repo: &Path,
    confirmed: bool,
    delete_branch: bool,
) -> Result<Option<(String, bool)>> {
    if !confirmed {
        check_saved(worktree)?;
    }
    let Some(name) = &worktree.branch else {
        return Ok(None);
    };
    let base = worktree.base_branch.as_deref();
    let merged = match merged(repo, name, base) {
        Ok(merged) => merged,
        Err(e) if delete_branch => return Err(e),
        // Only reported for a branch that is kept, as when it was renamed in the worktree. A
        // failed check means not merged.
        Err(_) => false,
    };
    if delete_branch && !merged {
        return Err(match base {
            Some(base) => CoreError::BranchNotMerged {
                branch: name.clone(),
                base: base.to_owned(),
            },
            None => CoreError::NoBaseBranch(repo.to_path_buf()),
        });
    }
    Ok(Some((name.clone(), merged)))
}

// Refuses a worktree with uncommitted changes, untracked files included, or with commits that
// are on no remote and not in its base branch. It blocks while git runs.
fn check_saved(worktree: &WorktreeInfo) -> Result<()> {
    let (uncommitted, unpushed) = unsaved(worktree)?;
    if !uncommitted && unpushed == 0 {
        return Ok(());
    }
    Err(CoreError::DeleteNeedsConfirmation {
        worktree: worktree.path.clone(),
        uncommitted,
        unpushed,
    })
}

// Whether the worktree has uncommitted changes, and how many of its commits are on no remote
// and not in its base branch. It blocks while git runs.
fn unsaved(worktree: &WorktreeInfo) -> Result<(bool, usize)> {
    let path = &worktree.path;
    let status = git_ok(path, &["status", "--porcelain"])?;
    let mut ahead = vec!["rev-list", "--count", "HEAD", "--not", "--remotes"];
    // Its own new commits count, not the ones it shares with the local base branch.
    let base = worktree
        .base_branch
        .as_ref()
        .map(|b| format!("refs/heads/{b}"));
    if let Some(base) = &base
        && has_ref(path, base)?
    {
        ahead.push(base);
    }
    let count = git_ok(path, &ahead)?;
    let unpushed = count.trim().parse().map_err(|_| CoreError::Git {
        path: path.clone(),
        reason: format!("rev-list printed {count:?}, not a count"),
    })?;
    Ok((!status.is_empty(), unpushed))
}

// git's stdout, or an error with its stderr. It blocks while git runs.
fn git_ok(dir: &Path, args: &[&str]) -> Result<String> {
    let out = run(dir, args)?;
    if !out.status.success() {
        return Err(CoreError::Git {
            path: dir.to_path_buf(),
            reason: stderr(&out),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// Runs `git worktree remove` from the repo's main worktree. `force` removes it with uncommitted
// changes. It blocks while git runs.
fn remove(repo: &Path, worktree: &Path, force: bool) -> Result<()> {
    let mut cmd = git(repo);
    cmd.args(["worktree", "remove"]).arg(worktree);
    if force {
        cmd.arg("--force");
    }
    let out = cmd.output().map_err(CoreError::io("run git in", repo))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(CoreError::Git {
            path: worktree.to_path_buf(),
            reason: stderr(&out),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn a_worktree_forgotten_meanwhile_has_no_repo() {
        // A ForgetRepo can empty the list between finding a worktree and finding its repo.
        assert_eq!(repo_of(&[], Path::new("/repo/feature")), None);
    }

    fn noise() -> CoreEvent {
        CoreEvent::DiffChanged {
            worktree: PathBuf::from("/elsewhere"),
        }
    }

    #[test]
    fn a_receiver_that_fell_behind_stops_waiting_for_sessions_no_longer_running() {
        let (tx, rx) = broadcast::channel(2);
        // Its SessionExited is among the events the receiver misses.
        tx.send(CoreEvent::SessionExited {
            session: SessionId(1),
            code: None,
        })
        .unwrap();
        for _ in 0..4 {
            tx.send(noise()).unwrap();
        }
        let left = HashSet::from([SessionId(1)]);

        let all_exited = block_on(wait_for_exits(rx, left, |_| false, Duration::from_secs(5)));

        assert!(all_exited);
    }

    #[test]
    fn a_receiver_that_fell_behind_keeps_waiting_for_sessions_still_running() {
        let (tx, rx) = broadcast::channel(2);
        for _ in 0..4 {
            tx.send(noise()).unwrap();
        }
        let left = HashSet::from([SessionId(1)]);

        let all_exited = block_on(wait_for_exits(
            rx,
            left,
            |_| true,
            Duration::from_millis(50),
        ));

        assert!(!all_exited);
    }
}
