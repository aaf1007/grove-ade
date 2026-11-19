use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use grove_core::{Core, CoreConfig, CoreError, CoreEvent, NewWorktree, WorktreeInfo};
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

// git's stdout, trimmed. Panics when git fails.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed in {}",
        dir.display()
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

// A repo on `main` with one commit. Returns its real path, as git reports it.
// On macOS /tmp is a link to /private/tmp.
fn new_repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    repo.canonicalize().unwrap()
}

fn main_worktree(path: &Path) -> WorktreeInfo {
    WorktreeInfo {
        path: path.to_path_buf(),
        branch: Some("main".into()),
        is_main: true,
        display_name: "main".into(),
        issue_link: None,
        base_branch: Some("main".into()),
    }
}

#[test]
fn adding_a_repo_lists_its_main_worktree() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());

    block_on(core.add_repo(&repo)).unwrap();

    assert_eq!(block_on(core.list_worktrees()), vec![main_worktree(&repo)]);
}

#[test]
fn adding_a_repo_adopts_worktrees_it_already_has() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    git(&repo, &["worktree", "add", "-q", "--detach", "../detached"]);
    let (core, _events) = start(dir.path());

    block_on(core.add_repo(&repo)).unwrap();

    let worktrees = block_on(core.list_worktrees());
    let (main, linked) = worktrees.split_first().unwrap();
    assert_eq!(*main, main_worktree(&repo));
    // git's docs don't promise an order for linked worktrees, so compare them sorted.
    let mut linked = linked.to_vec();
    linked.sort_by(|a, b| a.path.cmp(&b.path));
    let real = |name: &str| dir.path().join(name).canonicalize().unwrap();
    assert_eq!(
        linked,
        vec![
            WorktreeInfo {
                path: real("detached"),
                branch: None,
                is_main: false,
                display_name: "detached".into(),
                issue_link: None,
                base_branch: Some("main".into()),
            },
            WorktreeInfo {
                path: real("feature"),
                branch: Some("feature".into()),
                is_main: false,
                display_name: "feature".into(),
                issue_link: None,
                base_branch: Some("main".into()),
            },
        ]
    );
}

#[test]
fn adding_a_folder_that_is_not_a_repo_fails() {
    let dir = short_temp_dir();
    let folder = dir.path().join("plain");
    fs::create_dir(&folder).unwrap();
    let (core, _events) = start(dir.path());

    let added = block_on(core.add_repo(&folder));

    assert!(
        matches!(&added, Err(CoreError::NotARepo { path, .. }) if *path == folder),
        "{added:?}"
    );
    assert_eq!(block_on(core.list_worktrees()), vec![]);
}

#[test]
fn adding_a_repo_twice_fails_even_through_one_of_its_worktrees() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let before = block_on(core.list_worktrees());

    for path in [repo.clone(), dir.path().join("feature")] {
        let again = block_on(core.add_repo(&path));
        assert!(
            matches!(&again, Err(CoreError::RepoAlreadyAdded(main)) if *main == repo),
            "{again:?}"
        );
    }
    assert_eq!(block_on(core.list_worktrees()), before);
}

// Where grove puts a new worktree of the repo at `<dir>/repo`, as a real path.
fn worktree_folder(dir: &Path, name: &str) -> PathBuf {
    dir.join("home")
        .canonicalize()
        .unwrap()
        .join("worktrees/repo")
        .join(name)
}

fn new_worktree(branch: &str) -> NewWorktree {
    NewWorktree {
        branch: branch.into(),
        ..NewWorktree::default()
    }
}

