use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Core, CoreConfig, CoreError, CoreEvent, DiffBase, NewWorktree, ProfileSettings, RemoveOpts,
    RepoSettings, Warning,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

// Liveness and Commit liveness in docs/acceptance.md.
const LIVENESS: Duration = Duration::from_millis(500);

// Every test here counts watcher threads and times events, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

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
fn new_repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    repo.canonicalize().unwrap()
}

// Adds a linked worktree on a new branch `feature` next to the repo. Returns its real path.
fn linked_worktree(repo: &Path) -> PathBuf {
    git(
        repo,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    repo.parent()
        .unwrap()
        .join("feature")
        .canonicalize()
        .unwrap()
}

// Waits for the next `DiffChanged` for `worktree` and returns how long it took.
// Panics after `within`.
fn wait_diff_changed(
    events: &mut broadcast::Receiver<CoreEvent>,
    worktree: &Path,
    within: Duration,
) -> Duration {
    let start = Instant::now();
    loop {
        match events.try_recv() {
            Ok(CoreEvent::DiffChanged { worktree: w }) if w == worktree => {
                return start.elapsed();
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(
                    start.elapsed() < within,
                    "no DiffChanged for {} within {within:?}",
                    worktree.display()
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

// Counts the `DiffChanged` events for `worktree` sent during the next `window`.
fn count_diff_changed(
    events: &mut broadcast::Receiver<CoreEvent>,
    worktree: &Path,
    window: Duration,
) -> usize {
    let end = Instant::now() + window;
    let mut count = 0;
    while Instant::now() < end {
        match events.try_recv() {
            Ok(CoreEvent::DiffChanged { worktree: w }) if w == worktree => count += 1,
            Ok(_) => {}
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("{e}"),
        }
    }
    count
}

// Drops every event already sent.
fn drain(events: &mut broadcast::Receiver<CoreEvent>) {
    loop {
        match events.try_recv() {
            Ok(_) => {}
            Err(TryRecvError::Empty) => return,
            Err(e) => panic!("{e}"),
        }
    }
}

// The `WatcherNotStarted` warnings sent during the next `window`, as (worktree, reason).
fn watcher_warnings(
    events: &mut broadcast::Receiver<CoreEvent>,
    window: Duration,
) -> Vec<(PathBuf, String)> {
    let end = Instant::now() + window;
    let mut warnings = Vec::new();
    while Instant::now() < end {
        match events.try_recv() {
            Ok(CoreEvent::Warning(Warning::WatcherNotStarted { worktree, reason })) => {
                warnings.push((worktree, reason));
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("{e}"),
        }
    }
    warnings
}

// Moves the worktree's `.git` file out, so git2 cannot open it and its watcher cannot start.
// Returns where it went, for `put_back`.
fn hide_git_file(worktree: &Path, dir: &Path) -> PathBuf {
    let away = dir.join("hidden.git");
    fs::rename(worktree.join(".git"), &away).unwrap();
    away
}

fn put_back(away: &Path, worktree: &Path) {
    fs::rename(away, worktree.join(".git")).unwrap();
}

// A core restored with the repo and its linked worktree tracked, the linked one with its `.git`
// file hidden, so its watcher failed to start. Returns where the file went.
fn restored_with_an_unwatched_worktree(
    dir: &Path,
    repo: &Path,
    linked: &Path,
) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf) {
    let (core, _events) = start(dir);
    block_on(core.add_repo(repo)).unwrap();
    drop(core);
    let away = hide_git_file(linked, dir);
    let (core, events) = start(dir);
    (core, events, away)
}

// Asserts one warning for `worktree` names it and git2's cause, and none for anything else.
#[track_caller]
fn assert_one_warning(warnings: &[(PathBuf, String)], worktree: &Path) {
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let (warned, reason) = &warnings[0];
    assert_eq!(warned, worktree);
    let error = format!("git2 failed in {}: ", worktree.display());
    assert!(reason.starts_with(&error), "{reason}");
    assert!(reason.len() > error.len(), "no cause in {reason}");
}

fn eventually(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

// grove's watcher threads and the `notify` threads under them, by name.
fn watch_threads() -> usize {
    thread_names()
        .iter()
        .filter(|name| name.starts_with("grove-watch") || name.starts_with("notify-rs"))
        .count()
}

// Each watcher runs a `grove-watch` thread and one `notify` thread.
const THREADS_PER_WATCHER: usize = 2;

// Waits until exactly `watchers` watchers run. A thread gets its name only once it runs, so a
// count read right after a watcher starts can come up short.
#[track_caller]
fn wait_for_watchers(watchers: usize) {
    let threads = watchers * THREADS_PER_WATCHER;
    assert!(
        eventually(|| watch_threads() == threads),
        "want {threads} watcher threads: {:?}",
        thread_names()
    );
}

#[cfg(target_os = "linux")]
fn thread_names() -> Vec<String> {
    fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|task| fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .map(|name| name.trim().to_owned())
        .collect()
}

#[cfg(target_os = "macos")]
fn thread_names() -> Vec<String> {
    use std::ffi::{CStr, c_int, c_void};
    use std::mem::size_of;

    // From <sys/proc_info.h>. The libc crate does not have it.
    const PROC_PIDLISTTHREADS: c_int = 6;
    let pid = std::process::id() as c_int;
    let mut ids = vec![0u64; 1024];
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDLISTTHREADS,
            0,
            ids.as_mut_ptr().cast::<c_void>(),
            (ids.len() * size_of::<u64>()) as c_int,
        )
    };
    assert!(bytes > 0, "could not list threads");
    ids.truncate(bytes as usize / size_of::<u64>());
    ids.into_iter()
        .filter_map(|id| {
            let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
            let size = size_of::<libc::proc_threadinfo>() as c_int;
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    id,
                    (&raw mut info).cast::<c_void>(),
                    size,
                )
            };
            (got == size).then(|| {
                unsafe { CStr::from_ptr(info.pth_name.as_ptr()) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect()
}

#[test]
fn a_save_sends_one_diff_changed_within_500_ms() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    fs::write(repo.join("a.txt"), "two\n").unwrap();

    let took = wait_diff_changed(&mut events, &repo, LIVENESS);
    assert_eq!(
        count_diff_changed(&mut events, &repo, Duration::from_secs(1)),
        0,
        "the first came after {took:?}"
    );
}

#[test]
fn git_add_in_a_linked_worktree_sends_diff_changed_within_500_ms() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    fs::write(linked.join("b.txt"), "new\n").unwrap();
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    // Only the linked worktree's index changes.
    git(&linked, &["add", "b.txt"]);

    wait_diff_changed(&mut events, &linked, LIVENESS);
}

#[test]
fn a_commit_in_a_linked_worktree_sends_diff_changed_within_500_ms() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    fs::write(linked.join("b.txt"), "new\n").unwrap();
    git(&linked, &["add", "b.txt"]);
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    git(&linked, &["commit", "-q", "-m", "second"]);

    wait_diff_changed(&mut events, &linked, LIVENESS);
}

#[test]
fn a_repo_with_a_prunable_worktree_is_added_and_its_other_worktrees_watched() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    // Deleted without `git worktree prune`, so `git worktree list` still has it.
    fs::remove_dir_all(&linked).unwrap();
    let (core, mut events) = start(dir.path());

    block_on(core.add_repo(&repo)).unwrap();

    let paths: Vec<_> = block_on(core.list_worktrees())
        .into_iter()
        .map(|w| w.path)
        .collect();
    assert_eq!(paths, vec![repo.clone(), linked]);
    fs::write(repo.join("a.txt"), "two\n").unwrap();
    wait_diff_changed(&mut events, &repo, LIVENESS);
}

#[test]
fn a_created_worktree_whose_watcher_cannot_start_is_still_tracked_with_one_warning() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    // git runs this in the new worktree. Without its `.git` file git2 cannot open it to watch.
    let hook = repo.join(".git/hooks/post-checkout");
    fs::write(&hook, "#!/bin/sh\nrm -f .git\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let new = NewWorktree {
        branch: "topic".into(),
        ..NewWorktree::default()
    };

    let added = block_on(core.create_worktree(&repo, new)).unwrap().worktree;

    assert!(!added.path.join(".git").exists());
    assert!(block_on(core.list_worktrees()).contains(&added));
    assert_one_warning(&watcher_warnings(&mut events, LIVENESS), &added.path);
}

#[test]
fn a_restored_worktree_whose_watcher_cannot_start_sends_one_warning() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);

    let (_core, mut events, _away) =
        restored_with_an_unwatched_worktree(dir.path(), &repo, &linked);

    assert_one_warning(&watcher_warnings(&mut events, LIVENESS), &linked);
}

#[test]
fn adding_a_repo_with_a_worktree_that_cannot_be_watched_sends_one_warning() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    hide_git_file(&linked, dir.path());
    let (core, mut events) = start(dir.path());

    block_on(core.add_repo(&repo)).unwrap();

    assert_one_warning(&watcher_warnings(&mut events, LIVENESS), &linked);
}

