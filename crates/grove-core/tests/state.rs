use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Barrier, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Core, CoreConfig, CoreError, CoreEvent, LaunchArgs, NewWorktree, ProfileSettings, RemoveOpts,
    RepoSettings, RunSession, WorktreeInfo,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

// A child that another test's thread has forked keeps every open file of this process until it
// execs, grove.lock included. A core stopped in that window stays locked, and the next start
// fails with AlreadyRunning. Tests here restart cores, so they run one at a time.
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

// Stops `core`, starts a new one on the same home folder, and checks that it lists the same
// worktrees.
fn restart(core: Core, dir: &Path) -> Core {
    let before = block_on(core.list_worktrees());
    drop(core);
    let (core, _events) = start(dir);
    assert_eq!(block_on(core.list_worktrees()), before);
    core
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
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    repo.canonicalize().unwrap()
}

// Waits for a `DiffChanged` for `worktree`, and panics after 5 seconds. A watcher sends one about
// 150 ms after a change. The rest leaves room for a loaded CI runner.
fn wait_diff_changed(events: &mut broadcast::Receiver<CoreEvent>, worktree: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match events.try_recv() {
            Ok(CoreEvent::DiffChanged { worktree: w }) if w == worktree => return,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(
                    Instant::now() < deadline,
                    "no DiffChanged for {}",
                    worktree.display()
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn every_change_to_repos_and_worktrees_survives_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let first = new_repo(dir.path(), "first");
    git(&first, &["branch", "develop"]);
    let second = new_repo(dir.path(), "second");
    git(
        &second,
        &["worktree", "add", "-q", "--detach", "../detached"],
    );
    let detached = dir.path().join("detached").canonicalize().unwrap();
    let issue = "https://example.com/issues/50";
    let (core, _events) = start(dir.path());

    block_on(core.add_repo(&first)).unwrap();
    let core = restart(core, dir.path());
    block_on(core.add_repo(&second)).unwrap();
    let core = restart(core, dir.path());
    let new = NewWorktree {
        branch: "login".into(),
        display_name: Some("Fix login".into()),
        issue_link: Some("https://example.com/issues/20".into()),
        base_branch: None,
    };
    let added = block_on(core.create_worktree(&first, new))
        .unwrap()
        .worktree;
    let core = restart(core, dir.path());
    block_on(core.set_base_branch(&added.path, "develop".into())).unwrap();
    let core = restart(core, dir.path());
    block_on(core.set_display_name(&added.path, "Login fix".into())).unwrap();
    let core = restart(core, dir.path());
    block_on(core.set_issue_link(&added.path, Some(issue.into()))).unwrap();
    let core = restart(core, dir.path());

    let main = |path: &Path| WorktreeInfo {
        path: path.to_path_buf(),
        branch: Some("main".into()),
        is_main: true,
        display_name: "main".into(),
        issue_link: None,
        base_branch: Some("main".into()),
        nested_under: None,
    };
    assert_eq!(
        block_on(core.list_worktrees()),
        vec![
            main(&first),
            WorktreeInfo {
                path: added.path,
                branch: Some("login".into()),
                is_main: false,
                display_name: "Login fix".into(),
                issue_link: Some(issue.into()),
                base_branch: Some("develop".into()),
                nested_under: None,
            },
            main(&second),
            WorktreeInfo {
                path: detached,
                branch: None,
                is_main: false,
                display_name: "detached".into(),
                issue_link: None,
                base_branch: Some("main".into()),
                nested_under: None,
            },
        ]
    );
}

#[test]
fn run_sessions_and_launch_args_survive_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let app = new_repo(dir.path(), "app");
    let lib = new_repo(dir.path(), "lib");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&app)).unwrap();
    block_on(core.add_repo(&lib)).unwrap();
    let settings = RepoSettings {
        profile: None,
        overrides: ProfileSettings {
            run_sessions: Some(vec![
                RunSession {
                    name: "dev".into(),
                    command: "PORT=5173 pnpm dev".into(),
                    port: Some(5173),
                },
                RunSession {
                    name: "tests".into(),
                    command: "cargo test".into(),
                    port: None,
                },
            ]),
            launch_args: LaunchArgs {
                claude: Some(vec!["--add-dir".into(), "../shared".into()]),
                codex: Some(vec!["--search".into()]),
            },
            ..ProfileSettings::default()
        },
    };
    block_on(core.set_repo_settings(&app, settings.clone())).unwrap();

    let core = restart(core, dir.path());

    assert_eq!(core.repo_settings(&app).unwrap(), settings);
    assert_eq!(core.repo_settings(&lib).unwrap(), RepoSettings::default());
}

