//! Profiles: reusable setups for a kind of project, as TOML files in the home folder's
//! `profiles/` (ADR 0016). A repo picks one in its repo settings and may override parts of it.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use toml::de::{DeTable, DeValue};

use crate::agent::Agents;
use crate::panel_request::Refused;
use crate::remove::repo_of;
use crate::run::check_run_sessions;
use crate::{Agent, Core, CoreError, CoreEvent, LaunchArgs, Result, RunSession, SessionId};

/// A reusable setup for a kind of project, saved as a TOML file in `profiles/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Unique among the profiles. A new profile's file is named after it, as in
    /// `rust-crate.toml`.
    pub name: String,
    /// Marker files, relative to a repo's main worktree. `suggest_profile` names this profile for
    /// a repo that has one.
    pub suggest_for: Vec<String>,
    pub settings: ProfileSettings,
}

/// What a profile sets, and what a repo overrides. Each field the repo sets wins over the
/// profile's (`applied_to`). A field neither sets falls back to the agent's own config or grove's
/// default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSettings {
    #[serde(default)]
    pub agent: AgentDefaults,
    #[serde(default)]
    pub layout: Layout,
    /// Runs in a new worktree before agents start.
    pub setup: Option<String>,
    /// Runs in a worktree before it is deleted.
    pub cleanup: Option<String>,
    /// A list replaces the profile's whole list.
    #[serde(rename = "run_session")]
    pub run_sessions: Option<Vec<RunSession>>,
    #[serde(default)]
    pub launch_args: LaunchArgs,
    #[serde(default)]
    pub review: Review,
    #[serde(default)]
    pub notify: Notify,
}

/// The agent new agent sessions get, and how they start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefaults {
    #[serde(rename = "default")]
    pub default_agent: Option<Agent>,
    /// As the agent names it, such as `opus`.
    pub model: Option<String>,
    /// As the agent names it, such as `medium`.
    pub effort: Option<String>,
    /// As the agent names it, such as `default`.
    pub permission_mode: Option<String>,
    /// The first message of each new agent session.
    pub start_prompt: Option<String>,
}

/// The workspace a new worktree starts with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// The agent panels in the chat area, in order.
    pub chat: Option<Vec<Agent>>,
    /// The panel area's tabs, in order.
    pub panels: Option<Vec<PanelKind>>,
    /// Whether the terminal panel is open at the bottom of the panel area.
    pub terminal: Option<bool>,
}

/// A panel the panel area can show as a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelKind {
    Files,
    Diff,
    Browser,
    Logs,
    Profile,
}

/// How review batches go out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub send_to: Option<ReviewTarget>,
}

/// The agent session a review batch goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTarget {
    /// The worktree's most recently focused agent session.
    LastFocused,
}

/// Which agent states send a notification.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notify {
    pub states: Option<Vec<AgentStateKind>>,
}

/// An agent state without its details, as a profile names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStateKind {
    Working,
    NeedsInput,
    Failed,
    Done,
    Idle,
    Exited,
}

impl ProfileSettings {
    /// `base` with every field these settings set replaced by theirs. A list replaces the whole
    /// list, even when it is empty.
    pub fn applied_to(self, base: ProfileSettings) -> ProfileSettings {
        ProfileSettings {
            agent: AgentDefaults {
                default_agent: self.agent.default_agent.or(base.agent.default_agent),
                model: self.agent.model.or(base.agent.model),
                effort: self.agent.effort.or(base.agent.effort),
                permission_mode: self.agent.permission_mode.or(base.agent.permission_mode),
                start_prompt: self.agent.start_prompt.or(base.agent.start_prompt),
            },
            layout: Layout {
                chat: self.layout.chat.or(base.layout.chat),
                panels: self.layout.panels.or(base.layout.panels),
                terminal: self.layout.terminal.or(base.layout.terminal),
            },
            setup: self.setup.or(base.setup),
            cleanup: self.cleanup.or(base.cleanup),
            run_sessions: self.run_sessions.or(base.run_sessions),
            launch_args: LaunchArgs {
                claude: self.launch_args.claude.or(base.launch_args.claude),
                codex: self.launch_args.codex.or(base.launch_args.codex),
            },
            review: Review {
                send_to: self.review.send_to.or(base.review.send_to),
            },
            notify: Notify {
                states: self.notify.states.or(base.notify.states),
            },
        }
    }
}