#[test]
fn adding_a_repo_twice_sends_no_second_warning() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    hide_git_file(&linked, dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    drain(&mut events);

    assert!(block_on(core.add_repo(&repo)).is_err());

    assert_eq!(watcher_warnings(&mut events, LIVENESS), vec![]);
}

#[test]
fn a_retry_once_the_cause_is_gone_watches_the_worktree() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    let (core, mut events, away) = restored_with_an_unwatched_worktree(dir.path(), &repo, &linked);
    put_back(&away, &linked);

    block_on(core.retry_watcher(&linked)).unwrap();

    // The diff may have changed while nothing watched it.
    wait_diff_changed(&mut events, &linked, LIVENESS);
    fs::write(linked.join("a.txt"), "two\n").unwrap();
    wait_diff_changed(&mut events, &linked, LIVENESS);
}

#[test]
fn a_failed_retry_returns_its_error_and_sends_no_warning() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    let (core, mut events, _away) = restored_with_an_unwatched_worktree(dir.path(), &repo, &linked);
    drain(&mut events);

    let failed = block_on(core.retry_watcher(&linked));

    assert!(
        matches!(&failed, Err(CoreError::Git2 { path, .. }) if *path == linked),
        "{failed:?}"
    );
    assert_eq!(watcher_warnings(&mut events, LIVENESS), vec![]);
}