#[test]
fn a_state_file_from_before_repo_settings_loads_with_empty_settings() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let cfg = CoreConfig::new(dir.path().join("home"));
    fs::create_dir(&cfg.home).unwrap();
    // As a version 1 grove wrote it.
    let v1 = format!(
        r#"{{"version": 1, "repos": [{{"main_worktree": {{"path": "{}", "branch": "main",
        "display_name": "main", "issue_link": null, "base_branch": "main"}},
        "linked_worktrees": []}}]}}"#,
        repo.display()
    );
    fs::write(cfg.home.join("state.json"), v1).unwrap();

    let (core, _events) = Core::start(cfg).unwrap();

    assert_eq!(block_on(core.list_worktrees())[0].path, repo);
    assert_eq!(core.repo_settings(&repo).unwrap(), RepoSettings::default());
}

#[test]
fn a_state_file_from_before_nested_worktrees_loads_with_none_nested() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let linked = dir.path().join("linked");
    let cfg = CoreConfig::new(dir.path().join("home"));
    fs::create_dir(&cfg.home).unwrap();
    // As a version 7 grove wrote it, with no nested_under.
    let v7 = format!(
        r#"{{"version": 7, "repos": [{{"main_worktree": {{"path": "{}", "branch": "main",
        "display_name": "main", "issue_link": null, "base_branch": "main"}},
        "linked_worktrees": [{{"path": "{}", "branch": "feat", "display_name": "feat",
        "issue_link": null, "base_branch": "main"}}]}}]}}"#,
        repo.display(),
        linked.display()
    );
    fs::write(cfg.home.join("state.json"), v7).unwrap();

    let (core, _events) = Core::start(cfg).unwrap();

    let listed = block_on(core.list_worktrees());
    assert_eq!(
        listed
            .iter()
            .map(|w| (&w.path, &w.nested_under))
            .collect::<Vec<_>>(),
        [(&repo, &None), (&linked, &None)]
    );
}

#[test]
fn a_state_file_from_before_profiles_moves_run_sessions_and_launch_args_into_overrides() {
    let _serial = serial();
    let dir = short_temp_dir();
    let app = new_repo(dir.path(), "app");
    let lib = new_repo(dir.path(), "lib");
    let cfg = CoreConfig::new(dir.path().join("home"));
    fs::create_dir(&cfg.home).unwrap();
    // As a version 2 grove wrote it. `lib` saved no settings, and `app` no Codex args.
    let worktree = |path: &Path| {
        format!(
            r#"{{"path": "{}", "branch": "main", "display_name": "main", "issue_link": null,
            "base_branch": "main"}}"#,
            path.display()
        )
    };
    let v2 = format!(
        r#"{{"version": 2, "repos": [
        {{"main_worktree": {}, "linked_worktrees": [],
          "run_sessions": [{{"name": "dev", "command": "pnpm dev", "port": 5173}}],
          "launch_args": {{"claude": ["--verbose"], "codex": []}}}},
        {{"main_worktree": {}, "linked_worktrees": [], "run_sessions": [],
          "launch_args": {{"claude": [], "codex": []}}}}]}}"#,
        worktree(&app),
        worktree(&lib)
    );
    let state = cfg.home.join("state.json");
    fs::write(&state, v2).unwrap();
    let migrated = RepoSettings {
        profile: None,
        overrides: ProfileSettings {
            run_sessions: Some(vec![RunSession {
                name: "dev".into(),
                command: "pnpm dev".into(),
                port: Some(5173),
            }]),
            launch_args: LaunchArgs {
                claude: Some(vec!["--verbose".into()]),
                codex: None,
            },
            ..ProfileSettings::default()
        },
    };

    let (core, _events) = Core::start(cfg).unwrap();

    assert_eq!(core.repo_settings(&app).unwrap(), migrated);
    assert_eq!(core.repo_settings(&lib).unwrap(), RepoSettings::default());
    // The next save writes the new version, which reads back the same.
    block_on(core.set_repo_settings(&app, migrated.clone())).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(saved["version"], 11);
    let core = restart(core, dir.path());
    assert_eq!(core.repo_settings(&app).unwrap(), migrated);
}

#[test]
fn a_leftover_temp_file_never_corrupts_the_state() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    drop(core);
    // A save cut short leaves part of its temp file next to the state file.
    let temp = dir.path().join("home/state.json.tmp");
    fs::write(&temp, r#"{"version": 1, "repos": [{"main_wor"#).unwrap();

    let (core, _events) = start(dir.path());
    assert_eq!(block_on(core.list_worktrees()).len(), 1);
    block_on(core.set_display_name(&repo, "Repo".into())).unwrap();
    drop(core);

    assert!(!temp.exists());
    let (core, _events) = start(dir.path());
    assert_eq!(block_on(core.list_worktrees())[0].display_name, "Repo");
}