#[test]
fn a_new_name_makes_a_branch_from_the_base_branch() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    // The main worktree moves off `main`, so a branch made from HEAD would show.
    git(&repo, &["switch", "-q", "-c", "other"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "other"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let added = block_on(core.create_worktree(&repo, new_worktree("topic")))
        .unwrap()
        .worktree;

    let folder = worktree_folder(dir.path(), "topic");
    let expected = WorktreeInfo {
        path: folder.clone(),
        branch: Some("topic".into()),
        is_main: false,
        display_name: "topic".into(),
        issue_link: None,
        base_branch: Some("main".into()),
    };
    assert_eq!(added, expected);
    assert_eq!(
        git_out(&folder, &["rev-parse", "HEAD"]),
        git_out(&repo, &["rev-parse", "main"])
    );
    let listed = block_on(core.list_worktrees());
    assert_eq!(listed.last(), Some(&expected));
}

// Gives the repo a bare remote named `origin` and pushes `main` to it.
fn add_origin(dir: &Path, repo: &Path) {
    let origin = dir.join("origin.git");
    git(dir, &["init", "-q", "--bare", "origin.git"]);
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(repo, &["push", "-q", "origin", "main"]);
}

fn upstream(dir: &Path, branch: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "rev-parse",
            "--abbrev-ref",
            &format!("{branch}@{{upstream}}"),
        ])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
}

#[test]
fn a_new_branch_starts_from_the_base_on_origin_and_tracks_nothing() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    add_origin(dir.path(), &repo);
    // Local `main` gets ahead of `origin/main`.
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "local only"],
    );
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let added = block_on(core.create_worktree(&repo, new_worktree("topic")))
        .unwrap()
        .worktree;

    assert_eq!(added.base_branch.as_deref(), Some("main"));
    assert_eq!(
        git_out(&added.path, &["rev-parse", "HEAD"]),
        git_out(&repo, &["rev-parse", "origin/main"])
    );
    assert_eq!(upstream(&repo, "topic"), None);
}

#[test]
fn a_local_branch_is_checked_out() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    git(&repo, &["switch", "-q", "-c", "existing"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "existing"]);
    git(&repo, &["switch", "-q", "main"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let added = block_on(core.create_worktree(&repo, new_worktree("existing")))
        .unwrap()
        .worktree;

    assert_eq!(added.path, worktree_folder(dir.path(), "existing"));
    assert_eq!(added.branch.as_deref(), Some("existing"));
    assert_eq!(
        git_out(&added.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "existing"
    );
    assert_eq!(
        git_out(&added.path, &["rev-parse", "HEAD"]),
        git_out(&repo, &["rev-parse", "existing"])
    );
}

#[test]
fn a_branch_only_on_origin_is_checked_out_tracking_it() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    add_origin(dir.path(), &repo);
    git(&repo, &["switch", "-q", "-c", "remote-only"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "remote only"],
    );
    git(&repo, &["push", "-q", "origin", "remote-only"]);
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["branch", "-q", "-D", "remote-only"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let added = block_on(core.create_worktree(&repo, new_worktree("remote-only")))
        .unwrap()
        .worktree;

    assert_eq!(added.branch.as_deref(), Some("remote-only"));
    assert_eq!(
        git_out(&added.path, &["rev-parse", "HEAD"]),
        git_out(&repo, &["rev-parse", "origin/remote-only"])
    );
    assert_eq!(
        upstream(&repo, "remote-only").as_deref(),
        Some("origin/remote-only")
    );
}

#[test]
fn a_slash_in_the_name_becomes_a_dash_in_the_folder() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let added = block_on(core.create_worktree(&repo, new_worktree("feat/x")))
        .unwrap()
        .worktree;

    assert_eq!(added.path, worktree_folder(dir.path(), "feat-x"));
    assert_eq!(added.branch.as_deref(), Some("feat/x"));
    assert_eq!(added.display_name, "feat/x");
}

#[test]
fn names_that_map_to_a_taken_folder_fail() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.create_worktree(&repo, new_worktree("feat/x"))).unwrap();
    // A folder already on disk counts as taken too, even if grove does not track it.
    let stray = worktree_folder(dir.path(), "stray");
    fs::create_dir_all(&stray).unwrap();
    let before = block_on(core.list_worktrees());

    for (name, folder) in [("feat-x", "feat-x"), ("stray", "stray")] {
        let again = block_on(core.create_worktree(&repo, new_worktree(name)));
        let taken = worktree_folder(dir.path(), folder);
        assert!(
            matches!(&again, Err(CoreError::FolderTaken(path)) if *path == taken),
            "{again:?}"
        );
    }
    assert_eq!(block_on(core.list_worktrees()), before);
    // Neither failed add made a branch.
    for name in ["feat-x", "stray"] {
        let made = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ])
            .status()
            .unwrap();
        assert!(!made.success(), "{name} was made");
    }
}