// The schema of a profile file: a profile's own fields and its settings, all at the top level.
// Plain values come before tables, as TOML needs.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    suggest_for: Vec<String>,
    setup: Option<String>,
    cleanup: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    agent: AgentDefaults,
    #[serde(default, skip_serializing_if = "is_default")]
    layout: Layout,
    run_session: Option<Vec<RunSession>>,
    #[serde(default, skip_serializing_if = "is_default")]
    launch_args: LaunchArgs,
    #[serde(default, skip_serializing_if = "is_default")]
    review: Review,
    #[serde(default, skip_serializing_if = "is_default")]
    notify: Notify,
}

// The schema of a repo's overrides as text: a profile file without `name` and `suggest_for`.
// `ProfileSettings` reads it. This writes it without the empty tables.
#[derive(Serialize)]
struct OverridesFile {
    setup: Option<String>,
    cleanup: Option<String>,
    #[serde(skip_serializing_if = "is_default")]
    agent: AgentDefaults,
    #[serde(skip_serializing_if = "is_default")]
    layout: Layout,
    run_session: Option<Vec<RunSession>>,
    #[serde(skip_serializing_if = "is_default")]
    launch_args: LaunchArgs,
    #[serde(skip_serializing_if = "is_default")]
    review: Review,
    #[serde(skip_serializing_if = "is_default")]
    notify: Notify,
}