#[test]
fn a_retry_of_a_watched_worktree_starts_no_second_watcher() {
    let _serial = serial();
    wait_for_watchers(0);
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    // The profiles folder's and the worktree's.
    wait_for_watchers(2);

    block_on(core.retry_watcher(&repo)).unwrap();

    fs::write(repo.join("a.txt"), "two\n").unwrap();
    // Two watchers would send two.
    assert_eq!(
        count_diff_changed(&mut events, &repo, Duration::from_secs(1)),
        1
    );
    wait_for_watchers(2);
}

#[test]
fn two_retries_at_once_start_one_watcher() {
    let _serial = serial();
    wait_for_watchers(0);
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    let (core, mut events, away) = restored_with_an_unwatched_worktree(dir.path(), &repo, &linked);
    put_back(&away, &linked);

    thread::scope(|scope| {
        let retries = [(); 2].map(|()| scope.spawn(|| block_on(core.retry_watcher(&linked))));
        for retry in retries {
            retry.join().unwrap().unwrap();
        }
    });

    // The profiles folder's, the main worktree's and the linked worktree's.
    wait_for_watchers(3);
    wait_diff_changed(&mut events, &linked, LIVENESS);
    drain(&mut events);
    fs::write(linked.join("a.txt"), "two\n").unwrap();
    assert_eq!(
        count_diff_changed(&mut events, &linked, Duration::from_secs(1)),
        1
    );
}

