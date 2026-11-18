//! Profiles: TOML files in the home folder's `profiles/`, and repo settings that pick one and
//! override parts of it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, AgentDefaults, Core, CoreConfig, CoreError, CoreEvent, LaunchArgs, Profile,
    ProfileSettings, ProfileText, RepoSettings, RunSession, RunSpec, SessionId, SpawnSpec,
    Suggestion,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

// A watcher sends its event about 150 ms after a change. The rest leaves room for a loaded CI
// runner.
const TIMEOUT: Duration = Duration::from_secs(5);

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

fn profiles_dir(dir: &Path) -> PathBuf {
    dir.join("home/profiles")
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

// A repo on `main` with one commit and `files` in it. Returns its real path, as git reports it.
fn new_repo(dir: &Path, name: &str, files: &[&str]) -> PathBuf {
    let repo = dir.join(name);
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "one\n").unwrap();
    for file in files {
        fs::write(repo.join(file), "").unwrap();
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    repo.canonicalize().unwrap()
}

fn names(core: &Core) -> Vec<String> {
    core.profiles()
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect()
}

fn profile(core: &Core, name: &str) -> Profile {
    core.profiles()
        .unwrap()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap()
}

fn run_session(name: &str, command: &str) -> RunSession {
    RunSession {
        name: name.into(),
        command: command.into(),
        port: None,
    }
}

// Every `ProfilesChanged` within `window`.
fn profiles_changed_within(
    events: &mut broadcast::Receiver<CoreEvent>,
    window: Duration,
) -> Vec<Option<SessionId>> {
    let end = Instant::now() + window;
    let mut seen = Vec::new();
    while Instant::now() < end {
        match events.try_recv() {
            Ok(CoreEvent::ProfilesChanged { by }) => seen.push(by),
            Ok(_) => {}
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("{e}"),
        }
    }
    seen
}

// Waits for the next `ProfilesChanged` and returns its `by`. Panics after TIMEOUT.
fn wait_profiles_changed(events: &mut broadcast::Receiver<CoreEvent>) -> Option<SessionId> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(CoreEvent::ProfilesChanged { by }) => return by,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "no ProfilesChanged");
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn the_starter_profiles_are_written_once_and_read_back() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());

    assert_eq!(names(&core), ["Next.js app", "Rust crate"]);
    let rust = profile(&core, "Rust crate");
    assert_eq!(rust.suggest_for, ["Cargo.toml"]);
    assert_eq!(rust.settings.agent.default_agent, Some(Agent::Claude));
    assert_eq!(rust.settings.setup.as_deref(), Some("cargo fetch"));
    assert_eq!(
        rust.settings.run_sessions,
        Some(vec![run_session("tests", "cargo test")])
    );
    let files = profiles_dir(dir.path());
    assert!(files.join("rust-crate.toml").is_file());
    assert!(files.join("next-js-app.toml").is_file());

    // A starter the user deleted stays deleted.
    drop(core);
    fs::remove_file(files.join("next-js-app.toml")).unwrap();
    let (core, _events) = start(dir.path());
    assert_eq!(names(&core), ["Rust crate"]);
}

#[test]
fn a_profile_the_user_edited_survives_a_restart_untouched() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());
    drop(core);
    let path = profiles_dir(dir.path()).join("rust-crate.toml");
    let edited = "# Mine now.\nname = \"Rust crate\"\nsuggest_for = [\"Cargo.toml\"]\n\n[agent]\nmodel = \"sonnet\"\n";
    fs::write(&path, edited).unwrap();

    let (core, _events) = start(dir.path());

    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    let rust = profile(&core, "Rust crate");
    assert_eq!(rust.settings.agent.model.as_deref(), Some("sonnet"));
    assert_eq!(rust.settings.setup, None);
}