impl From<ProfileSettings> for OverridesFile {
    fn from(settings: ProfileSettings) -> Self {
        OverridesFile {
            setup: settings.setup,
            cleanup: settings.cleanup,
            agent: settings.agent,
            layout: settings.layout,
            run_session: settings.run_sessions,
            launch_args: settings.launch_args,
            review: settings.review,
            notify: settings.notify,
        }
    }
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

impl From<ProfileFile> for Profile {
    fn from(file: ProfileFile) -> Self {
        Profile {
            name: file.name,
            suggest_for: file.suggest_for,
            settings: ProfileSettings {
                agent: file.agent,
                layout: file.layout,
                setup: file.setup,
                cleanup: file.cleanup,
                run_sessions: file.run_session,
                launch_args: file.launch_args,
                review: file.review,
                notify: file.notify,
            },
        }
    }
}

impl From<Profile> for ProfileFile {
    fn from(profile: Profile) -> Self {
        let settings = profile.settings;
        ProfileFile {
            name: profile.name,
            suggest_for: profile.suggest_for,
            setup: settings.setup,
            cleanup: settings.cleanup,
            agent: settings.agent,
            layout: settings.layout,
            run_session: settings.run_sessions,
            launch_args: settings.launch_args,
            review: settings.review,
            notify: settings.notify,
        }
    }
}

// Written once, when grove makes the profiles folder. Named as `file_name` names them.
const STARTERS: [(&str, &str); 2] = [
    ("rust-crate.toml", RUST_CRATE),
    ("next-js-app.toml", NEXT_JS_APP),
];

const RUST_CRATE: &str = r#"# A starter profile grove wrote on first start. Edit it freely.
name = "Rust crate"
suggest_for = ["Cargo.toml"]
setup = "cargo fetch"
cleanup = "cargo clean"

[agent]
default = "claude"
model = "opus"
effort = "medium"
permission_mode = "default"

[layout]
chat = ["claude"]
panels = ["files", "diff"]
terminal = true

[[run_session]]
name = "tests"
command = "cargo test"

[review]
send_to = "last_focused"

[notify]
states = ["NeedsInput", "Failed"]
"#;

const NEXT_JS_APP: &str = r#"# A starter profile grove wrote on first start. Edit it freely.
name = "Next.js app"
suggest_for = ["next.config.js", "next.config.mjs", "next.config.ts"]
setup = "npm ci"
cleanup = "rm -rf .next"

[agent]
default = "claude"
model = "sonnet"
effort = "medium"
permission_mode = "default"

[layout]
chat = ["claude"]
panels = ["files", "diff", "browser"]
terminal = true

[[run_session]]
name = "dev"
command = "npm run dev"
port = 3000

[[run_session]]
name = "tests"
command = "npm test"

[review]
send_to = "last_focused"

[notify]
states = ["NeedsInput", "Failed"]
"#;

/// The profile files as grove last read or wrote them. A watcher rereads them when the folder
/// changes.
pub(crate) struct Profiles {
    dir: PathBuf,
    // Held across a whole write or reread, so a reread never sees a write half done.
    files: Mutex<BTreeMap<PathBuf, Loaded>>,
}

struct Loaded {
    // What a reread compares, so only a real change to the file sends `ProfilesChanged`.
    bytes: Vec<u8>,
    profile: std::result::Result<Profile, Invalid>,
    // The profile's name, or for a file that is not valid, the name it had when last valid. None
    // when it has not been valid since grove started.
    name: Option<String>,
}

// Why a profile file is not valid. The path is the file's key.
#[derive(Clone)]
struct Invalid {
    field: Option<String>,
    line: Option<usize>,
    reason: String,
}

impl Profiles {
    /// The profiles in `dir`, read later by `reread`. When `dir` does not exist yet, it makes it
    /// with the starter profiles in it. It blocks while it writes.
    pub(crate) fn open(dir: &Path) -> Result<Profiles> {
        if !dir.exists() {
            // Filled first and moved into place, so a crash never leaves only some starters.
            let temp = dir.with_extension("tmp");
            let _ = fs::remove_dir_all(&temp);
            fs::create_dir(&temp).map_err(CoreError::io("create the profiles folder", &temp))?;
            for (name, text) in STARTERS {
                let path = temp.join(name);
                fs::write(&path, text).map_err(CoreError::io("write the profile", &path))?;
            }
            fs::rename(&temp, dir).map_err(CoreError::io("create the profiles folder", dir))?;
        }
        Ok(Profiles {
            dir: dir.to_path_buf(),
            files: Mutex::default(),
        })
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Rereads the folder. Returns whether a file changed, came or went. A folder that cannot be
    /// read keeps the profiles read before. It blocks while it reads.
    pub(crate) fn reread(&self, agents: &Agents) -> bool {
        let mut files = self.files();
        let Ok(paths) = toml_files(&self.dir) else {
            return false;
        };
        let mut fresh = BTreeMap::new();
        for path in paths {
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                // Gone since the folder was listed.
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => {
                    let invalid = Invalid {
                        field: None,
                        line: None,
                        reason: e.to_string(),
                    };
                    let loaded = Loaded::new(Vec::new(), Err(invalid), files.get(&path));
                    fresh.insert(path, loaded);
                    continue;
                }
            };
            let old = files.get(&path);
            let profile = match old {
                Some(old) if old.bytes == bytes => old.profile.clone(),
                _ => parse(&bytes, agents),
            };
            fresh.insert(path, Loaded::new(bytes, profile, old));
        }
        let changed = fresh.len() != files.len()
            || fresh
                .iter()
                .any(|(path, new)| files.get(path).is_none_or(|old| old.bytes != new.bytes));
        *files = fresh;
        changed
    }

    /// Every profile, by name. Fails on the first file that is not valid.
    fn list(&self) -> Result<Vec<Profile>> {
        let files = self.files();
        let mut by_name: HashMap<&str, &Path> = HashMap::new();
        let mut profiles = Vec::new();
        for (path, loaded) in files.iter() {
            let profile = loaded.profile.as_ref().map_err(|e| e.of(path))?;
            if let Some(other) = by_name.insert(&profile.name, path) {
                return Err(CoreError::BadProfile {
                    path: path.clone(),
                    field: Some("name".into()),
                    line: None,
                    reason: format!("{} has a profile with the same name", other.display()),
                });
            }
            profiles.push(profile.clone());
        }
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(profiles)
    }

    /// Every valid profile, by name, skipping files that are not valid.
    pub(crate) fn valid(&self) -> Vec<Profile> {
        let mut profiles: Vec<Profile> = self
            .files()
            .values()
            .filter_map(|loaded| loaded.profile.clone().ok())
            .collect();
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
        profiles
    }

    pub(crate) fn get(&self, name: &str) -> Option<Profile> {
        self.valid().into_iter().find(|p| p.name == name)
    }

