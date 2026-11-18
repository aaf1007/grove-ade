use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::event::{CreateKind, ModifyKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;

use crate::error::with_causes;
use crate::{CoreError, CoreEvent, Result, Warning};

// Changes this close together send one event.
const DEBOUNCE: Duration = Duration::from_millis(150);

// Folders in a worktree whose changes are ignored: git's own, dependencies and build output.
const SKIPPED: [&str; 3] = [".git", "node_modules", "target"];

// inotify watches one folder per watch, so a recursive watch would enter the skipped folders too
// and can use up the user's watches. On Linux each of the worktree's own folders gets a watch.
// macOS watches the whole tree with one FSEvents stream.
const EACH_FOLDER: bool = cfg!(target_os = "linux");

/// Watches a worktree or a folder on its own thread. Dropping it stops it and waits for its
/// thread.
pub(crate) struct Watch {
    stop: mpsc::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

enum Msg {
    Changed,
    // A folder made or moved in, which may already hold folders.
    Folder(PathBuf),
    Stop,
}

impl Watch {
    /// Sends `DiffChanged` when the worktree's files, index, HEAD or refs change. It stops by
    /// itself when the folder goes away. It blocks while git2 opens the repo and notify walks the
    /// folder.
    pub(crate) fn start(worktree: &Path, events: broadcast::Sender<CoreEvent>) -> Result<Watch> {
        let paths = Paths::of(worktree)?;
        let watched = paths.watched();
        // `follow` takes real paths, so the walk starts from the real one.
        let real = paths.worktree.clone();
        let followed = paths.clone();
        let follow = move |p: &Path| EACH_FOLDER && followed.in_files(p);
        let path = worktree.to_path_buf();
        let burst = move || {
            // `git worktree remove`, or anything else, took the folder away.
            if !path.exists() {
                return false;
            }
            let _ = events.send(CoreEvent::DiffChanged {
                worktree: path.clone(),
            });
            true
        };
        Watch::spawn(&real, watched, move |p| paths.relevant(p), follow, burst)
    }

    /// Calls `changed` on the watch's thread after each burst of changes to the entries of
    /// `folder`.
    pub(crate) fn folder(
        folder: &Path,
        mut changed: impl FnMut() + Send + 'static,
    ) -> Result<Watch> {
        let watched = vec![(folder.to_path_buf(), RecursiveMode::NonRecursive)];
        let burst = move || {
            changed();
            true
        };
        Watch::spawn(folder, watched, |_| true, |_| false, burst)
    }

    // Calls `burst` DEBOUNCE after the first relevant change of each burst, until it returns
    // false. Folders under `root` that `follow` takes get a watch each, now and when made later.
    fn spawn(
        root: &Path,
        watched: Vec<(PathBuf, RecursiveMode)>,
        relevant: impl Fn(&Path) -> bool + Send + 'static,
        follow: impl Fn(&Path) -> bool + Send + Sync + 'static,
        burst: impl FnMut() -> bool + Send + 'static,
    ) -> Result<Watch> {
        let (tx, rx) = mpsc::channel();
        let changed = tx.clone();
        let follow = Arc::new(follow);
        let followed = Arc::clone(&follow);
        // notify calls this on its own thread. It only filters and forwards.
        let handler = move |event: notify::Result<Event>| {
            // An error says nothing about the files.
            let Ok(event) = event else { return };
            if matches!(
                event.kind,
                EventKind::Create(CreateKind::Folder) | EventKind::Modify(ModifyKind::Name(_))
            ) {
                for path in event.paths.iter().filter(|p| followed(p)) {
                    let _ = changed.send(Msg::Folder(path.clone()));
                }
            }
            // A rescan means events were lost.
            if event.need_rescan()
                // Reads are access events on Linux. Computing a diff reads the worktree.
                || !event.kind.is_access() && event.paths.iter().any(|p| relevant(p))
            {
                let _ = changed.send(Msg::Changed);
            }
        };
        let failed = |e: notify::Error| CoreError::io("watch", root)(io::Error::other(e));
        let mut watcher = notify::recommended_watcher(handler).map_err(failed)?;
        for (path, mode) in watched {
            watcher.watch(&path, mode).map_err(failed)?;
        }
        if follow(root) {
            watch_folders(&mut watcher, root, &*follow).map_err(failed)?;
        }
        let thread = thread::Builder::new()
            .name("grove-watch".into())
            .spawn(move || debounce(watcher, &rx, &*follow, burst))
            .map_err(CoreError::io("start a watcher thread for", root))?;
        Ok(Watch {
            stop: tx,
            thread: Some(thread),
        })
    }
}

/// The warning for a worktree whose watcher failed to start with `error`.
pub(crate) fn not_started(worktree: &Path, error: &CoreError) -> Warning {
    Warning::WatcherNotStarted {
        worktree: worktree.to_path_buf(),
        reason: with_causes(error),
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.stop.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// Calls `burst` once per burst, DEBOUNCE after its first change, and watches the folders it is
// sent. It owns the watcher, so returning stops it.
fn debounce(
    mut watcher: RecommendedWatcher,
    rx: &mpsc::Receiver<Msg>,
    follow: &dyn Fn(&Path) -> bool,
    mut burst: impl FnMut() -> bool,
) {
    // When the current burst ends.
    let mut end: Option<Instant> = None;
    loop {
        let msg = match end {
            Some(end) => rx.recv_timeout(end.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match msg {
            Ok(Msg::Changed) => {
                end.get_or_insert_with(|| Instant::now() + DEBOUNCE);
            }
            // Best effort: the folder alone goes without live updates, and there is no one to
            // tell.
            Ok(Msg::Folder(folder)) => {
                let _ = watch_folders(&mut watcher, &folder, follow);
            }
            Err(RecvTimeoutError::Timeout) => {
                end = None;
                if !burst() {
                    return;
                }
            }
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

// Watches `folder` and the folders under it that `follow` takes, one watch each. Nothing is
// watched if `folder` is not a folder, as after a file's name event or once it is gone. An error
// watching `folder` itself is returned. Below it, a folder that cannot be watched or listed, such
// as a gone or unreadable one, goes without live updates and the walk goes on. Only running out
// of watches stops it, since every later watch would fail too.
fn watch_folders(
    watcher: &mut RecommendedWatcher,
    folder: &Path,
    follow: &dyn Fn(&Path) -> bool,
) -> notify::Result<()> {
    if !fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir()) {
        return Ok(());
    }
    match watcher.watch(folder, RecursiveMode::NonRecursive) {
        Err(e) if matches!(e.kind, notify::ErrorKind::PathNotFound) => return Ok(()),
        result => result?,
    }
    let mut folders = subfolders(folder, follow);
    while let Some(folder) = folders.pop() {
        match watcher.watch(&folder, RecursiveMode::NonRecursive) {
            Ok(()) => folders.extend(subfolders(&folder, follow)),
            Err(e) if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) => return Err(e),
            Err(_) => {}
        }
    }
    Ok(())
}

// The folders in `folder` that `follow` takes, or none if it cannot be listed. Links are not
// followed: they can lead out of the worktree or loop.
fn subfolders(folder: &Path, follow: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| follow(path))
        .collect()
}

// Real paths, since notify reports real paths.
#[derive(Clone)]
struct Paths {
    worktree: PathBuf,
    // `.git` in the main worktree, `<common dir>/worktrees/<name>` in a linked one.
    git_dir: PathBuf,
    common_dir: PathBuf,
}

impl Paths {
    fn of(worktree: &Path) -> Result<Paths> {
        let repo = git2::Repository::open(worktree).map_err(CoreError::git2(worktree))?;
        let real = |path: &Path| fs::canonicalize(path).map_err(CoreError::io("resolve", path));
        Ok(Paths {
            worktree: real(worktree)?,
            git_dir: real(repo.path())?,
            common_dir: real(repo.commondir())?,
        })
    }

    // On macOS the worktree's recursive watch takes in the git dirs inside it, as in the main
    // worktree, so only git dirs outside it need watches of their own. On Linux the worktree's
    // own folders are watched without them. Git replaces HEAD, index and packed-refs by
    // renaming, so their folders are watched.
    fn watched(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let mut watched = Vec::new();
        if !EACH_FOLDER {
            watched.push((self.worktree.clone(), RecursiveMode::Recursive));
        }
        let own_watch = |dir: &Path| EACH_FOLDER || !dir.starts_with(&self.worktree);
        if own_watch(&self.git_dir) {
            watched.push((self.git_dir.clone(), RecursiveMode::NonRecursive));
        }
        if own_watch(&self.common_dir) {
            // The same folder in a main worktree.
            if self.common_dir != self.git_dir {
                watched.push((self.common_dir.clone(), RecursiveMode::NonRecursive));
            }
            watched.push((self.common_dir.join("refs"), RecursiveMode::Recursive));
        }
        watched
    }

    // Whether a change at `path` can change the worktree's diff.
    fn relevant(&self, path: &Path) -> bool {
        let in_git_dir = path.strip_prefix(&self.git_dir).ok();
        let in_common_dir = path.strip_prefix(&self.common_dir).ok();
        if in_git_dir.is_some() || in_common_dir.is_some() {
            return in_git_dir
                .is_some_and(|rest| rest == Path::new("HEAD") || rest == Path::new("index"))
                || in_common_dir.is_some_and(|rest| {
                    rest == Path::new("packed-refs") || rest.starts_with("refs")
                });
        }
        self.in_files(path)
    }

    // Whether `path` is in the worktree and in none of its skipped folders.
    fn in_files(&self, path: &Path) -> bool {
        path.strip_prefix(&self.worktree).is_ok_and(|rest| {
            !rest
                .components()
                .any(|part| SKIPPED.iter().any(|skipped| part.as_os_str() == *skipped))
        })
    }
}