#[test]
fn a_profile_with_an_unknown_or_wrong_typed_field_names_its_file_field_and_line() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());
    drop(core);
    let path = profiles_dir(dir.path()).join("rust-crate.toml");
    let cases = [
        (
            "name = \"Rust crate\"\n\n[agent]\nmodel = 5\n",
            "agent.model",
            4,
        ),
        (
            "name = \"Rust crate\"\n\n[agent]\nmodle = \"opus\"\n",
            "agent.modle",
            4,
        ),
        (
            "name = \"Rust crate\"\n\n[[run_session]]\nname = \"dev\"\ncommand = \"npm run dev\"\nport = \"3000\"\n",
            "run_session.port",
            6,
        ),
        (
            "name = \"Rust crate\"\n[notify]\nstates = [\n  \"NeedsInput\",\n  \"Asleep\",\n]\n",
            "notify.states",
            5,
        ),
    ];
    for (text, expected_field, expected_line) in cases {
        fs::write(&path, text).unwrap();
        let (core, _events) = start(dir.path());

        let err = core.profiles().unwrap_err();

        let CoreError::BadProfile {
            path: p,
            field,
            line,
            ..
        } = &err
        else {
            panic!("{err:?}");
        };
        assert_eq!(p, &path, "{text}");
        assert_eq!(field.as_deref(), Some(expected_field), "{text}");
        assert_eq!(*line, Some(expected_line), "{text}");
        let message = err.to_string();
        assert!(message.contains("rust-crate.toml"), "{message}");
        assert!(message.contains(expected_field), "{message}");
        assert!(
            message.contains(&format!("line {expected_line}")),
            "{message}"
        );
        drop(core);
    }
}

#[test]
fn a_repo_gets_the_profile_of_the_marker_file_it_has_and_one_with_no_marker_gets_none() {
    let _serial = serial();
    let dir = short_temp_dir();
    let rust = new_repo(dir.path(), "rust", &["Cargo.toml"]);
    // The last of the Next.js profile's three markers, so the suggestion names the one found.
    let next = new_repo(dir.path(), "next", &["next.config.ts"]);
    let plain = new_repo(dir.path(), "plain", &[]);
    let (core, _events) = start(dir.path());

    assert_eq!(
        core.suggest_profile(&rust),
        Some(Suggestion {
            profile: "Rust crate".into(),
            marker: "Cargo.toml".into()
        })
    );
    assert_eq!(
        core.suggest_profile(&next),
        Some(Suggestion {
            profile: "Next.js app".into(),
            marker: "next.config.ts".into()
        })
    );
    assert_eq!(core.suggest_profile(&plain), None);
}

#[test]
fn repo_overrides_win_over_the_profile_field_by_field() {
    let base = ProfileSettings {
        agent: AgentDefaults {
            default_agent: Some(Agent::Claude),
            model: Some("opus".into()),
            effort: Some("medium".into()),
            ..AgentDefaults::default()
        },
        setup: Some("cargo fetch".into()),
        run_sessions: Some(vec![run_session("tests", "cargo test")]),
        launch_args: LaunchArgs {
            claude: Some(vec!["--verbose".into()]),
            codex: Some(vec!["--search".into()]),
        },
        ..ProfileSettings::default()
    };
    let overrides = ProfileSettings {
        agent: AgentDefaults {
            model: Some("sonnet".into()),
            ..AgentDefaults::default()
        },
        // An empty list still replaces the profile's.
        run_sessions: Some(Vec::new()),
        launch_args: LaunchArgs {
            codex: Some(vec!["--add-dir".into(), "../shared".into()]),
            ..LaunchArgs::default()
        },
        ..ProfileSettings::default()
    };

    let merged = overrides.applied_to(base);

    assert_eq!(
        merged,
        ProfileSettings {
            agent: AgentDefaults {
                default_agent: Some(Agent::Claude),
                model: Some("sonnet".into()),
                effort: Some("medium".into()),
                ..AgentDefaults::default()
            },
            setup: Some("cargo fetch".into()),
            run_sessions: Some(Vec::new()),
            launch_args: LaunchArgs {
                claude: Some(vec!["--verbose".into()]),
                codex: Some(vec!["--add-dir".into(), "../shared".into()]),
            },
            ..ProfileSettings::default()
        }
    );
}

