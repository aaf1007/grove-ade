use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::Command;
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{Core, CoreConfig, CoreError, CoreEvent};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

const WAIT: Duration = Duration::from_secs(5);

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
// Tests keep the home folder, and the socket in it, under /tmp.
fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

// The core owns its runtime, and dropping it inside an async context panics.
// So tests make and drop the core outside, and only await its calls here.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

// Builds fixtures/diff-repo in `dir`, starts a core that tracks it, and returns the repo's real
// path.
fn start_on_fixture(dir: &Path) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf) {
    let build = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/diff-repo/build.py");
    let out = Command::new("python3")
        .arg(build)
        .arg(dir.join("fixture"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let repo = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
        .canonicalize()
        .unwrap();
    let (core, events) = Core::start(CoreConfig::new(dir.join("home"))).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    (core, events, repo)
}

// `git ls-files --cached --others --exclude-standard`, sorted, each path once. git prints a
// conflicted path once per stage, and the untracked files apart from the rest.
fn git_ls_files(repo: &Path) -> Vec<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut paths: Vec<PathBuf> = out
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(OsStr::from_bytes(p)))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

// Waits until no event has come for a while, so later ones come from what the test does next.
fn settle(events: &mut broadcast::Receiver<CoreEvent>) {
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_millis(500) {
        match events.try_recv() {
            Ok(_) => quiet_since = Instant::now(),
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(10)),
            Err(e) => panic!("{e}"),
        }
    }
}