    /// The file of the profile named `name`, as `file_for` finds it, as last read or written.
    fn text(&self, name: &str) -> Result<ProfileText> {
        let files = self.files();
        let (path, loaded) = file_for(&self.dir, &files, name)
            .ok_or_else(|| CoreError::UnknownProfile(name.into()))?;
        Ok(ProfileText {
            path: path.clone(),
            // A file that is not valid may not be UTF-8.
            text: String::from_utf8_lossy(&loaded.bytes).into_owned(),
        })
    }

    /// The profile named `name`, or why its file is not valid, from the file `file_for` finds.
    fn profile(&self, name: &str) -> Result<Profile> {
        found(&self.dir, &self.files(), name)
    }

    // Writes `profile` over the file of the profile with its name, or to a new file named after
    // it. It blocks while it writes.
    fn write(&self, profile: Profile, agents: &Agents) -> Result<()> {
        self.write_in(&mut self.files(), profile, agents)
    }

    // Writes the profile named `name` with each setting `changes` sets replaced, over its file as
    // it is now: an edit the watcher has not reread yet is kept, and a file that is not valid is
    // refused, never written over. It blocks while it reads and writes.
    fn change(&self, name: &str, changes: ProfileSettings, agents: &Agents) -> Result<()> {
        let mut files = self.files();
        let (path, loaded) = file_for(&self.dir, &files, name)
            .ok_or_else(|| CoreError::UnknownProfile(name.into()))?;
        let path = path.clone();
        let bytes = fs::read(&path).map_err(CoreError::io("read the profile", &path))?;
        let now = if bytes == loaded.bytes {
            loaded.profile.clone()
        } else {
            parse(&bytes, agents)
        };
        let mut profile = now.map_err(|e| e.of(&path))?;
        if profile.name != name {
            return Err(CoreError::UnknownProfile(name.into()));
        }
        profile.settings = changes.applied_to(profile.settings);
        check(&profile.settings, agents)?;
        save(&mut files, path, profile)
    }

    // `write`, with the files already locked.
    fn write_in(
        &self,
        files: &mut BTreeMap<PathBuf, Loaded>,
        profile: Profile,
        agents: &Agents,
    ) -> Result<()> {
        check(&profile.settings, agents)?;
        let same_name = named(files, &profile.name).map(|(path, _)| path.clone());
        let path = match same_name {
            Some(path) => path,
            None => {
                let name = file_name(&profile.name);
                if name == ".toml" {
                    return Err(CoreError::InvalidProfileName(profile.name));
                }
                let path = self.dir.join(name);
                // Only a file that is not valid may be replaced: saving fixes it.
                let fixing = files.get(&path).is_some_and(|l| l.profile.is_err());
                if path.exists() && !fixing {
                    return Err(CoreError::FolderTaken(path));
                }
                path
            }
        };
        fs::create_dir_all(&self.dir)
            .map_err(CoreError::io("create the profiles folder", &self.dir))?;
        save(files, path, profile)
    }

    // Writes `text` as is over the file of the profile named `name`, as `file_for` finds it, which
    // repairs a file that is not valid. Refuses text that is not a valid profile or that names
    // another one. It blocks while it writes.
    fn write_text(&self, name: &str, text: String, agents: &Agents) -> Result<()> {
        let mut files = self.files();
        let path = file_for(&self.dir, &files, name)
            .map(|(path, _)| path.clone())
            .ok_or_else(|| CoreError::UnknownProfile(name.into()))?;
        let profile = parse(text.as_bytes(), agents).map_err(|e| e.of(&path))?;
        if profile.name != name {
            return Err(CoreError::BadProfile {
                path,
                field: Some("name".into()),
                line: line_of_key(&text, "name"),
                reason: format!("it must stay {name:?}: renaming a profile is not supported"),
            });
        }
        // The cache holds the bytes written, so the watcher's reread sees no change.
        write_whole(&path, text.as_bytes())?;
        files.insert(path, Loaded::new(text.into_bytes(), Ok(profile), None));
        Ok(())
    }