#[test]
fn a_repo_runs_its_profile_run_sessions_until_it_overrides_them() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &[]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let mine = Profile {
        name: "Mine".into(),
        suggest_for: Vec::new(),
        settings: ProfileSettings {
            run_sessions: Some(vec![run_session("hello", "true")]),
            ..ProfileSettings::default()
        },
    };
    block_on(core.set_profile(mine, None)).unwrap();
    let picked = RepoSettings {
        profile: Some("Mine".into()),
        overrides: ProfileSettings::default(),
    };
    block_on(core.set_repo_settings(&repo, picked)).unwrap();
    let spec = |name: &str| RunSpec {
        name: name.into(),
        cols: 80,
        rows: 24,
    };

    core.spawn_run(&repo, spec("hello")).unwrap();

    let overridden = RepoSettings {
        profile: Some("Mine".into()),
        overrides: ProfileSettings {
            run_sessions: Some(vec![run_session("bye", "true")]),
            ..ProfileSettings::default()
        },
    };
    block_on(core.set_repo_settings(&repo, overridden)).unwrap();
    let err = core.spawn_run(&repo, spec("hello")).unwrap_err();
    assert!(
        matches!(err, CoreError::UnknownRunSession { .. }),
        "{err:?}"
    );
    core.spawn_run(&repo, spec("bye")).unwrap();
}

#[test]
fn a_repo_cannot_pick_a_profile_that_does_not_exist() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &[]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let settings = RepoSettings {
        profile: Some("Nope".into()),
        overrides: ProfileSettings::default(),
    };

    let err = block_on(core.set_repo_settings(&repo, settings)).unwrap_err();

    assert!(
        matches!(&err, CoreError::UnknownProfile(name) if name == "Nope"),
        "{err:?}"
    );
    assert_eq!(core.repo_settings(&repo).unwrap(), RepoSettings::default());
}

#[test]
fn the_chosen_profile_and_overrides_survive_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &["Cargo.toml"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let settings = RepoSettings {
        profile: Some("Rust crate".into()),
        overrides: ProfileSettings {
            agent: AgentDefaults {
                model: Some("sonnet".into()),
                ..AgentDefaults::default()
            },
            ..ProfileSettings::default()
        },
    };
    block_on(core.set_repo_settings(&repo, settings.clone())).unwrap();
    drop(core);

    let (core, _events) = start(dir.path());

    assert_eq!(core.repo_settings(&repo).unwrap(), settings);
}

#[test]
fn set_profile_writes_a_file_that_a_restarted_core_reads_back() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());
    let mut rust = profile(&core, "Rust crate");
    rust.settings.agent.model = Some("sonnet".into());
    rust.settings.launch_args.claude = Some(vec!["--verbose".into()]);
    let mine = Profile {
        name: "Go service".into(),
        suggest_for: vec!["go.mod".into()],
        settings: ProfileSettings::default(),
    };

    block_on(core.set_profile(rust.clone(), None)).unwrap();
    block_on(core.set_profile(mine.clone(), None)).unwrap();

    assert_eq!(profile(&core, "Rust crate"), rust);
    drop(core);
    let (core, _events) = start(dir.path());
    assert_eq!(profile(&core, "Rust crate"), rust);
    assert_eq!(profile(&core, "Go service"), mine);
    assert!(profiles_dir(dir.path()).join("go-service.toml").is_file());
}

#[test]
fn set_profile_refuses_launch_args_that_would_replace_what_grove_passes() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());
    let before = profile(&core, "Rust crate");
    let mut rust = before.clone();
    rust.settings.launch_args.claude = Some(vec!["--settings".into(), "x.json".into()]);

    let err = block_on(core.set_profile(rust, None)).unwrap_err();

    assert!(
        matches!(&err, CoreError::InvalidLaunchArgs { agent: Agent::Claude, arg } if arg == "--settings"),
        "{err:?}"
    );
    assert_eq!(profile(&core, "Rust crate"), before);
}

