use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    BranchOutcome, Core, CoreConfig, CoreError, CoreEvent, DeleteCheck, NewReviewComment,
    NewWorktree, ProfileSettings, RemoveOpts, RepoSettings, SessionId, SpawnSpec,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
// Tests keep the home folder, and the socket in it, under /tmp.
fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

fn start(dir: &Path) -> (Core, broadcast::Receiver<CoreEvent>) {
    Core::start(CoreConfig::new(dir.join("home"))).unwrap()
}

// The core owns its runtime, and dropping it inside an async context panics.
// So tests make and drop the core outside, and only await its calls here.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

// Runs git with the user's config turned off, so their hooks or signing settings never apply.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Grove Test")
        .env("GIT_AUTHOR_EMAIL", "test@grove.invalid")
        .env("GIT_COMMITTER_NAME", "Grove Test")
        .env("GIT_COMMITTER_EMAIL", "test@grove.invalid")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

// A repo on `main` with one commit. Returns its real path, as git reports it.
// On macOS /tmp is a link to /private/tmp.
fn new_repo(dir: &Path, name: &str) -> PathBuf {
    let repo = dir.join(name);
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    repo.canonicalize().unwrap()
}

// Adds a linked worktree on a new branch `name` next to the repo. Returns its real path.
fn linked_worktree(repo: &Path, name: &str) -> PathBuf {
    git(
        repo,
        &["worktree", "add", "-q", "-b", name, &format!("../{name}")],
    );
    repo.parent().unwrap().join(name).canonicalize().unwrap()
}

fn delete(confirmed: bool) -> RemoveOpts {
    RemoveOpts::Delete {
        confirmed,
        delete_branch: false,
    }
}

fn has_branch(repo: &Path, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
        .status()
        .unwrap()
        .success()
}

// A shell that has started a background job, so ending it means ending its whole session.
fn spawn_shell(core: &Core, worktree: &Path) -> SessionId {
    core.spawn(
        worktree,
        SpawnSpec {
            command: "/bin/dash".into(),
            args: vec!["-c".into(), "sleep 60 & sleep 60".into()],
            cols: 80,
            rows: 24,
        },
    )
    .unwrap()
}

fn sleeper() -> SpawnSpec {
    SpawnSpec {
        command: "/bin/sleep".into(),
        args: vec!["60".into()],
        cols: 80,
        rows: 24,
    }
}

// The sessions whose `SessionExited` was already sent. It never waits.
fn exited(events: &mut broadcast::Receiver<CoreEvent>) -> Vec<SessionId> {
    let mut exited = Vec::new();
    loop {
        match events.try_recv() {
            Ok(CoreEvent::SessionExited { session, .. }) => exited.push(session),
            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
            Err(_) => return exited,
        }
    }
}

fn listed(core: &Core) -> Vec<PathBuf> {
    block_on(core.list_worktrees())
        .into_iter()
        .map(|w| w.path)
        .collect()
}

// Takes every event already sent, so a check after it sees only new ones.
fn drain(events: &mut broadcast::Receiver<CoreEvent>) {
    loop {
        match events.try_recv() {
            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
            Err(_) => return,
        }
    }
}

// Whether a `DiffChanged` for `worktree` arrives within a second, well past the watcher's
// 150 ms debounce.
fn diff_changed_arrives(events: &mut broadcast::Receiver<CoreEvent>, worktree: &Path) -> bool {
    thread::sleep(Duration::from_secs(1));
    loop {
        match events.try_recv() {
            Ok(CoreEvent::DiffChanged { worktree: w }) if w == worktree => return true,
            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
            Err(_) => return false,
        }
    }
}

#[test]
fn forget_drops_the_worktree_and_leaves_its_folder_and_branch() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    block_on(core.remove_worktree(&feature, RemoveOpts::Forget)).unwrap();

    assert_eq!(listed(&core), vec![repo.clone()]);
    assert!(feature.is_dir());
    assert!(has_branch(&repo, "feature"));
}

#[test]
fn forgetting_or_deleting_the_main_worktree_alone_is_refused() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let forgot = block_on(core.remove_worktree(&repo, RemoveOpts::Forget));
    let deleted = block_on(core.remove_worktree(&repo, delete(true)));

    assert!(
        matches!(forgot, Err(CoreError::MainWorktree(ref p)) if *p == repo),
        "{forgot:?}"
    );
    assert!(
        matches!(deleted, Err(CoreError::MainWorktree(ref p)) if *p == repo),
        "{deleted:?}"
    );
    assert_eq!(listed(&core), vec![repo.clone(), feature]);
    assert!(repo.join(".git").is_dir());
}