    fn files(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Loaded>> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// Writes `profile` to `path` and keeps what it wrote, so the watcher's reread sees no change.
fn save(files: &mut BTreeMap<PathBuf, Loaded>, path: PathBuf, profile: Profile) -> Result<()> {
    let text = toml::to_string(&ProfileFile::from(profile.clone()))
        .map_err(|e| CoreError::io("encode the profile for", &path)(io::Error::other(e)))?;
    write_whole(&path, text.as_bytes())?;
    files.insert(path, Loaded::new(text.into_bytes(), Ok(profile), None));
    Ok(())
}

// The profile in the file `file_for` finds for `name`, or why that file is not valid.
fn found(dir: &Path, files: &BTreeMap<PathBuf, Loaded>, name: &str) -> Result<Profile> {
    let (path, loaded) =
        file_for(dir, files, name).ok_or_else(|| CoreError::UnknownProfile(name.into()))?;
    loaded.profile.clone().map_err(|e| e.of(path))
}

// The first file whose valid profile is named `name`, as `get` finds it.
fn named<'a>(
    files: &'a BTreeMap<PathBuf, Loaded>,
    name: &str,
) -> Option<(&'a PathBuf, &'a Loaded)> {
    files
        .iter()
        .find(|(_, loaded)| loaded.profile.as_ref().is_ok_and(|p| p.name == name))
}

// The file the Profile panel reads and saves for `name`: the valid profile of that name, else a
// file that is not valid and had that name when last valid, else one that has not been valid since
// grove started and is named after it, as a new profile's file would be. So a repair never writes
// over a valid profile's file.
fn file_for<'a>(
    dir: &Path,
    files: &'a BTreeMap<PathBuf, Loaded>,
    name: &str,
) -> Option<(&'a PathBuf, &'a Loaded)> {
    let broken = |loaded: &Loaded| loaded.profile.is_err();
    named(files, name)
        .or_else(|| {
            files
                .iter()
                .find(|(_, loaded)| broken(loaded) && loaded.name.as_deref() == Some(name))
        })
        .or_else(|| {
            files
                .get_key_value(&dir.join(file_name(name)))
                .filter(|(_, loaded)| broken(loaded) && loaded.name.is_none())
        })
}

/// A profile's file, as the Profile panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileText {
    /// Under `profiles/`.
    pub path: PathBuf,
    /// As grove last read or wrote it, comments included.
    pub text: String,
}

/// A profile `Core::suggest_profile` names for a repo, and the marker file that made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub profile: String,
    /// As the profile's `suggest_for` lists it, such as `Cargo.toml`.
    pub marker: String,
}

impl Core {
    /// Every profile, by name. Fails when a profile file is not valid, with an error that names
    /// the file and, where it can, the field and line.
    pub fn profiles(&self) -> Result<Vec<Profile>> {
        self.profiles.list()
    }

    /// Saves `profile` over the profile with its name, or as a new one. `by` is the agent session
    /// that changed it, None for the user. Sends one `ProfilesChanged` with `by`.
    pub async fn set_profile(&self, profile: Profile, by: Option<SessionId>) -> Result<()> {
        self.write_profiles(move |profiles, agents| profiles.write(profile, agents), by)
            .await
    }

    /// The file of the profile named `name`, comments included. When no valid profile has that name,
    /// a file that is not valid now: the one that had that name when last valid, or one named after
    /// it that has not been valid since grove started. UnknownProfile when there is none.
    pub fn profile_text(&self, name: &str) -> Result<ProfileText> {
        self.profiles.text(name)
    }

    /// Replaces the file `profile_text` finds for `name` with `text`, kept as typed, which repairs a
    /// file that is not valid. Saves nothing and fails with BadProfile, at the line where TOML
    /// gives one, when `text` is not a valid profile or names another one: renaming is refused, so
    /// a save never overwrites another profile. `by` and the event are as for `set_profile`.
    pub async fn set_profile_text(
        &self,
        name: &str,
        text: String,
        by: Option<SessionId>,
    ) -> Result<()> {
        let name = name.to_owned();
        self.write_profiles(
            move |profiles, agents| profiles.write_text(&name, text, agents),
            by,
        )
        .await
    }

    /// The profile of agent session `caller`'s repo, as grove-mcp gives it: its name and its
    /// settings, in the shape `change_agent_profile` takes.
    pub(crate) fn agent_profile(&self, caller: SessionId) -> std::result::Result<Value, Refused> {
        let profile = self.profiles.profile(&self.caller_profile(caller)?)?;
        Ok(json!({"name": profile.name, "settings": profile.settings}))
    }