#[test]
fn an_invalid_branch_name_fails() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    for name in ["", "bad..name", "-dash"] {
        let added = block_on(core.create_worktree(&repo, new_worktree(name)));
        assert!(
            matches!(&added, Err(CoreError::InvalidBranchName { name: n, .. }) if n == name),
            "{name:?}: {added:?}"
        );
    }
    assert_eq!(block_on(core.list_worktrees()).len(), 1);
}

#[test]
fn adding_to_a_repo_grove_does_not_have_fails() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());

    let added = block_on(core.create_worktree(&repo, new_worktree("topic")));

    assert!(
        matches!(&added, Err(CoreError::UnknownRepo(path)) if *path == repo),
        "{added:?}"
    );
}

#[test]
fn the_default_base_branch_follows_origin_head() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    add_origin(dir.path(), &repo);
    git(&repo, &["switch", "-q", "-c", "trunk"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "trunk"]);
    git(&repo, &["push", "-q", "origin", "trunk"]);
    git(&repo, &["switch", "-q", "main"]);
    git(&repo, &["remote", "set-head", "origin", "trunk"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let added = block_on(core.create_worktree(&repo, new_worktree("topic")))
        .unwrap()
        .worktree;

    assert_eq!(added.base_branch.as_deref(), Some("trunk"));
    assert_eq!(
        git_out(&added.path, &["rev-parse", "HEAD"]),
        git_out(&repo, &["rev-parse", "origin/trunk"])
    );
    let main = &block_on(core.list_worktrees())[0];
    assert_eq!(main.base_branch.as_deref(), Some("trunk"));
}

// A repo whose only branch is `branch`, at `<dir>/repo`.
fn repo_on(dir: &Path, branch: &str) -> PathBuf {
    let repo = dir.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", branch]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    repo.canonicalize().unwrap()
}

#[test]
fn without_origin_head_the_base_branch_is_main_then_master() {
    let both = short_temp_dir();
    let repo = repo_on(both.path(), "master");
    git(&repo, &["branch", "main"]);
    let (core, _events) = start(both.path());
    block_on(core.add_repo(&repo)).unwrap();
    let added = block_on(core.create_worktree(&repo, new_worktree("topic")))
        .unwrap()
        .worktree;
    assert_eq!(added.base_branch.as_deref(), Some("main"));
    drop(core);

    let only_master = short_temp_dir();
    let repo = repo_on(only_master.path(), "master");
    let (core, _events) = start(only_master.path());
    block_on(core.add_repo(&repo)).unwrap();
    let added = block_on(core.create_worktree(&repo, new_worktree("topic")))
        .unwrap()
        .worktree;
    assert_eq!(added.base_branch.as_deref(), Some("master"));
}

#[test]
fn with_no_default_branch_a_new_name_needs_a_base_branch() {
    let dir = short_temp_dir();
    let repo = repo_on(dir.path(), "develop");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    assert_eq!(block_on(core.list_worktrees())[0].base_branch, None);

    let added = block_on(core.create_worktree(&repo, new_worktree("topic")));
    assert!(
        matches!(&added, Err(CoreError::NoBaseBranch(path)) if *path == repo),
        "{added:?}"
    );

    let given = NewWorktree {
        base_branch: Some("develop".into()),
        ..new_worktree("topic")
    };
    let added = block_on(core.create_worktree(&repo, given))
        .unwrap()
        .worktree;
    assert_eq!(added.base_branch.as_deref(), Some("develop"));
    assert_eq!(
        git_out(&added.path, &["rev-parse", "HEAD"]),
        git_out(&repo, &["rev-parse", "develop"])
    );
}

#[test]
fn changing_the_base_branch_is_stored_on_the_worktree() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    git(&repo, &["branch", "release"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let added = block_on(core.create_worktree(&repo, new_worktree("topic")))
        .unwrap()
        .worktree;

    block_on(core.set_base_branch(&added.path, "release".into())).unwrap();
    let missing = block_on(core.set_base_branch(&added.path, "nope".into()));

    assert!(
        matches!(&missing, Err(CoreError::UnknownBranch { name, .. }) if name == "nope"),
        "{missing:?}"
    );
    let listed = block_on(core.list_worktrees());
    assert_eq!(listed[0].base_branch.as_deref(), Some("main"));
    assert_eq!(listed[1].base_branch.as_deref(), Some("release"));
}

#[test]
fn a_base_branch_that_does_not_exist_fails_the_add() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let given = NewWorktree {
        base_branch: Some("nope".into()),
        ..new_worktree("topic")
    };
    let added = block_on(core.create_worktree(&repo, given));

    assert!(
        matches!(&added, Err(CoreError::UnknownBranch { name, .. }) if name == "nope"),
        "{added:?}"
    );
    assert!(!worktree_folder(dir.path(), "topic").exists());
}

#[test]
fn display_name_and_issue_link_are_stored_and_listed() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let link = "https://example.com/issues/20";
    let given = NewWorktree {
        display_name: Some("Fix login".into()),
        issue_link: Some(link.into()),
        ..new_worktree("fix-login")
    };

    let added = block_on(core.create_worktree(&repo, given))
        .unwrap()
        .worktree;

    assert_eq!(added.display_name, "Fix login");
    assert_eq!(added.issue_link.as_deref(), Some(link));
    assert_eq!(block_on(core.list_worktrees())[1], added);

    block_on(core.set_display_name(&added.path, "Login fix".into())).unwrap();
    block_on(core.set_issue_link(&added.path, None)).unwrap();
    block_on(core.set_issue_link(&repo, Some(link.into()))).unwrap();

    let listed = block_on(core.list_worktrees());
    assert_eq!(listed[0].issue_link.as_deref(), Some(link));
    assert_eq!(listed[1].display_name, "Login fix");
    assert_eq!(listed[1].issue_link, None);
}