// A file name comes from the name only when set_profile makes a new file.
#[test]
fn a_profile_name_with_no_ascii_letter_or_digit_is_read_but_never_named_into_a_new_file() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());
    drop(core);
    fs::write(
        profiles_dir(dir.path()).join("jp.toml"),
        "name = \"日本語\"\n",
    )
    .unwrap();
    let (core, _events) = start(dir.path());

    let mut jp = profile(&core, "日本語");
    jp.settings.setup = Some("make".into());
    block_on(core.set_profile(jp.clone(), None)).unwrap();
    assert_eq!(profile(&core, "日本語"), jp);

    let new = Profile {
        name: "中文".into(),
        suggest_for: Vec::new(),
        settings: ProfileSettings::default(),
    };
    let err = block_on(core.set_profile(new, None)).unwrap_err();
    assert!(
        matches!(&err, CoreError::InvalidProfileName(name) if name == "中文"),
        "{err:?}"
    );
    assert!(err.to_string().contains("ASCII letter or digit"), "{err}");
}

#[test]
fn editing_a_profile_file_on_disk_sends_profiles_changed_with_no_by() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events) = start(dir.path());
    let path = profiles_dir(dir.path()).join("rust-crate.toml");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("\"opus\"", "\"sonnet\"");

    fs::write(&path, edited).unwrap();

    assert_eq!(wait_profiles_changed(&mut events), None);
    assert_eq!(
        profile(&core, "Rust crate").settings.agent.model.as_deref(),
        Some("sonnet")
    );
}

#[test]
fn set_profile_with_by_sends_exactly_one_profiles_changed_naming_that_session() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &[]);
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let spec = SpawnSpec {
        command: "/bin/sleep".into(),
        args: vec!["30".into()],
        cols: 80,
        rows: 24,
    };
    let session = core.spawn(&repo, spec).unwrap();
    let mut rust = profile(&core, "Rust crate");
    rust.settings.agent.model = Some("sonnet".into());

    block_on(core.set_profile(rust, Some(session))).unwrap();

    // Long enough for the watcher to see the write.
    assert_eq!(
        profiles_changed_within(&mut events, Duration::from_secs(1)),
        [Some(session)]
    );
    // The watcher is still running: an edit on disk after it gets its own event.
    let path = profiles_dir(dir.path()).join("rust-crate.toml");
    fs::write(&path, "name = \"Rust crate\"\n").unwrap();
    assert_eq!(wait_profiles_changed(&mut events), None);
}

// Profile panel (#57): profiles and overrides as TOML text.

#[test]
fn profile_text_gives_the_file_as_grove_read_it_with_its_comments() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());
    let path = profiles_dir(dir.path()).join("rust-crate.toml");

    let text = core.profile_text("Rust crate").unwrap();

    assert_eq!(
        text,
        ProfileText {
            path: path.clone(),
            text: fs::read_to_string(&path).unwrap(),
        }
    );
    assert!(
        text.text.starts_with("# A starter profile"),
        "{}",
        text.text
    );
    let err = core.profile_text("Go service").unwrap_err();
    assert!(
        matches!(&err, CoreError::UnknownProfile(name) if name == "Go service"),
        "{err:?}"
    );
}

#[test]
fn set_profile_text_keeps_the_text_as_typed_and_sends_exactly_one_profiles_changed() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events) = start(dir.path());
    let path = profiles_dir(dir.path()).join("rust-crate.toml");
    let text = "# Mine now.\nname = \"Rust crate\" # keep\nsetup = \"cargo build\"\n";

    block_on(core.set_profile_text("Rust crate", text.into(), None)).unwrap();

    // Long enough for the watcher to see the write.
    assert_eq!(
        profiles_changed_within(&mut events, Duration::from_secs(1)),
        [None]
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    assert_eq!(core.profile_text("Rust crate").unwrap().text, text);
    let rust = profile(&core, "Rust crate");
    assert_eq!(rust.settings.setup.as_deref(), Some("cargo build"));
    assert_eq!(rust.settings.agent, AgentDefaults::default());
    drop(core);
    let (core, _events) = start(dir.path());
    assert_eq!(profile(&core, "Rust crate"), rust);
}