    /// Applies `changes`, settings as a profile file has them, to the profile of agent session
    /// `caller`'s repo, with `caller` as `by`. Each setting `changes` sets replaces the profile's,
    /// and the rest stay. Returns the profile's name. A profile other repos use changes for them
    /// too.
    pub(crate) fn change_agent_profile(
        &self,
        caller: SessionId,
        changes: Value,
    ) -> std::result::Result<String, Refused> {
        let name = self.caller_profile(caller)?;
        let changes: ProfileSettings =
            serde_json::from_value(changes).map_err(|e| Refused::BadChanges(e.to_string()))?;
        let changed = name.clone();
        // The panel request thread is no runtime worker, so it may block on one.
        self.runtime.block_on(self.write_profiles(
            move |profiles, agents| profiles.change(&changed, changes, agents),
            Some(caller),
        ))?;
        Ok(name)
    }

    /// Writes the workspace of `worktree` into the layout of the profile its repo uses: the
    /// agents of its shown agent panels, its tabs and whether the terminal is open. Hidden panels
    /// and the selected tab are left out, as a layout has no place for them. A profile other
    /// repos use changes for them too. Sends one `ProfilesChanged` with no `by`. NoRepoProfile
    /// when the repo uses none.
    pub async fn save_workspace_to_profile(&self, worktree: &Path) -> Result<()> {
        let workspace = self.workspace(worktree)?;
        let repo = repo_of(&self.tracked(), worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?;
        let name = self
            .repo_settings(&repo)?
            .profile
            .ok_or(CoreError::NoRepoProfile(repo))?;
        let layout = Layout {
            chat: Some(
                workspace
                    .chat
                    .iter()
                    .filter(|panel| panel.shown)
                    .map(|panel| panel.agent)
                    .collect(),
            ),
            panels: Some(workspace.panels),
            terminal: Some(workspace.terminal),
        };
        let changes = ProfileSettings {
            layout,
            ..ProfileSettings::default()
        };
        self.write_profiles(
            move |profiles, agents| profiles.change(&name, changes, agents),
            None,
        )
        .await
    }

    // The name of the profile agent session `caller`'s repo uses.
    fn caller_profile(&self, caller: SessionId) -> std::result::Result<String, Refused> {
        let worktree = self.caller_worktree(caller)?;
        let repo = repo_of(&self.tracked(), &worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.clone()))?;
        let profile = self.repo_settings(&repo)?.profile;
        profile.ok_or(Refused::NoProfile(repo))
    }

    // Runs `write` off the runtime's workers, then sends one `ProfilesChanged` with `by`.
    async fn write_profiles(
        &self,
        write: impl FnOnce(&Profiles, &Agents) -> Result<()> + Send + 'static,
        by: Option<SessionId>,
    ) -> Result<()> {
        let (profiles, agents) = (self.profiles.clone(), self.agents.clone());
        let dir = profiles.dir().to_path_buf();
        self.runtime
            .spawn_blocking(move || write(&profiles, &agents))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("write a profile to", &dir)(io::Error::other(e)))??;
        let _ = self.events.send(CoreEvent::ProfilesChanged { by });
        Ok(())
    }

    /// The first profile, by name, with a marker file in the repo whose main worktree is at
    /// `repo`, and the first of its markers found there. None when no profile has one there.
    pub fn suggest_profile(&self, repo: &Path) -> Option<Suggestion> {
        self.profiles.valid().into_iter().find_map(|p| {
            let marker = p.suggest_for.iter().find(|m| repo.join(m).exists())?;
            Some(Suggestion {
                marker: marker.clone(),
                profile: p.name,
            })
        })
    }
}

impl Loaded {
    // `before` is what the file held before, whose name a file that is no longer valid keeps.
    fn new(
        bytes: Vec<u8>,
        profile: std::result::Result<Profile, Invalid>,
        before: Option<&Loaded>,
    ) -> Loaded {
        let name = match &profile {
            Ok(profile) => Some(profile.name.clone()),
            Err(_) => before.and_then(|before| before.name.clone()),
        };
        Loaded {
            bytes,
            profile,
            name,
        }
    }
}

impl Invalid {
    fn of(&self, path: &Path) -> CoreError {
        CoreError::BadProfile {
            path: path.to_path_buf(),
            field: self.field.clone(),
            line: self.line,
            reason: self.reason.clone(),
        }
    }