#[test]
fn changing_an_untracked_worktree_fails() {
    let dir = short_temp_dir();
    let folder = dir.path().join("plain");
    let (core, _events) = start(dir.path());

    let renamed = block_on(core.set_display_name(&folder, "x".into()));
    let linked = block_on(core.set_issue_link(&folder, None));
    let based = block_on(core.set_base_branch(&folder, "main".into()));

    for result in [renamed, linked, based] {
        assert!(
            matches!(&result, Err(CoreError::UnknownWorktree(path)) if *path == folder),
            "{result:?}"
        );
    }
}

#[test]
fn adding_runs_the_repos_git_hooks() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let hooks = dir.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    fs::write(&hook, "#!/bin/sh\ntouch hook-ran\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &repo,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let added = block_on(core.create_worktree(&repo, new_worktree("topic")))
        .unwrap()
        .worktree;

    // git runs post-checkout at the top of the new worktree.
    assert!(added.path.join("hook-ran").exists());
}

#[test]
fn a_new_worktree_is_listed_with_its_own_repo() {
    let dir = short_temp_dir();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let first = new_repo(&first);
    let second = new_repo(&second);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&first)).unwrap();
    block_on(core.add_repo(&second)).unwrap();

    let added = block_on(core.create_worktree(&first, new_worktree("topic")))
        .unwrap()
        .worktree;

    let paths: Vec<_> = block_on(core.list_worktrees())
        .into_iter()
        .map(|w| w.path)
        .collect();
    assert_eq!(paths, vec![first, added.path, second]);
}