// Waits for the next `DiffChanged` for `worktree`. Panics after WAIT.
fn wait_diff_changed(events: &mut broadcast::Receiver<CoreEvent>, worktree: &Path) {
    let start = Instant::now();
    loop {
        match events.try_recv() {
            Ok(CoreEvent::DiffChanged { worktree: w }) if w == worktree => return,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(start.elapsed() < WAIT, "no DiffChanged within {WAIT:?}");
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

fn assert_outside(result: grove_core::Result<impl std::fmt::Debug>, path: &str) {
    match result {
        Err(CoreError::OutsideWorktree(p)) => assert_eq!(p, Path::new(path)),
        other => panic!("{path}: expected OutsideWorktree, got {other:?}"),
    }
}

#[test]
fn list_files_matches_git_ls_files() {
    let dir = short_temp_dir();
    let (core, _events, repo) = start_on_fixture(dir.path());

    let files = block_on(core.list_files(&repo)).unwrap();

    assert_eq!(files, git_ls_files(&repo));
    // Untracked files are listed, ignored ones are not.
    assert!(files.contains(&PathBuf::from("untracked-dir/nested.txt")));
    assert!(files.contains(&PathBuf::from("dir with space/naïve.txt")));
    assert!(!files.contains(&PathBuf::from("build.log")));
    assert!(!files.iter().any(|f| f.starts_with(".git")));
}

#[test]
fn listing_an_untracked_worktree_fails() {
    let dir = short_temp_dir();
    let (core, _events, _repo) = start_on_fixture(dir.path());
    let elsewhere = dir.path().canonicalize().unwrap();

    match block_on(core.list_files(&elsewhere)) {
        Err(CoreError::UnknownWorktree(p)) => assert_eq!(p, elsewhere),
        other => panic!("expected UnknownWorktree, got {other:?}"),
    }
}

// The first poll hands the work to another thread and returns, so the caller's tokio worker is
// free while git runs.
#[test]
fn listing_does_not_run_on_the_callers_thread() {
    let dir = short_temp_dir();
    let (core, _events, repo) = start_on_fixture(dir.path());

    let mut listing = pin!(core.list_files(&repo));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(listing.as_mut().poll(&mut cx).is_pending());

    let files = block_on(listing).unwrap();
    assert_eq!(files, git_ls_files(&repo));
}

#[test]
fn read_file_returns_the_exact_bytes() {
    let dir = short_temp_dir();
    let (core, _events, repo) = start_on_fixture(dir.path());

    for file in ["latin1.txt", "binary.dat", "untracked.bin", "emptied.txt"] {
        let bytes = block_on(core.read_file(&repo, Path::new(file))).unwrap();
        assert_eq!(bytes, fs::read(repo.join(file)).unwrap(), "{file}");
    }
}

#[test]
fn write_file_round_trips_bytes_that_are_not_utf8() {
    let dir = short_temp_dir();
    let (core, _events, repo) = start_on_fixture(dir.path());
    let latin1 = b"caf\xe9\nna\xefve\n".to_vec();
    let all_bytes: Vec<u8> = (0..=255).collect();

    block_on(core.write_file(&repo, Path::new("latin1.txt"), &latin1)).unwrap();
    block_on(core.write_file(&repo, Path::new("untracked-dir/new.bin"), &all_bytes)).unwrap();

    assert_eq!(
        block_on(core.read_file(&repo, Path::new("latin1.txt"))).unwrap(),
        latin1
    );
    assert_eq!(
        fs::read(repo.join("untracked-dir/new.bin")).unwrap(),
        all_bytes
    );
}

#[test]
fn write_file_leaves_no_temp_file_and_keeps_the_mode() {
    let dir = short_temp_dir();
    let (core, _events, repo) = start_on_fixture(dir.path());
    let before = block_on(core.list_files(&repo)).unwrap();

    block_on(core.write_file(&repo, Path::new("script.sh"), b"#!/bin/sh\necho hi\n")).unwrap();

    assert_eq!(block_on(core.list_files(&repo)).unwrap(), before);
    assert_eq!(
        fs::read(repo.join("script.sh")).unwrap(),
        b"#!/bin/sh\necho hi\n"
    );
    assert_eq!(mode_of(&repo.join("script.sh")), 0o755);
}

#[test]
fn a_private_file_stays_private_and_a_new_file_gets_the_usual_mode() {
    let dir = short_temp_dir();
    let (core, _events, repo) = start_on_fixture(dir.path());
    fs::set_permissions(
        repo.join("untracked.txt"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(repo.join("plain.txt"), "made the usual way\n").unwrap();

    block_on(core.write_file(&repo, Path::new("untracked.txt"), b"secret\n")).unwrap();
    block_on(core.write_file(&repo, Path::new("new.txt"), b"new\n")).unwrap();

    assert_eq!(mode_of(&repo.join("untracked.txt")), 0o600);
    assert_eq!(
        mode_of(&repo.join("new.txt")),
        mode_of(&repo.join("plain.txt"))
    );
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// A symlink inside the worktree is followed: the file it points at changes, and the link stays.
#[test]
fn write_file_through_a_symlink_writes_its_target() {
    let dir = short_temp_dir();
    let (core, _events, repo) = start_on_fixture(dir.path());
    symlink("untracked.txt", repo.join("inner-link")).unwrap();

    block_on(core.write_file(&repo, Path::new("inner-link"), b"through the link\n")).unwrap();

    assert!(
        fs::symlink_metadata(repo.join("inner-link"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        fs::read(repo.join("untracked.txt")).unwrap(),
        b"through the link\n"
    );
}

#[test]
fn paths_that_lead_out_of_the_worktree_are_refused() {
    let dir = short_temp_dir();
    let (core, _events, repo) = start_on_fixture(dir.path());
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "secret\n").unwrap();
    symlink(outside.join("secret.txt"), repo.join("escape")).unwrap();
    symlink(&outside, repo.join("out")).unwrap();
    let absolute = repo.join("untracked.txt");
    let untouched = fs::read(&absolute).unwrap();

    for path in [
        "../outside/secret.txt",
        "untracked-dir/../../outside/secret.txt",
        absolute.to_str().unwrap(),
        "escape",
        "out/secret.txt",
    ] {
        assert_outside(block_on(core.read_file(&repo, Path::new(path))), path);
        assert_outside(
            block_on(core.write_file(&repo, Path::new(path), b"changed\n")),
            path,
        );
    }
    assert_outside(
        block_on(core.write_file(&repo, Path::new("out/new.txt"), b"new\n")),
        "out/new.txt",
    );

    assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), b"secret\n");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(fs::read(&absolute).unwrap(), untouched);
}

#[test]
fn a_write_sends_diff_changed() {
    let dir = short_temp_dir();
    let (core, mut events, repo) = start_on_fixture(dir.path());
    settle(&mut events);

    block_on(core.write_file(&repo, Path::new("feature.txt"), b"saved from grove\n")).unwrap();

    wait_diff_changed(&mut events, &repo);
}