#[test]
fn forgetting_a_repo_drops_all_its_worktrees_and_no_other_repo() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let other = new_repo(dir.path(), "other");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_repo(&other)).unwrap();

    block_on(core.remove_worktree(&repo, RemoveOpts::ForgetRepo)).unwrap();

    assert_eq!(listed(&core), vec![other]);
    assert!(repo.is_dir() && feature.is_dir());
}

#[test]
fn forgetting_a_repo_by_a_linked_worktree_is_refused() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let forgot = block_on(core.remove_worktree(&feature, RemoveOpts::ForgetRepo));

    assert!(
        matches!(forgot, Err(CoreError::UnknownRepo(ref p)) if *p == feature),
        "{forgot:?}"
    );
    assert_eq!(listed(&core), vec![repo, feature]);
}

#[test]
fn a_forgotten_worktree_sends_no_diff_changed() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    block_on(core.remove_worktree(&feature, RemoveOpts::Forget)).unwrap();
    drain(&mut events);
    fs::write(feature.join("new.txt"), "new\n").unwrap();

    assert!(!diff_changed_arrives(&mut events, &feature));
}

#[test]
fn delete_removes_a_clean_worktree_and_keeps_its_merged_branch() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let branch = block_on(core.remove_worktree(&feature, delete(false))).unwrap();

    assert_eq!(
        branch,
        BranchOutcome::Kept {
            name: "feature".into(),
            merged: true
        }
    );
    assert_eq!(listed(&core), vec![repo.clone()]);
    assert!(!feature.exists());
    assert!(has_branch(&repo, "feature"));
}

#[test]
fn delete_without_confirmation_refuses_a_dirty_worktree_and_removes_it_with_one() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    fs::write(feature.join("new.txt"), "new\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let refused = block_on(core.remove_worktree(&feature, delete(false)));

    assert!(
        matches!(
            refused,
            Err(CoreError::DeleteNeedsConfirmation {
                ref worktree,
                uncommitted: true,
                unpushed: 0,
            }) if *worktree == feature
        ),
        "{refused:?}"
    );
    assert!(feature.join("new.txt").is_file());
    assert_eq!(listed(&core), vec![repo.clone(), feature.clone()]);

    block_on(core.remove_worktree(&feature, delete(true))).unwrap();

    assert!(!feature.exists());
    assert_eq!(listed(&core), vec![repo]);
}

#[test]
fn delete_without_confirmation_refuses_commits_on_no_remote() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let origin = dir.path().join("origin.git");
    git(dir.path(), &["clone", "-q", "--bare", "repo", "origin.git"]);
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo, &["fetch", "-q", "origin"]);
    let feature = linked_worktree(&repo, "feature");
    git(&feature, &["commit", "-q", "--allow-empty", "-m", "work"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let refused = block_on(core.remove_worktree(&feature, delete(false)));

    assert!(
        matches!(
            refused,
            Err(CoreError::DeleteNeedsConfirmation {
                uncommitted: false,
                unpushed: 1,
                ..
            })
        ),
        "{refused:?}"
    );
    assert!(feature.is_dir());

    // Once pushed, the commit is on a remote and nothing needs confirming.
    git(&feature, &["push", "-q", "origin", "feature"]);
    let branch = block_on(core.remove_worktree(&feature, delete(false))).unwrap();

    assert!(!feature.exists());
    assert_eq!(
        branch,
        BranchOutcome::Kept {
            name: "feature".into(),
            merged: false
        }
    );
}

#[test]
fn delete_check_reports_a_clean_worktree_with_a_merged_branch_and_removes_nothing() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let check = block_on(core.delete_check(&feature)).unwrap();

    assert_eq!(
        check,
        DeleteCheck {
            uncommitted: false,
            unpushed: 0,
            branch: Some("feature".into()),
            merged: true,
        }
    );
    assert!(feature.is_dir());
    assert_eq!(listed(&core), vec![repo, feature]);
}

#[test]
fn delete_check_reports_what_delete_would_lose_as_delete_refuses_it() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    git(&feature, &["commit", "-q", "--allow-empty", "-m", "work"]);
    fs::write(feature.join("new.txt"), "new\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let check = block_on(core.delete_check(&feature)).unwrap();

    assert_eq!(
        check,
        DeleteCheck {
            uncommitted: true,
            unpushed: 1,
            branch: Some("feature".into()),
            merged: false,
        }
    );
    let refused = block_on(core.remove_worktree(&feature, delete(false)));
    assert!(
        matches!(
            refused,
            Err(CoreError::DeleteNeedsConfirmation {
                uncommitted: true,
                unpushed: 1,
                ..
            })
        ),
        "{refused:?}"
    );
}