#[test]
fn set_profile_text_refuses_text_that_is_not_valid_at_its_line_and_saves_nothing() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events) = start(dir.path());
    let path = profiles_dir(dir.path()).join("rust-crate.toml");
    let before = fs::read_to_string(&path).unwrap();

    let cases = [
        // An unknown field.
        (
            "name = \"Rust crate\"\n\n[agent]\nmodle = \"opus\"\n",
            Some("agent.modle"),
            Some(4),
            "unknown field `modle`",
        ),
        // Not TOML.
        (
            "name = \"Rust crate\"\n[agent\n",
            None,
            Some(2),
            "unclosed table",
        ),
        // TOML, but a run session with no name.
        (
            "name = \"Rust crate\"\n[[run_session]]\nname = \"\"\ncommand = \"x\"\n",
            Some("run_session"),
            None,
            "has no name",
        ),
    ];
    for (text, want_field, want_line, want_reason) in cases {
        let err = block_on(core.set_profile_text("Rust crate", text.into(), None)).unwrap_err();

        match &err {
            CoreError::BadProfile {
                path: at,
                field,
                line,
                reason,
            } => {
                assert_eq!(at, &path, "{text}");
                assert_eq!(field.as_deref(), want_field, "{text}");
                assert_eq!(*line, want_line, "{text}");
                assert!(reason.contains(want_reason), "{text}: {reason}");
            }
            _ => panic!("{text}: {err:?}"),
        }
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    assert_eq!(
        profiles_changed_within(&mut events, Duration::from_millis(500)),
        []
    );
}

// A save names the profile it edits, so a typo in `name` can never overwrite another one.
#[test]
fn set_profile_text_refuses_to_rename_the_profile_at_the_name_line() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, _events) = start(dir.path());
    let next = core.profile_text("Next.js app").unwrap();
    let rust = core.profile_text("Rust crate").unwrap();

    let err = block_on(core.set_profile_text(
        "Rust crate",
        "# Renamed.\nname = \"Next.js app\"\n".into(),
        None,
    ))
    .unwrap_err();

    assert!(
        matches!(&err, CoreError::BadProfile { field: Some(field), line: Some(2), reason, .. }
            if field == "name" && reason.contains("renaming")),
        "{err:?}"
    );
    assert_eq!(core.profile_text("Next.js app").unwrap(), next);
    assert_eq!(core.profile_text("Rust crate").unwrap(), rust);
    let err = block_on(core.set_profile_text("Go service", "name = \"Go service\"\n".into(), None))
        .unwrap_err();
    assert!(matches!(&err, CoreError::UnknownProfile(_)), "{err:?}");
}

#[test]
fn the_overrides_text_holds_only_what_the_repo_overrides() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &[]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    assert_eq!(core.repo_overrides_text(&repo).unwrap(), "");

    let overrides = ProfileSettings {
        agent: AgentDefaults {
            model: Some("haiku".into()),
            ..AgentDefaults::default()
        },
        setup: Some("make".into()),
        run_sessions: Some(Vec::new()),
        ..ProfileSettings::default()
    };
    let settings = RepoSettings {
        profile: Some("Rust crate".into()),
        overrides,
    };
    block_on(core.set_repo_settings(&repo, settings)).unwrap();

    assert_eq!(
        core.repo_overrides_text(&repo).unwrap(),
        "setup = \"make\"\nrun_session = []\n\n[agent]\nmodel = \"haiku\"\n"
    );
    let err = core
        .repo_overrides_text(&dir.path().join("elsewhere"))
        .unwrap_err();
    assert!(matches!(&err, CoreError::UnknownRepo(_)), "{err:?}");
}