    fn of_overrides(self) -> CoreError {
        CoreError::BadOverrides {
            field: self.field,
            line: self.line,
            reason: self.reason,
        }
    }
}

// The `.toml` files in `dir`. Hidden ones, such as a write's temp file, are skipped. Fails when
// any entry cannot be listed or checked, so a reread never takes a profile for gone.
fn toml_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let hidden = path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        if hidden || path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => paths.push(path),
            Ok(_) => {}
            // Gone since the folder was listed.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(paths)
}

fn parse(bytes: &[u8], agents: &Agents) -> std::result::Result<Profile, Invalid> {
    let text = std::str::from_utf8(bytes).map_err(|e| Invalid {
        field: None,
        line: None,
        reason: e.to_string(),
    })?;
    let profile = Profile::from(from_toml::<ProfileFile>(text)?);
    checked(&profile.settings, agents)?;
    Ok(profile)
}

/// A repo's overrides from their text, in a profile file's schema without `name` and
/// `suggest_for`. Fails with BadOverrides.
pub(crate) fn parse_overrides(text: &str, agents: &Agents) -> Result<ProfileSettings> {
    let settings = from_toml::<ProfileSettings>(text).map_err(Invalid::of_overrides)?;
    checked(&settings, agents).map_err(Invalid::of_overrides)?;
    Ok(settings)
}

/// A repo's overrides as text, as `parse_overrides` reads them. Empty when they set nothing.
pub(crate) fn overrides_text(
    settings: ProfileSettings,
) -> std::result::Result<String, toml::ser::Error> {
    toml::to_string(&OverridesFile::from(settings))
}

// `text` as a T, or why not, with the field and line where TOML gives them.
fn from_toml<T: DeserializeOwned>(text: &str) -> std::result::Result<T, Invalid> {
    toml::from_str(text).map_err(|e| {
        let span = e.span();
        Invalid {
            field: span.clone().and_then(|span| field_at(text, span)),
            line: span.map(|span| line_at(text, span.start)),
            reason: e.message().to_owned(),
        }
    })
}

// `check`, with the field that failed it.
fn checked(settings: &ProfileSettings, agents: &Agents) -> std::result::Result<(), Invalid> {
    check(settings, agents).map_err(|e| Invalid {
        field: Some(match &e {
            CoreError::InvalidLaunchArgs { agent, .. } => format!("launch_args.{}", agent.name()),
            _ => "run_session".into(),
        }),
        line: None,
        reason: e.to_string(),
    })
}

// The same checks for settings from a file, from `set_profile` and from a repo's overrides.
fn check(settings: &ProfileSettings, agents: &Agents) -> Result<()> {
    let runs = settings.run_sessions.as_deref().unwrap_or_default();
    check_run_sessions(runs)?;
    agents.check_launch_args(&settings.launch_args)
}