#[test]
fn a_retry_of_an_untracked_worktree_fails() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());

    let failed = block_on(core.retry_watcher(&repo));

    assert!(
        matches!(&failed, Err(CoreError::UnknownWorktree(path)) if *path == repo),
        "{failed:?}"
    );
}

#[test]
fn a_worktree_grove_creates_is_watched() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let new = NewWorktree {
        branch: "topic".into(),
        ..NewWorktree::default()
    };
    let added = block_on(core.create_worktree(&repo, new)).unwrap().worktree;

    fs::write(added.path.join("a.txt"), "two\n").unwrap();

    wait_diff_changed(&mut events, &added.path, LIVENESS);
}

#[test]
fn a_burst_of_100_saves_sends_a_few_events() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    for i in 0..100 {
        fs::write(repo.join(format!("{}.txt", i % 10)), i.to_string()).unwrap();
    }

    let sent = count_diff_changed(&mut events, &repo, Duration::from_secs(1));
    assert!((1..=5).contains(&sent), "{sent} events");
}

#[test]
fn a_folder_made_after_start_is_watched() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    // The inner folders can exist before the watcher sees the outer one.
    fs::create_dir_all(repo.join("new/inner/deep")).unwrap();
    wait_diff_changed(&mut events, &repo, LIVENESS);
    assert_eq!(count_diff_changed(&mut events, &repo, LIVENESS), 0);
    fs::write(repo.join("new/inner/deep/b.txt"), "new\n").unwrap();

    wait_diff_changed(&mut events, &repo, LIVENESS);
}

#[test]
fn a_folder_moved_in_after_start_is_watched() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    fs::create_dir_all(dir.path().join("outside/inner")).unwrap();
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    fs::rename(dir.path().join("outside"), repo.join("moved")).unwrap();
    wait_diff_changed(&mut events, &repo, LIVENESS);
    assert_eq!(count_diff_changed(&mut events, &repo, LIVENESS), 0);
    fs::write(repo.join("moved/inner/b.txt"), "new\n").unwrap();

    wait_diff_changed(&mut events, &repo, LIVENESS);
}

#[test]
fn a_folder_that_cannot_be_read_leaves_the_rest_of_the_worktree_watched() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let locked = repo.join("locked");
    fs::create_dir_all(locked.join("inner")).unwrap();
    fs::create_dir(repo.join("open")).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    fs::write(repo.join("open/b.txt"), "new\n").unwrap();

    let sent = count_diff_changed(&mut events, &repo, LIVENESS);
    // So the temp folder can be removed.
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(sent, 1);
}

#[test]
fn a_worktree_whose_path_goes_through_a_link_is_watched() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    // grove makes its worktrees under `home/worktrees/<repo>`, here a link to another folder.
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::create_dir_all(dir.path().join("home/worktrees")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.path().join("home/worktrees/repo")).unwrap();
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let new = NewWorktree {
        branch: "topic".into(),
        ..NewWorktree::default()
    };
    let added = block_on(core.create_worktree(&repo, new)).unwrap().worktree;
    assert_ne!(added.path, added.path.canonicalize().unwrap());

    fs::write(added.path.join("a.txt"), "two\n").unwrap();

    wait_diff_changed(&mut events, &added.path, LIVENESS);
}

#[test]
fn index_ref_and_head_changes_in_the_main_worktree_send_diff_changed() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    fs::write(repo.join("a.txt"), "two\n").unwrap();
    git(&repo, &["commit", "-q", "-a", "-m", "second"]);
    git(&repo, &["branch", "other"]);
    fs::write(repo.join("b.txt"), "new\n").unwrap();
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    // Each changes only the index, a ref, or HEAD.
    for args in [
        &["add", "b.txt"][..],
        &["update-ref", "refs/heads/main", "HEAD~1"],
        &["symbolic-ref", "HEAD", "refs/heads/other"],
    ] {
        git(&repo, args);
        wait_diff_changed(&mut events, &repo, LIVENESS);
        assert_eq!(count_diff_changed(&mut events, &repo, LIVENESS), 0);
    }
}