#[test]
fn set_repo_overrides_text_keeps_the_repo_profile_and_survives_a_restart() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &[]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let settings = RepoSettings {
        profile: Some("Rust crate".into()),
        overrides: ProfileSettings::default(),
    };
    block_on(core.set_repo_settings(&repo, settings)).unwrap();

    let text = "# Comments go.\nsetup = \"make\"\n\n[agent]\nmodel = \"haiku\"\n";
    block_on(core.set_repo_overrides_text(&repo, text)).unwrap();

    let want = RepoSettings {
        profile: Some("Rust crate".into()),
        overrides: ProfileSettings {
            agent: AgentDefaults {
                model: Some("haiku".into()),
                ..AgentDefaults::default()
            },
            setup: Some("make".into()),
            ..ProfileSettings::default()
        },
    };
    assert_eq!(core.repo_settings(&repo).unwrap(), want);
    assert_eq!(
        core.repo_overrides_text(&repo).unwrap(),
        "setup = \"make\"\n\n[agent]\nmodel = \"haiku\"\n"
    );
    drop(core);
    let (core, _events) = start(dir.path());
    assert_eq!(core.repo_settings(&repo).unwrap(), want);

    block_on(core.set_repo_overrides_text(&repo, "")).unwrap();
    assert_eq!(core.repo_overrides_text(&repo).unwrap(), "");
}

#[test]
fn set_repo_overrides_text_refuses_text_that_is_not_valid_at_its_line_and_saves_nothing() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &[]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.set_repo_overrides_text(&repo, "setup = \"make\"\n")).unwrap();
    let before = core.repo_settings(&repo).unwrap();

    let cases = [
        // A profile's own field: overrides have no name.
        (
            "setup = \"x\"\nname = \"Mine\"\n",
            Some("name"),
            Some(2),
            "unknown field `name`",
        ),
        (
            "[agent]\nmodel = 1\n",
            Some("agent.model"),
            Some(2),
            "invalid type",
        ),
        (
            "[launch_args]\nclaude = [\"--settings\"]\n",
            Some("launch_args.claude"),
            None,
            "--settings",
        ),
    ];
    for (text, want_field, want_line, want_reason) in cases {
        let err = block_on(core.set_repo_overrides_text(&repo, text)).unwrap_err();

        match &err {
            CoreError::BadOverrides {
                field,
                line,
                reason,
            } => {
                assert_eq!(field.as_deref(), want_field, "{text}");
                assert_eq!(*line, want_line, "{text}");
                assert!(reason.contains(want_reason), "{text}: {reason}");
            }
            _ => panic!("{text}: {err:?}"),
        }
    }
    assert_eq!(core.repo_settings(&repo).unwrap(), before);
}

// Profile repair (#174): a profile file made invalid outside grove can be read and fixed.

// Writes `text` to `file` in the profiles folder and waits for the core to reread it. Events from
// earlier writes are dropped first, and the write is one rename, so the wait never ends on a
// stale event or on a reread of a half-written file. The temp file is hidden, so the core skips it.
fn write_on_disk(dir: &Path, events: &mut broadcast::Receiver<CoreEvent>, file: &str, text: &str) {
    while events.try_recv().is_ok() {}
    let folder = profiles_dir(dir);
    let temp = folder.join(format!(".{file}.test"));
    fs::write(&temp, text).unwrap();
    fs::rename(&temp, folder.join(file)).unwrap();
    wait_profiles_changed(events);
}

const BROKEN: &str = "name = \"Mine\"\n[agent\n";

#[test]
fn a_profile_that_broke_on_disk_after_it_was_valid_can_be_read_and_repaired() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events) = start(dir.path());
    // Not named after the profile, so only its remembered name finds it.
    let path = profiles_dir(dir.path()).join("custom.toml");
    write_on_disk(dir.path(), &mut events, "custom.toml", "name = \"Mine\"\n");
    write_on_disk(dir.path(), &mut events, "custom.toml", BROKEN);

    assert_eq!(
        core.profile_text("Mine").unwrap(),
        ProfileText {
            path: path.clone(),
            text: BROKEN.into(),
        }
    );

    let fixed = "# Fixed.\nname = \"Mine\"\nsetup = \"make\"\n";
    block_on(core.set_profile_text("Mine", fixed.into(), None)).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), fixed);
    assert_eq!(
        profile(&core, "Mine").settings.setup.as_deref(),
        Some("make")
    );
}