#[test]
fn delete_check_reports_no_branch_for_a_detached_head() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    git(&repo, &["worktree", "add", "-q", "--detach", "../spike"]);
    let spike = dir.path().join("spike").canonicalize().unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let check = block_on(core.delete_check(&spike)).unwrap();

    assert_eq!(check.branch, None);
    assert!(!check.merged);
}

#[test]
fn delete_check_refuses_a_main_worktree_and_one_grove_does_not_track() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let elsewhere = dir.path().join("elsewhere");

    let main = block_on(core.delete_check(&repo));
    let unknown = block_on(core.delete_check(&elsewhere));

    assert!(
        matches!(main, Err(CoreError::MainWorktree(ref p)) if *p == repo),
        "{main:?}"
    );
    assert!(
        matches!(unknown, Err(CoreError::UnknownWorktree(ref p)) if *p == elsewhere),
        "{unknown:?}"
    );
}

#[test]
fn delete_ends_the_worktrees_sessions_before_it_returns() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let first = spawn_shell(&core, &feature);
    let second = spawn_shell(&core, &feature);
    let elsewhere = spawn_shell(&core, &repo);

    block_on(core.remove_worktree(&feature, delete(false))).unwrap();

    let mut ended = exited(&mut events);
    ended.sort_by_key(ToString::to_string);
    assert_eq!(ended, vec![first, second]);
    assert!(!feature.exists());
    // The main worktree's session still runs.
    core.write(elsewhere, b"").unwrap();
}

#[test]
fn forget_ends_the_worktrees_sessions_before_it_returns() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let session = spawn_shell(&core, &feature);

    block_on(core.remove_worktree(&feature, RemoveOpts::Forget)).unwrap();

    assert_eq!(exited(&mut events), vec![session]);
    assert!(feature.is_dir());
}

#[test]
fn a_deleted_worktree_sends_no_diff_changed() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    drain(&mut events);

    block_on(core.remove_worktree(&feature, delete(false))).unwrap();

    assert!(!diff_changed_arrives(&mut events, &feature));
}

#[test]
fn delete_can_delete_a_merged_branch_too() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let opts = RemoveOpts::Delete {
        confirmed: false,
        delete_branch: true,
    };
    let branch = block_on(core.remove_worktree(&feature, opts)).unwrap();

    assert_eq!(
        branch,
        BranchOutcome::Deleted {
            name: "feature".into()
        }
    );
    assert!(!feature.exists());
    assert!(!has_branch(&repo, "feature"));
}

#[test]
fn delete_refuses_to_delete_an_unmerged_branch_and_removes_nothing() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    git(&feature, &["commit", "-q", "--allow-empty", "-m", "work"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let opts = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: true,
    };
    let refused = block_on(core.remove_worktree(&feature, opts));

    assert!(
        matches!(
            refused,
            Err(CoreError::BranchNotMerged { ref branch, ref base })
                if branch == "feature" && base == "main"
        ),
        "{refused:?}"
    );
    assert!(feature.is_dir());
    assert!(has_branch(&repo, "feature"));
    assert_eq!(listed(&core), vec![repo, feature]);
}

#[test]
fn delete_keeps_a_branch_renamed_in_the_worktree_without_a_merge_check() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    // grove still lists it as `feature`, a branch that no longer exists.
    git(&feature, &["branch", "-m", "renamed"]);

    let branch = block_on(core.remove_worktree(&feature, delete(true))).unwrap();

    assert_eq!(
        branch,
        BranchOutcome::Kept {
            name: "feature".into(),
            merged: false
        }
    );
    assert!(!feature.exists());
    assert!(has_branch(&repo, "renamed"));
}