// inotify watches one folder each, so a watch inside a skipped folder costs one of the user's
// limited watches for nothing.
#[cfg(target_os = "linux")]
#[test]
fn on_linux_no_folder_is_watched_inside_skipped_ones() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    for folder in ["node_modules/pkg", "target/debug", "sub/node_modules/pkg"] {
        fs::create_dir_all(repo.join(folder)).unwrap();
    }
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    for folder in [
        "node_modules/later",
        "target/release",
        "new/target/debug",
        "new/inner",
    ] {
        fs::create_dir_all(repo.join(folder)).unwrap();
    }

    // The worktree's folders, and the git folders HEAD, index and refs are in.
    for folder in ["", "sub", ".git", ".git/refs", ".git/refs/heads"] {
        assert!(is_watched(&repo.join(folder)), "{folder:?} is not watched");
    }
    assert!(
        eventually(|| is_watched(&repo.join("new/inner"))),
        "new/inner is not watched"
    );
    for folder in [
        "node_modules",
        "node_modules/pkg",
        "node_modules/later",
        "target",
        "target/debug",
        "target/release",
        "sub/node_modules",
        "sub/node_modules/pkg",
        "new/target",
        "new/target/debug",
        ".git/objects",
        ".git/hooks",
        ".git/logs",
    ] {
        assert!(!is_watched(&repo.join(folder)), "{folder:?} is watched");
    }
}

// Whether any inotify instance in this process watches `path`. Tests run one at a time, and a
// watch on a deleted folder goes away with it, so an inode match is this test's own watch.
#[cfg(target_os = "linux")]
fn is_watched(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let inode = fs::metadata(path).unwrap().ino();
    // A line per watch: `inotify wd:1 ino:1a2b sdev:... mask:...`, the inode in hex.
    fs::read_dir("/proc/self/fdinfo")
        .unwrap()
        .filter_map(|fd| fs::read_to_string(fd.ok()?.path()).ok())
        .flat_map(|info| {
            info.lines()
                .filter(|line| line.starts_with("inotify wd:"))
                .filter_map(|line| line.split_whitespace().find_map(|f| f.strip_prefix("ino:")))
                .map(|hex| u64::from_str_radix(hex, 16).unwrap())
                .collect::<Vec<_>>()
        })
        .any(|watched| watched == inode)
}

#[test]
fn writes_under_node_modules_or_target_send_nothing() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    // Making `sub` itself is a change.
    fs::create_dir(repo.join("sub")).unwrap();
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    for folder in ["node_modules/pkg", "target/debug", "sub/node_modules"] {
        fs::create_dir_all(repo.join(folder)).unwrap();
        fs::write(repo.join(folder).join("out"), "x").unwrap();
    }

    assert_eq!(
        count_diff_changed(&mut events, &repo, Duration::from_secs(1)),
        0
    );
}

#[test]
fn computing_a_diff_sends_nothing() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    fs::write(repo.join("a.txt"), "two\n").unwrap();
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    // The UI asks for a diff on every DiffChanged, so reading one must not send another.
    for base in [DiffBase::Head, DiffBase::MergeBase] {
        assert!(!block_on(core.diff(&repo, base)).unwrap().is_empty());
    }

    assert_eq!(
        count_diff_changed(&mut events, &repo, Duration::from_secs(1)),
        0
    );
}

#[test]
fn removing_a_worktree_folder_stops_its_watcher() {
    let _serial = serial();
    // Threads from an earlier test may still be shutting down.
    wait_for_watchers(0);
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let linked = linked_worktree(&repo);
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    // The profiles folder's, the main worktree's and the linked worktree's.
    wait_for_watchers(3);

    git(
        &repo,
        &["worktree", "remove", "--force", &linked.to_string_lossy()],
    );

    wait_for_watchers(2);
    // Events the watcher sent while git was removing the folder.
    drain(&mut events);
    fs::create_dir(&linked).unwrap();
    fs::write(linked.join("a.txt"), "back\n").unwrap();
    assert_eq!(count_diff_changed(&mut events, &linked, LIVENESS), 0);
    // The main worktree is still watched.
    fs::write(repo.join("a.txt"), "two\n").unwrap();
    wait_diff_changed(&mut events, &repo, LIVENESS);
}