#[test]
fn a_profile_invalid_since_start_is_found_by_its_file_name_and_repaired() {
    let _serial = serial();
    let dir = short_temp_dir();
    drop(start(dir.path()));
    let path = profiles_dir(dir.path()).join("go-service.toml");
    let broken = "name = \"Go service\"\nsetup = \n";
    fs::write(&path, broken).unwrap();
    let (core, _events) = start(dir.path());

    assert_eq!(core.profile_text("Go service").unwrap().text, broken);
    let err = core.profile_text("Go").unwrap_err();
    assert!(matches!(&err, CoreError::UnknownProfile(_)), "{err:?}");

    let fixed = "name = \"Go service\"\nsetup = \"go mod download\"\n";
    block_on(core.set_profile_text("Go service", fixed.into(), None)).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), fixed);
    assert_eq!(
        profile(&core, "Go service").settings.setup.as_deref(),
        Some("go mod download")
    );
}

// The valid profile of a name always wins over a broken file that shares the name.
#[test]
fn a_repair_never_writes_over_a_broken_file_when_a_valid_profile_has_the_name() {
    let _serial = serial();
    let dir = short_temp_dir();
    drop(start(dir.path()));
    let profiles = profiles_dir(dir.path());
    // The valid "Rust crate" moves to another file, and its old file name is broken.
    let mine = profiles.join("mine.toml");
    fs::rename(profiles.join("rust-crate.toml"), &mine).unwrap();
    let stem_broken = "name = \"Rust crate\"\n[agent\n";
    fs::write(profiles.join("rust-crate.toml"), stem_broken).unwrap();
    let (core, mut events) = start(dir.path());
    // A broken copy that was valid, so its remembered name is "Rust crate" too.
    let copy = fs::read_to_string(&mine).unwrap();
    write_on_disk(dir.path(), &mut events, "copy.toml", &copy);
    let copy_broken = format!("{copy}[agent\n");
    write_on_disk(dir.path(), &mut events, "copy.toml", &copy_broken);

    assert_eq!(core.profile_text("Rust crate").unwrap().path, mine);

    let fixed = "name = \"Rust crate\"\nsetup = \"make\"\n";
    block_on(core.set_profile_text("Rust crate", fixed.into(), None)).unwrap();

    assert_eq!(fs::read_to_string(&mine).unwrap(), fixed);
    assert_eq!(
        fs::read_to_string(profiles.join("rust-crate.toml")).unwrap(),
        stem_broken
    );
    assert_eq!(
        fs::read_to_string(profiles.join("copy.toml")).unwrap(),
        copy_broken
    );
}

// A broken file is found by the name it had when last valid, never by an older one, and never by
// its file name once it has been valid.
#[test]
fn a_broken_file_answers_only_to_the_name_it_had_when_last_valid() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events) = start(dir.path());
    let path = profiles_dir(dir.path()).join("alpha.toml");
    write_on_disk(dir.path(), &mut events, "alpha.toml", "name = \"Alpha\"\n");
    write_on_disk(
        dir.path(),
        &mut events,
        "alpha.toml",
        "name = \"Alpha\"\n[x\n",
    );
    write_on_disk(dir.path(), &mut events, "alpha.toml", "name = \"Beta\"\n");
    let broken = "name = \"Beta\"\n[x\n";
    write_on_disk(dir.path(), &mut events, "alpha.toml", broken);

    let err = core.profile_text("Alpha").unwrap_err();
    assert!(matches!(&err, CoreError::UnknownProfile(_)), "{err:?}");
    let err =
        block_on(core.set_profile_text("Alpha", "name = \"Alpha\"\n".into(), None)).unwrap_err();
    assert!(matches!(&err, CoreError::UnknownProfile(_)), "{err:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), broken);

    assert_eq!(core.profile_text("Beta").unwrap().text, broken);
}