#[test]
fn no_session_started_during_a_delete_outlives_it() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let (spawned, done) = (AtomicBool::new(false), AtomicBool::new(false));

    let started = thread::scope(|scope| {
        let spawner = scope.spawn(|| {
            let mut started = Vec::new();
            while !done.load(Ordering::SeqCst) {
                // Refused once the worktree is being removed or gone.
                if let Ok(session) = core.spawn(&feature, sleeper()) {
                    started.push(session);
                    spawned.store(true, Ordering::SeqCst);
                }
                thread::sleep(Duration::from_millis(1));
            }
            started
        });
        // Deletes only once sessions are starting, or after 5 s, which the check below fails.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !spawned.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        let deleted = block_on(core.remove_worktree(&feature, delete(true)));
        // Stops the spawner before a failed delete panics, or the scope would wait on it forever.
        done.store(true, Ordering::SeqCst);
        let started = spawner.join().unwrap();
        deleted.unwrap();
        started
    });

    assert!(!started.is_empty());
    let running: Vec<_> = started
        .into_iter()
        .filter(|&s| core.write(s, b"").is_ok())
        .collect();
    assert_eq!(
        running,
        vec![],
        "sessions still running in a deleted worktree"
    );
}

#[test]
fn a_branch_git_cannot_delete_is_reported_after_the_worktree_is_gone() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    // git cannot take the ref's lock, so `git branch -D` fails.
    fs::write(repo.join(".git/refs/heads/feature.lock"), "").unwrap();

    let opts = RemoveOpts::Delete {
        confirmed: false,
        delete_branch: true,
    };
    let branch = block_on(core.remove_worktree(&feature, opts)).unwrap();

    assert!(
        matches!(&branch, BranchOutcome::NotDeleted { name, reason }
            if name == "feature" && reason.contains("lock")),
        "{branch:?}"
    );
    assert!(!feature.exists());
    assert_eq!(listed(&core), vec![repo.clone()]);
    assert!(has_branch(&repo, "feature"));
}

// A repo on `main` whose one commit has `a.txt`, so a comment has a line to go on. Returns its
// real path.
fn repo_with_a_file(dir: &Path) -> PathBuf {
    let repo = new_repo(dir, "repo");
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "a file"]);
    repo
}

fn add_worktree(core: &Core, repo: &Path, branch: &str) -> PathBuf {
    let new = NewWorktree {
        branch: branch.into(),
        ..NewWorktree::default()
    };
    block_on(core.create_worktree(repo, new))
        .unwrap()
        .worktree
        .path
}

fn add_comment(core: &Core, worktree: &Path) {
    let comment = NewReviewComment {
        path: "a.txt".into(),
        start_line: 1,
        end_line: 1,
        body: "rename this".into(),
    };
    block_on(core.add_comment(worktree, comment)).unwrap();
}

// restore.rs checks that they stay gone after a restart.
#[test]
fn deleting_a_worktree_drops_its_comments() {
    let dir = short_temp_dir();
    let repo = repo_with_a_file(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let feature = add_worktree(&core, &repo, "feature");
    add_comment(&core, &feature);

    block_on(core.remove_worktree(&feature, delete(false))).unwrap();
    // The same branch, so the same path.
    assert_eq!(add_worktree(&core, &repo, "feature"), feature);

    assert_eq!(core.comments(&feature).unwrap(), vec![]);
}

#[test]
fn forgetting_a_worktree_or_repo_drops_their_comments() {
    let dir = short_temp_dir();
    let repo = repo_with_a_file(dir.path());
    let feature = linked_worktree(&repo, "feature");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    add_comment(&core, &repo);
    add_comment(&core, &feature);

    block_on(core.remove_worktree(&feature, RemoveOpts::Forget)).unwrap();
    block_on(core.remove_worktree(&repo, RemoveOpts::ForgetRepo)).unwrap();
    // Adding the repo again adopts both worktrees at the same paths.
    block_on(core.add_repo(&repo)).unwrap();

    assert_eq!(listed(&core), vec![repo.clone(), feature.clone()]);
    assert_eq!(core.comments(&repo).unwrap(), vec![]);
    assert_eq!(core.comments(&feature).unwrap(), vec![]);
}

#[test]
fn a_failed_delete_keeps_the_comments() {
    let dir = short_temp_dir();
    let repo = repo_with_a_file(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let settings = RepoSettings {
        overrides: ProfileSettings {
            cleanup: Some("exit 1".into()),
            ..ProfileSettings::default()
        },
        ..RepoSettings::default()
    };
    block_on(core.set_repo_settings(&repo, settings)).unwrap();
    let feature = add_worktree(&core, &repo, "feature");
    add_comment(&core, &feature);

    let deleted = block_on(core.remove_worktree(&feature, delete(false)));

    assert!(
        matches!(deleted, Err(CoreError::CleanupFailed { .. })),
        "{deleted:?}"
    );
    assert_eq!(core.comments(&feature).unwrap().len(), 1);
}