#[test]
fn adding_a_repo_twice_keeps_one_watcher_per_worktree() {
    let _serial = serial();
    wait_for_watchers(0);
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    // The profiles folder's and the worktree's.
    wait_for_watchers(2);

    assert!(block_on(core.add_repo(&repo)).is_err());

    // The second call's watchers stop.
    wait_for_watchers(2);
    fs::write(repo.join("a.txt"), "two\n").unwrap();
    wait_diff_changed(&mut events, &repo, LIVENESS);
    assert_eq!(
        count_diff_changed(&mut events, &repo, Duration::from_secs(1)),
        0
    );
    // The first check can run before the second call's threads have their names. By now they do.
    wait_for_watchers(2);
}

#[test]
fn dropping_the_core_stops_every_watcher() {
    let _serial = serial();
    wait_for_watchers(0);
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    linked_worktree(&repo);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    // The profiles folder's, the main worktree's and the linked worktree's.
    wait_for_watchers(3);

    drop(core);

    wait_for_watchers(0);
}

// Gives the repo a cleanup script, which a Delete runs in the worktree.
fn set_cleanup(core: &Core, repo: &Path, script: String) {
    let settings = RepoSettings {
        overrides: ProfileSettings {
            cleanup: Some(script),
            ..ProfileSettings::default()
        },
        ..RepoSettings::default()
    };
    block_on(core.set_repo_settings(repo, settings)).unwrap();
}

fn add_topic(core: &Core, repo: &Path) -> PathBuf {
    let new = NewWorktree {
        branch: "topic".into(),
        ..NewWorktree::default()
    };
    block_on(core.create_worktree(repo, new))
        .unwrap()
        .worktree
        .path
}

const DELETE: RemoveOpts = RemoveOpts::Delete {
    confirmed: true,
    delete_branch: false,
};

#[test]
fn a_retry_while_the_worktree_is_deleted_fails_and_starts_no_watcher() {
    let _serial = serial();
    wait_for_watchers(0);
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let topic = add_topic(&core, &repo);
    // The cleanup script runs after Delete stopped the watcher, and waits until told to go on.
    let (started, go) = (dir.path().join("started"), dir.path().join("go"));
    let script = format!(
        "touch '{}'; while [ ! -e '{}' ]; do sleep 0.05; done",
        started.display(),
        go.display()
    );
    set_cleanup(&core, &repo, script);

    thread::scope(|scope| {
        let deleting = scope.spawn(|| block_on(core.remove_worktree(&topic, DELETE)));
        assert!(eventually(|| started.exists()), "the cleanup never started");

        let retried = block_on(core.retry_watcher(&topic));

        fs::write(&go, "").unwrap();
        deleting.join().unwrap().unwrap();
        assert!(
            matches!(&retried, Err(CoreError::Removing(path)) if *path == topic),
            "{retried:?}"
        );
    });
    // The profiles folder's and the main worktree's.
    wait_for_watchers(2);
}

#[test]
fn a_failed_delete_whose_watcher_cannot_start_again_sends_one_warning() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let topic = add_topic(&core, &repo);
    // Without its `.git` file git2 cannot open the worktree to watch it again.
    set_cleanup(&core, &repo, "rm -f .git; exit 1".into());
    drain(&mut events);

    let deleted = block_on(core.remove_worktree(&topic, DELETE));

    assert!(
        matches!(deleted, Err(CoreError::CleanupFailed { .. })),
        "{deleted:?}"
    );
    assert_one_warning(&watcher_warnings(&mut events, LIVENESS), &topic);
    // Not marked as being removed any more, so a retry gets as far as starting the watcher.
    let retried = block_on(core.retry_watcher(&topic));
    assert!(
        matches!(&retried, Err(CoreError::Git2 { path, .. }) if *path == topic),
        "{retried:?}"
    );
}