// Two broken files that remember one name: the first by path answers to it, and a repair writes
// only that one.
#[test]
fn of_two_broken_files_with_one_remembered_name_only_the_first_is_read_and_repaired() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events) = start(dir.path());
    let (a, b) = (
        profiles_dir(dir.path()).join("a.toml"),
        profiles_dir(dir.path()).join("b.toml"),
    );
    write_on_disk(dir.path(), &mut events, "a.toml", "name = \"Mine\"\n");
    write_on_disk(dir.path(), &mut events, "b.toml", "name = \"Mine\"\n");
    let broken_a = "name = \"Mine\"\n[a\n";
    let broken_b = "name = \"Mine\"\n[b\n";
    write_on_disk(dir.path(), &mut events, "a.toml", broken_a);
    write_on_disk(dir.path(), &mut events, "b.toml", broken_b);

    assert_eq!(
        core.profile_text("Mine").unwrap(),
        ProfileText {
            path: a.clone(),
            text: broken_a.into(),
        }
    );

    let fixed = "name = \"Mine\"\nsetup = \"make\"\n";
    block_on(core.set_profile_text("Mine", fixed.into(), None)).unwrap();

    assert_eq!(fs::read_to_string(&a).unwrap(), fixed);
    assert_eq!(fs::read_to_string(&b).unwrap(), broken_b);
}

#[test]
fn a_repair_still_refuses_to_rename_the_profile() {
    let _serial = serial();
    let dir = short_temp_dir();
    let (core, mut events) = start(dir.path());
    let path = profiles_dir(dir.path()).join("custom.toml");
    write_on_disk(dir.path(), &mut events, "custom.toml", "name = \"Mine\"\n");
    write_on_disk(dir.path(), &mut events, "custom.toml", BROKEN);
    let rust = core.profile_text("Rust crate").unwrap();

    let err = block_on(core.set_profile_text("Mine", "name = \"Rust crate\"\n".into(), None))
        .unwrap_err();

    assert!(
        matches!(&err, CoreError::BadProfile { field: Some(field), line: Some(1), reason, .. }
            if field == "name" && reason.contains("renaming")),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), BROKEN);
    assert_eq!(core.profile_text("Rust crate").unwrap(), rust);
}

// Repo settings sync (#174): each change to a repo's settings sends one event naming the repo.

// Every `RepoSettingsChanged` within `window`.
fn repo_settings_changed_within(
    events: &mut broadcast::Receiver<CoreEvent>,
    window: Duration,
) -> Vec<(PathBuf, Option<SessionId>)> {
    let end = Instant::now() + window;
    let mut seen = Vec::new();
    while Instant::now() < end {
        match events.try_recv() {
            Ok(CoreEvent::RepoSettingsChanged { repo, by }) => seen.push((repo, by)),
            Ok(_) => {}
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("{e}"),
        }
    }
    seen
}

#[test]
fn set_repo_settings_and_set_repo_overrides_text_each_send_one_repo_settings_changed() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &[]);
    let other = new_repo(dir.path(), "other", &[]);
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_repo(&other)).unwrap();
    let window = Duration::from_millis(300);
    repo_settings_changed_within(&mut events, window);
    let settings = RepoSettings {
        profile: Some("Rust crate".into()),
        overrides: ProfileSettings::default(),
    };

    block_on(core.set_repo_settings(&repo, settings)).unwrap();
    assert_eq!(
        repo_settings_changed_within(&mut events, window),
        [(repo.clone(), None)]
    );

    block_on(core.set_repo_overrides_text(&other, "setup = \"make\"\n")).unwrap();
    assert_eq!(
        repo_settings_changed_within(&mut events, window),
        [(other.clone(), None)]
    );
}

#[test]
fn a_failed_change_to_repo_settings_sends_no_repo_settings_changed() {
    let _serial = serial();
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), "repo", &[]);
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let unknown = RepoSettings {
        profile: Some("Nope".into()),
        overrides: ProfileSettings::default(),
    };

    block_on(core.set_repo_settings(&repo, unknown)).unwrap_err();
    block_on(core.set_repo_overrides_text(&repo, "[agent]\nmodel = 1\n")).unwrap_err();
    let untracked = dir.path().join("elsewhere");
    block_on(core.set_repo_settings(&untracked, RepoSettings::default())).unwrap_err();

    assert_eq!(
        repo_settings_changed_within(&mut events, Duration::from_millis(300)),
        []
    );
}
