use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use grove_core::{Core, CoreConfig, CoreError, CoreEvent, Editor};
use tempfile::TempDir;
use tokio::sync::broadcast;

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

fn new_repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    let status = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    repo.canonicalize().unwrap()
}

// Each case fails before grove runs `open`, so no editor starts.

#[test]
fn opening_a_file_needs_a_worktree_grove_tracks() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());

    let opened = block_on(core.open_in_editor(Editor::Zed, &repo, Path::new("a.rs"), 1));

    assert!(
        matches!(&opened, Err(CoreError::UnknownWorktree(path)) if *path == repo),
        "{opened:?}"
    );
}

#[test]
fn a_file_outside_the_worktree_cannot_be_opened() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    for file in [
        PathBuf::from("../outside.txt"),
        dir.path().join("outside.txt"),
    ] {
        let opened = block_on(core.open_in_editor(Editor::VsCode, &repo, &file, 1));
        assert!(
            matches!(&opened, Err(CoreError::OutsideWorktree(p)) if *p == file),
            "{opened:?}"
        );
    }
}

#[test]
fn a_symlink_out_of_the_worktree_cannot_be_opened() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let secret = dir.path().join("secret.txt");
    fs::write(&secret, "key\n").unwrap();
    symlink(&secret, repo.join("link")).unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let opened = block_on(core.open_in_editor(Editor::Zed, &repo, Path::new("link"), 1));

    assert!(
        matches!(&opened, Err(CoreError::OutsideWorktree(p)) if p == Path::new("link")),
        "{opened:?}"
    );
}

#[test]
fn revealing_in_finder_needs_a_worktree_grove_tracks() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());

    let revealed = block_on(core.reveal_in_finder(&repo));

    assert!(
        matches!(&revealed, Err(CoreError::UnknownWorktree(path)) if *path == repo),
        "{revealed:?}"
    );
}

// `open -R` fails on a folder that is gone, so no Finder window opens. Only macOS has `open`.
#[cfg(target_os = "macos")]
#[test]
fn revealing_a_tracked_worktree_that_is_gone_says_why() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    fs::remove_dir_all(&repo).unwrap();

    let revealed = block_on(core.reveal_in_finder(&repo));

    assert!(
        matches!(&revealed, Err(CoreError::RevealInFinder { path, reason })
            if *path == repo && reason.contains("does not exist")),
        "{revealed:?}"
    );
}

// Each is refused before `open` runs, so no app starts.
#[test]
fn opening_a_link_refuses_anything_but_http_and_https() {
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());

    for link in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "mailto:someone@example.com",
        "vscode://file/etc/passwd:1",
        "/System/Applications/Calculator.app",
        "-a Calculator",
        "http:example.com",
        "",
    ] {
        let opened = block_on(core.open_link_in_default_browser(link));

        assert!(
            matches!(&opened, Err(CoreError::LinkIsNotHttpOrHttps { link: refused }) if refused == link),
            "{link}: {opened:?}"
        );
    }
}