// The line, from 1, that byte `offset` of `text` is on.
fn line_at(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

// The line of the top-level `key` in `text`. None when it has none.
fn line_of_key(text: &str, key: &str) -> Option<usize> {
    let doc = DeTable::parse(text).ok()?;
    let (found, _) = doc.get_ref().iter().find(|(k, _)| k.get_ref() == key)?;
    Some(line_at(text, found.span().start))
}

// The dotted key, as in `agent.model`, whose key or value holds `span`. TOML gives the span of
// a bad field, not its name.
fn field_at(text: &str, span: Range<usize>) -> Option<String> {
    // An empty span marks a whole table, as for a missing field. The reason names that field.
    if span.is_empty() {
        return None;
    }
    let doc = DeTable::parse(text).ok()?;
    let holds = |r: Range<usize>| r.start <= span.start && span.end <= r.end && !r.is_empty();
    let mut path = Vec::new();
    find_in_table(doc.get_ref(), &holds, &mut path).then(|| path.join("."))
}

fn find_in_table(
    table: &DeTable,
    holds: &dyn Fn(Range<usize>) -> bool,
    path: &mut Vec<String>,
) -> bool {
    for (key, value) in table {
        path.push(key.get_ref().to_string());
        if holds(key.span()) || find_in_value(value, holds, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn find_in_value(
    value: &toml::Spanned<DeValue>,
    holds: &dyn Fn(Range<usize>) -> bool,
    path: &mut Vec<String>,
) -> bool {
    match value.get_ref() {
        DeValue::Table(table) => find_in_table(table, holds, path) || holds(value.span()),
        DeValue::Array(items) => items.iter().any(|item| find_in_value(item, holds, path)),
        _ => holds(value.span()),
    }
}

// `name` in lowercase with each run of other characters than ASCII letters and digits as one
// `-`, as in `next-js-app.toml`.
fn file_name(name: &str) -> String {
    let mut stem = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            stem.push(c.to_ascii_lowercase());
        } else if !stem.is_empty() && !stem.ends_with('-') {
            stem.push('-');
        }
    }
    format!("{}.toml", stem.trim_end_matches('-'))
}

// Writes a temp file, flushes it and renames it over `path`, so the file always holds one whole
// write.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = path.with_file_name(format!(".{name}.tmp"));
    let mut file = File::create(&temp).map_err(CoreError::io("write the profile to", &temp))?;
    file.write_all(bytes)
        .map_err(CoreError::io("write the profile to", &temp))?;
    file.sync_all()
        .map_err(CoreError::io("flush the profile to", &temp))?;
    fs::rename(&temp, path).map_err(CoreError::io("move the new profile to", path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_keep_only_lowercase_letters_and_digits_between_dashes() {
        assert_eq!(file_name("Rust crate"), "rust-crate.toml");
        assert_eq!(file_name("Next.js app"), "next-js-app.toml");
        assert_eq!(file_name("  Go 1.24!  "), "go-1-24.toml");
        assert_eq!(file_name("..."), ".toml");
    }

    // A permission or I/O error on the folder says nothing about the profiles in it.
    #[test]
    fn a_folder_that_cannot_be_read_keeps_the_profiles_read_before() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let agents = Agents::new(
            &crate::CoreConfig::new(home.path()),
            Default::default(),
            std::sync::Arc::new(crate::warnings::Warnings::new(
                tokio::sync::broadcast::channel(1).0,
            )),
        );
        let dir = home.path().join("profiles");
        let profiles = Profiles::open(&dir).unwrap();
        assert!(profiles.reread(&agents));
        // 0o000 cannot be listed. 0o400 lists its entries but cannot read their metadata.
        for mode in [0o000, 0o400] {
            fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();

            let changed = profiles.reread(&agents);

            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(!changed, "{mode:o}");
            assert!(profiles.get("Rust crate").is_some(), "{mode:o}");
        }
    }

    // Without a watcher, nothing rereads the folder, so what grove read stays stale.
    #[test]
    fn a_change_starts_from_the_file_on_disk_not_from_what_grove_read() {
        let home = tempfile::tempdir().unwrap();
        let agents = Agents::new(
            &crate::CoreConfig::new(home.path()),
            Default::default(),
            std::sync::Arc::new(crate::warnings::Warnings::new(
                tokio::sync::broadcast::channel(1).0,
            )),
        );
        let dir = home.path().join("profiles");
        let profiles = Profiles::open(&dir).unwrap();
        assert!(profiles.reread(&agents));
        let path = dir.join("rust-crate.toml");
        let changes = ProfileSettings {
            agent: AgentDefaults {
                model: Some("sonnet".into()),
                ..AgentDefaults::default()
            },
            ..ProfileSettings::default()
        };
        let edited = fs::read_to_string(&path)
            .unwrap()
            .replace("effort = \"medium\"", "effort = \"high\"");
        fs::write(&path, &edited).unwrap();

        profiles
            .change("Rust crate", changes.clone(), &agents)
            .unwrap();

        let file = fs::read_to_string(&path).unwrap();
        assert!(file.contains("effort = \"high\""), "{file}");
        assert!(file.contains("model = \"sonnet\""), "{file}");

        let broken = "name = \"Rust crate\"\n[agent\n";
        fs::write(&path, broken).unwrap();

        let error = profiles.change("Rust crate", changes, &agents).unwrap_err();

        assert!(matches!(error, CoreError::BadProfile { .. }), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn the_starters_are_named_as_new_profiles_would_be() {
        for (name, text) in STARTERS {
            let file: ProfileFile = toml::from_str(text).unwrap();
            assert_eq!(file_name(&file.name), name);
        }
    }
}