#[test]
fn a_state_file_from_a_newer_grove_fails_start_and_is_left_as_it_was() {
    let _serial = serial();
    let dir = short_temp_dir();
    let cfg = CoreConfig::new(dir.path().join("home"));
    fs::create_dir(&cfg.home).unwrap();
    let file = cfg.home.join("state.json");
    let newer = r#"{"version": 999, "repos": {"a layout": "this grove does not know"}}"#;
    fs::write(&file, newer).unwrap();

    let Err(err) = Core::start(cfg) else {
        panic!("start did not fail");
    };

    let message = err.to_string();
    let CoreError::NewerStateFile { version, known, .. } = err else {
        panic!("{err:?}");
    };
    assert_eq!(version, 999);
    assert!(known < version);
    assert!(
        message.contains("999") && message.contains(&known.to_string()),
        "{message}"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), newer);
}

#[test]
fn a_state_file_that_does_not_parse_fails_start_and_is_left_as_it_was() {
    let _serial = serial();
    let dir = short_temp_dir();
    let cfg = CoreConfig::new(dir.path().join("home"));
    fs::create_dir(&cfg.home).unwrap();
    let file = cfg.home.join("state.json");
    let broken = r#"{"version": 1, "repos": [{"main_worktree": "#;
    fs::write(&file, broken).unwrap();

    let started = Core::start(cfg);

    assert!(
        matches!(&started, Err(CoreError::BadStateFile { path, .. }) if *path == file),
        "{:?}",
        started.err()
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), broken);
}

#[test]
fn a_restored_worktree_sends_diff_changed_when_a_file_in_it_changes() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let new = NewWorktree {
        branch: "topic".into(),
        ..NewWorktree::default()
    };
    let added = block_on(core.create_worktree(&repo, new)).unwrap().worktree;
    drop(core);
    let (_core, mut events) = start(dir.path());

    fs::write(added.path.join("a.txt"), "two\n").unwrap();

    wait_diff_changed(&mut events, &added.path);
}

#[test]
fn a_forgotten_worktree_stays_gone_after_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    git(&repo, &["worktree", "add", "-q", "-b", "topic", "../topic"]);
    let topic = dir.path().join("topic").canonicalize().unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    block_on(core.remove_worktree(&topic, RemoveOpts::Forget)).unwrap();
    drop(core);

    let (core, _events) = start(dir.path());
    let paths: Vec<_> = block_on(core.list_worktrees())
        .into_iter()
        .map(|w| w.path)
        .collect();
    assert_eq!(paths, vec![repo]);
}

#[test]
fn a_deleted_worktree_stays_gone_after_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    git(&repo, &["worktree", "add", "-q", "-b", "topic", "../topic"]);
    let topic = dir.path().join("topic").canonicalize().unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let delete = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };

    block_on(core.remove_worktree(&topic, delete)).unwrap();
    drop(core);

    let (core, _events) = start(dir.path());
    let paths: Vec<_> = block_on(core.list_worktrees())
        .into_iter()
        .map(|w| w.path)
        .collect();
    assert_eq!(paths, vec![repo]);
}

#[test]
fn a_worktree_whose_folder_went_away_is_still_restored() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    git(&repo, &["worktree", "add", "-q", "-b", "topic", "../topic"]);
    let topic = dir.path().join("topic").canonicalize().unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    drop(core);
    fs::remove_dir_all(&topic).unwrap();

    let (core, _events) = start(dir.path());

    let paths: Vec<_> = block_on(core.list_worktrees())
        .into_iter()
        .map(|w| w.path)
        .collect();
    assert_eq!(paths, vec![repo, topic]);
}

#[test]
fn a_save_that_fails_is_reported_and_leaves_the_last_state_whole() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    // A folder where the temp file goes makes the save fail.
    let temp = dir.path().join("home/state.json.tmp");
    fs::create_dir(&temp).unwrap();

    let renamed = block_on(core.set_display_name(&repo, "Repo".into()));
    drop(core);

    assert!(matches!(renamed, Err(CoreError::Io { .. })), "{renamed:?}");
    fs::remove_dir(&temp).unwrap();
    let (core, _events) = start(dir.path());
    assert_eq!(block_on(core.list_worktrees())[0].display_name, "main");
}

#[test]
fn changes_saved_at_the_same_time_all_survive_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo");
    for i in 0..8 {
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("topic-{i}"),
                &format!("../topic-{i}"),
            ],
        );
    }
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let worktrees = block_on(core.list_worktrees());
    let ready = Barrier::new(worktrees.len());

    thread::scope(|scope| {
        for worktree in &worktrees {
            let (core, ready) = (&core, &ready);
            scope.spawn(move || {
                ready.wait();
                let name = format!("renamed {}", worktree.display_name);
                block_on(core.set_display_name(&worktree.path, name)).unwrap();
            });
        }
    });
    drop(core);

    let (core, _events) = start(dir.path());
    for restored in block_on(core.list_worktrees()) {
        assert!(
            restored.display_name.starts_with("renamed "),
            "{restored:?}"
        );
    }
}
