use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

use git2::ConfigLevel;
use grove_core::{
    Core, CoreConfig, CoreError, CoreEvent, DiffBase, DiffLine, FileDiff, FileStatus, FileVersions,
    Hunk, LineChange, LineKind, NewWorktree,
};
use tempfile::TempDir;
use tokio::sync::broadcast;

// libgit2 reads the user's global and system config, attributes and ignore file, and a global
// `core.autocrlf` or ignore rule would split it from the pinned git CLI. So this points those
// lookups at nothing, once per test process, before any repo opens. The one exception is a user
// config that defines a filter driver, `userzero`, as git-lfs defines `filter.lfs` there.
fn pin_git2_config() {
    static PINNED: Once = Once::new();
    PINNED.call_once(|| {
        let user = Path::new(env!("CARGO_TARGET_TMPDIR")).join("diff-user-config");
        fs::create_dir_all(&user).unwrap();
        fs::write(
            user.join(".gitconfig"),
            "[filter \"userzero\"]\n\tclean = sed s/o/0/g\n",
        )
        .unwrap();
        for (level, path) in [
            (ConfigLevel::ProgramData, ""),
            (ConfigLevel::System, ""),
            (ConfigLevel::XDG, ""),
            (ConfigLevel::Global, user.to_str().unwrap()),
        ] {
            // SAFETY: every test calls this first, so no other libgit2 call runs meanwhile.
            unsafe { git2::opts::set_search_path(level, path) }.unwrap();
        }
    });
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
    pin_git2_config();
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

// git with the pinned environment from fixtures/diff-repo/README.md: no user or system config,
// attributes or ignore files, and no inherited GIT_*.
fn pinned_git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd.current_dir(dir).envs([
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("XDG_CONFIG_HOME", "/dev/null"),
        ("GIT_AUTHOR_NAME", "Grove Test"),
        ("GIT_AUTHOR_EMAIL", "test@grove.invalid"),
        ("GIT_COMMITTER_NAME", "Grove Test"),
        ("GIT_COMMITTER_EMAIL", "test@grove.invalid"),
    ]);
    cmd
}

fn git(dir: &Path, args: &[&str]) {
    let status = pinned_git(dir).args(args).status().unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

// git's stdout. `ok` lists the exit codes that count as success: `diff --no-index` exits 1 when
// the files differ.
fn git_output<S: AsRef<OsStr>>(dir: &Path, args: &[S], ok: &[i32]) -> Vec<u8> {
    let out = pinned_git(dir).args(args).output().unwrap();
    assert!(
        out.status.code().is_some_and(|c| ok.contains(&c)),
        "git failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

// Builds fixtures/diff-repo in `dir` and returns the repo's real path.
fn fixture(dir: &Path) -> PathBuf {
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
    let repo = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
    repo.canonicalize().unwrap()
}

fn literal(path: &Path) -> OsString {
    let mut spec = OsString::from(":(literal)");
    spec.push(path);
    spec
}

// `git diff <from>` in the structured form: statuses and paths from `--raw -z`, then the binary
// flag and hunks from each file's own patch.
fn git_diff(repo: &Path, from: &str) -> Vec<FileDiff> {
    let raw = git_output(repo, &["diff", from, "--raw", "-z"], &[0]);
    let mut fields = raw.split(|&b| b == 0).filter(|f| !f.is_empty());
    let mut files = Vec::new();
    while let Some(meta) = fields.next() {
        let letter = meta.rsplit(|&b| b == b' ').next().unwrap()[0];
        let old_path = PathBuf::from(OsStr::from_bytes(fields.next().unwrap()));
        let new_path = match letter {
            b'R' => PathBuf::from(OsStr::from_bytes(fields.next().unwrap())),
            _ => old_path.clone(),
        };
        let status = match letter {
            b'A' => FileStatus::Added,
            b'D' => FileStatus::Deleted,
            b'M' => FileStatus::Modified,
            b'R' => FileStatus::Renamed,
            other => panic!("unexpected status {}", other as char),
        };
        let mut args = vec![OsString::from("diff"), from.into(), "--".into()];
        args.push(literal(&old_path));
        args.push(literal(&new_path));
        let (binary, hunks) = parse_patch(&git_output(repo, &args, &[0]));
        files.push(FileDiff {
            status,
            old_path,
            new_path,
            binary,
            hunks,
        });
    }
    files
}

// Each untracked file as `git diff --no-index /dev/null <file>` shows it.
fn git_diff_untracked(repo: &Path) -> Vec<FileDiff> {
    let listed = git_output(
        repo,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        &[0],
    );
    listed
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let path = PathBuf::from(OsStr::from_bytes(p));
            let args = [
                OsStr::new("diff"),
                "--no-index".as_ref(),
                "/dev/null".as_ref(),
                path.as_os_str(),
            ];
            let (binary, hunks) = parse_patch(&git_output(repo, &args, &[1]));
            FileDiff {
                status: FileStatus::Untracked,
                old_path: path.clone(),
                new_path: path,
                binary,
                hunks,
            }
        })
        .collect()
}

// The binary flag and hunks of a patch that holds one file.
fn parse_patch(patch: &[u8]) -> (bool, Vec<Hunk>) {
    let mut lines = patch.split_inclusive(|&b| b == b'\n').peekable();
    let (mut binary, mut hunks) = (false, Vec::new());
    while let Some(header) = lines.next() {
        if header.starts_with(b"Binary files ") {
            binary = true;
        }
        let Some(ranges) = header.strip_prefix(b"@@ -") else {
            continue;
        };
        let ranges = String::from_utf8_lossy(ranges);
        let (old, new) = ranges
            .split(" @@")
            .next()
            .unwrap()
            .split_once(" +")
            .unwrap();
        let range = |r: &str| match r.split_once(',') {
            Some((start, count)) => (start.parse().unwrap(), count.parse().unwrap()),
            None => (r.parse().unwrap(), 1),
        };
        let ((old_start, old_lines), (new_start, new_lines)) = (range(old), range(new));
        let mut hunk = Hunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            lines: Vec::new(),
        };
        let (mut old_left, mut new_left) = (old_lines, new_lines);
        while old_left > 0 || new_left > 0 || lines.peek().is_some_and(|l| l[0] == b'\\') {
            let l = lines.next().unwrap();
            let kind = match l[0] {
                b' ' => {
                    (old_left, new_left) = (old_left - 1, new_left - 1);
                    LineKind::Context
                }
                b'-' => {
                    old_left -= 1;
                    LineKind::Removed
                }
                b'+' => {
                    new_left -= 1;
                    LineKind::Added
                }
                // "\ No newline at end of file": the line before has no newline.
                b'\\' => {
                    hunk.lines.last_mut().unwrap().text.pop();
                    continue;
                }
                _ => panic!("bad hunk line {:?}", String::from_utf8_lossy(l)),
            };
            hunk.lines.push(DiffLine {
                kind,
                text: l[1..].to_vec(),
            });
        }
        hunks.push(hunk);
    }
    (binary, hunks)
}

// Compares file by file, so a failure names the file.
fn assert_same_files(mut actual: Vec<FileDiff>, mut expected: Vec<FileDiff>) {
    for files in [&mut actual, &mut expected] {
        files.sort_by(|a, b| a.new_path.as_os_str().cmp(b.new_path.as_os_str()));
    }
    let paths = |files: &[FileDiff]| files.iter().map(|f| f.new_path.clone()).collect::<Vec<_>>();
    assert_eq!(paths(&actual), paths(&expected));
    for (a, e) in actual.iter().zip(&expected) {
        assert_eq!(a, e, "{}", e.new_path.display());
    }
}

// A repo on `main` whose first commit holds `files`. Returns its real path, as git reports it.
// On macOS /tmp is a link to /private/tmp.
fn new_repo(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
    let repo = dir.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for (path, text) in files {
        fs::write(repo.join(path), text).unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    repo.canonicalize().unwrap()
}

fn line(kind: LineKind, text: &str) -> DiffLine {
    DiffLine {
        kind,
        text: text.into(),
    }
}

#[test]
fn an_edited_file_shows_its_hunk() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "one\ntwo\nthree\n")]);
    fs::write(repo.join("a.txt"), "one\n2\nthree\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    assert_eq!(
        diff,
        vec![FileDiff {
            status: FileStatus::Modified,
            old_path: "a.txt".into(),
            new_path: "a.txt".into(),
            binary: false,
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 3,
                new_start: 1,
                new_lines: 3,
                lines: vec![
                    line(LineKind::Context, "one\n"),
                    line(LineKind::Removed, "two\n"),
                    line(LineKind::Added, "2\n"),
                    line(LineKind::Context, "three\n"),
                ],
            }],
        }]
    );
}

#[test]
fn head_diff_matches_git_diff_head_on_the_fixture() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    let tracked = diff
        .into_iter()
        .filter(|f| f.status != FileStatus::Untracked)
        .collect();
    assert_same_files(tracked, git_diff(&repo, "HEAD"));
}

#[test]
fn untracked_files_match_git_diff_no_index_on_the_fixture() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    let untracked = diff
        .into_iter()
        .filter(|f| f.status == FileStatus::Untracked)
        .collect();
    assert_same_files(untracked, git_diff_untracked(&repo));
}

fn merge_base(repo: &Path, base: &str) -> String {
    let out = git_output(repo, &["merge-base", base, "HEAD"], &[0]);
    String::from_utf8(out).unwrap().trim().to_owned()
}

// The fixture's base branch is `main`, and `origin/main` exists, so the merge base comes from it.
#[test]
fn merge_base_diff_matches_git_diff_from_the_merge_base_on_the_fixture() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    let tracked = diff
        .into_iter()
        .filter(|f| f.status != FileStatus::Untracked)
        .collect();
    assert_same_files(tracked, git_diff(&repo, &merge_base(&repo, "origin/main")));
}

fn paths(diff: &[FileDiff]) -> Vec<&Path> {
    diff.iter().map(|f| f.new_path.as_path()).collect()
}

fn status_of(diff: &[FileDiff], path: &str) -> Option<FileStatus> {
    diff.iter()
        .find(|f| f.new_path == Path::new(path))
        .map(|f| f.status)
}

// In the fixture local `main` is at A and `origin/main` at C. `merge-base main HEAD` is A, which
// would add `upstream.txt`. Diffing against the tip of `origin/main` would delete
// `upstream-later.txt`.
#[test]
fn merge_base_comes_from_origin_when_it_is_ahead_of_the_local_base() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    assert_eq!(status_of(&diff, "feature.txt"), Some(FileStatus::Added));
    assert_eq!(status_of(&diff, "upstream.txt"), None, "{:?}", paths(&diff));
    assert_eq!(status_of(&diff, "upstream-later.txt"), None);
}

#[test]
fn changing_the_base_branch_changes_the_merge_base_diff() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    // Only local, at D, where `feature.txt` already exists.
    git(&repo, &["branch", "release", "HEAD~1"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let before = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    block_on(core.set_base_branch(&repo, "release".into())).unwrap();
    let after = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    assert_eq!(status_of(&before, "feature.txt"), Some(FileStatus::Added));
    assert_eq!(status_of(&after, "feature.txt"), Some(FileStatus::Modified));
    let tracked = after
        .into_iter()
        .filter(|f| f.status != FileStatus::Untracked)
        .collect();
    assert_same_files(tracked, git_diff(&repo, &merge_base(&repo, "release")));
}

#[test]
fn merge_base_diff_includes_untracked_files() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    let untracked = diff
        .into_iter()
        .filter(|f| f.status == FileStatus::Untracked)
        .collect();
    assert_same_files(untracked, git_diff_untracked(&repo));
}

#[test]
fn merge_base_file_versions_read_the_old_side_at_the_merge_base() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let diff = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    let versions = |path: &str| {
        let file = diff.iter().find(|f| f.new_path == Path::new(path)).unwrap();
        block_on(core.file_versions(&repo, DiffBase::MergeBase, file)).unwrap()
    };
    let feature = versions("feature.txt");
    let big = versions("big.py");

    assert_eq!(feature.old, None);
    assert_eq!(
        feature.new.as_deref(),
        Some(&b"feature work\nmore work\n"[..])
    );
    // E edits `x = 5` after the merge base, so the old side still has it.
    let old = big.old.unwrap();
    assert!(old.windows(10).any(|w| w == b"    x = 5\n"));
    assert!(!old.windows(9).any(|w| w == b"x = 5 * 2"));
}

#[test]
fn merge_base_diff_without_a_base_branch_fails() {
    let dir = short_temp_dir();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "trunk"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let repo = repo.canonicalize().unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::MergeBase));

    assert!(
        matches!(&diff, Err(CoreError::NoBaseBranch(path)) if *path == repo),
        "{diff:?}"
    );
}

// The tracked side as git diffs it from `from`, plus each untracked file.
fn assert_matches_git(diff: Vec<FileDiff>, repo: &Path, from: &str) {
    let (untracked, tracked) = diff
        .into_iter()
        .partition(|f| f.status == FileStatus::Untracked);
    assert_same_files(tracked, git_diff(repo, from));
    assert_same_files(untracked, git_diff_untracked(repo));
}

// git shows the commit's delete, and the file on disk is untracked.
#[test]
fn a_file_deleted_on_the_branch_and_made_again_shows_both() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "old\n")]);
    git(&repo, &["checkout", "-q", "-b", "topic"]);
    git(&repo, &["rm", "-q", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "delete"]);
    fs::write(repo.join("a.txt"), "new\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    assert_matches_git(diff, &repo, &merge_base(&repo, "main"));
}

#[test]
fn a_file_removed_from_the_index_but_kept_on_disk_shows_both() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "old\n")]);
    git(&repo, &["rm", "-q", "--cached", "a.txt"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    assert_matches_git(diff, &repo, "HEAD");
}

#[test]
fn a_file_renamed_away_and_made_again_shows_both() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "old\n")]);
    git(&repo, &["mv", "a.txt", "b.txt"]);
    fs::write(repo.join("a.txt"), "new\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    assert_matches_git(diff, &repo, "HEAD");
}

#[test]
fn merge_base_diff_of_a_branch_unrelated_to_its_base_fails() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "on main\n")]);
    git(&repo, &["checkout", "-q", "--orphan", "lonely"]);
    git(&repo, &["commit", "-q", "-m", "no common history"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::MergeBase));

    assert!(
        matches!(
            &diff,
            Err(CoreError::NoMergeBase { worktree, base }) if *worktree == repo && base == "main"
        ),
        "{diff:?}"
    );
}

// Not the fixture: adding a worktree checks out its git-lfs pointer, and a user's git-lfs would
// fail to download it.
#[test]
fn merge_base_diff_works_in_a_worktree_grove_added() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("upstream.txt", "first\n")]);
    let origin = dir.path().join("origin.git");
    git(
        dir.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    fs::write(repo.join("later.txt"), "only on origin\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "later"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    // Local `main` falls behind `origin/main`.
    git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let added = block_on(core.create_worktree(
        &repo,
        NewWorktree {
            branch: "topic".into(),
            ..NewWorktree::default()
        },
    ))
    .unwrap();
    let topic = added.worktree.path;
    fs::write(topic.join("topic.txt"), "committed\n").unwrap();
    git(&topic, &["add", "topic.txt"]);
    git(&topic, &["commit", "-q", "-m", "topic"]);
    fs::write(topic.join("upstream.txt"), "edited, not committed\n").unwrap();

    let diff = block_on(core.diff(&topic, DiffBase::MergeBase)).unwrap();

    assert_eq!(status_of(&diff, "topic.txt"), Some(FileStatus::Added));
    assert_eq!(status_of(&diff, "upstream.txt"), Some(FileStatus::Modified));
    // The branch started from `origin/main`, so the merge base has it.
    assert_eq!(status_of(&diff, "later.txt"), None);
    assert_same_files(diff, git_diff(&topic, &merge_base(&topic, "origin/main")));
}

// Two merge bases. git and git2 must pick the same one.
#[test]
fn merge_base_diff_matches_git_on_a_criss_cross_history() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "a\n")]);
    git(&repo, &["checkout", "-q", "-b", "side"]);
    fs::write(repo.join("side.txt"), "side\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "side"]);
    git(&repo, &["checkout", "-q", "main"]);
    fs::write(repo.join("main.txt"), "main\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "main"]);
    git(&repo, &["branch", "main-tip"]);
    git(&repo, &["merge", "-q", "--no-edit", "side"]);
    git(&repo, &["checkout", "-q", "side"]);
    git(&repo, &["merge", "-q", "--no-edit", "main-tip"]);
    fs::write(repo.join("a.txt"), "a\nb\n").unwrap();
    let all = git_output(&repo, &["merge-base", "--all", "main", "HEAD"], &[0]);
    assert_eq!(
        all.split(|&b| b == b'\n').filter(|l| !l.is_empty()).count(),
        2
    );
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    assert_same_files(diff, git_diff(&repo, &merge_base(&repo, "main")));
}

fn versions_of(core: &Core, repo: &Path, diff: &[FileDiff], path: &str) -> FileVersions {
    let file = diff.iter().find(|f| f.new_path == Path::new(path)).unwrap();
    block_on(core.file_versions(repo, DiffBase::Head, file)).unwrap()
}

#[test]
fn file_versions_are_the_exact_bytes_with_crlf_kept() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    let crlf = versions_of(&core, &repo, &diff, "crlf-convert.txt");
    let link = versions_of(&core, &repo, &diff, "link");

    assert_eq!(crlf.old.as_deref(), Some(&b"one\ntwo\nthree\n"[..]));
    assert_eq!(crlf.new.as_deref(), Some(&b"one\r\ntwo\r\nthree\r\n"[..]));
    // A symlink's version is its target, as git stores it.
    assert_eq!(link.old.as_deref(), Some(&b"target-a.txt"[..]));
    assert_eq!(link.new.as_deref(), Some(&b"target-b.txt"[..]));
}

#[test]
fn binary_and_non_utf8_versions_are_flagged_binary() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    let flagged = |path| versions_of(&core, &repo, &diff, path).binary;

    assert!(flagged("binary.dat"));
    assert!(flagged("untracked.bin"));
    // git diffs Latin-1 as text, but Monaco cannot show it.
    assert!(flagged("latin1.txt"));
    assert!(!flagged("crlf-convert.txt"));
    assert!(!flagged("control-chars.txt"));
}

#[test]
fn a_file_has_no_version_on_a_side_it_is_missing_from() {
    let dir = short_temp_dir();
    let repo = fixture(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    let sides = |path| {
        let v = versions_of(&core, &repo, &diff, path);
        (v.old, v.new)
    };

    assert_eq!(sides("added.txt"), (None, Some(b"a new file\n".to_vec())));
    assert_eq!(
        sides("untracked.txt"),
        (None, Some(b"not added yet\n".to_vec()))
    );
    assert_eq!(
        sides("deleted.txt"),
        (Some(b"this file gets deleted\n".to_vec()), None)
    );
    // The old version of a renamed file comes from its old path.
    let renamed = b"this file gets renamed\n".to_vec();
    assert_eq!(
        sides("renamed-new.txt"),
        (Some(renamed.clone()), Some(renamed))
    );
}

#[test]
fn diffing_a_worktree_grove_does_not_track_fails() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "one\n")]);
    let (core, _events) = start(dir.path());

    let diff = block_on(core.diff(&repo, DiffBase::Head));
    let file = FileDiff {
        status: FileStatus::Modified,
        old_path: "a.txt".into(),
        new_path: "a.txt".into(),
        binary: false,
        hunks: Vec::new(),
    };
    let versions = block_on(core.file_versions(&repo, DiffBase::Head, &file));

    assert!(
        matches!(&diff, Err(CoreError::UnknownWorktree(path)) if *path == repo),
        "{diff:?}"
    );
    assert!(
        matches!(&versions, Err(CoreError::UnknownWorktree(path)) if *path == repo),
        "{versions:?}"
    );
}

// A plain folder has no diff. No session can run in one until issue #60, so the folder here is a
// tracked checkout whose `.git` is gone, which grove tracks the same way.
#[test]
fn a_tracked_place_that_is_not_a_git_repo_has_no_diff_on_either_base() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "one\n")]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    fs::remove_dir_all(repo.join(".git")).unwrap();
    fs::write(repo.join("a.txt"), "two\n").unwrap();
    let file = FileDiff {
        status: FileStatus::Modified,
        old_path: "a.txt".into(),
        new_path: "a.txt".into(),
        binary: false,
        hunks: Vec::new(),
    };

    for base in [DiffBase::Head, DiffBase::MergeBase] {
        let diff = block_on(core.diff(&repo, base));
        let versions = block_on(core.file_versions(&repo, base, &file));

        assert!(
            matches!(&diff, Err(CoreError::NotARepo { path, .. }) if *path == repo),
            "{base:?}: {diff:?}"
        );
        assert!(
            matches!(&versions, Err(CoreError::NotARepo { path, .. }) if *path == repo),
            "{base:?}: {versions:?}"
        );
    }
    let marks = block_on(core.line_changes(&repo, Path::new("a.txt")));
    assert!(
        matches!(&marks, Err(CoreError::NotARepo { path, .. }) if *path == repo),
        "{marks:?}"
    );
}

#[test]
fn file_versions_never_read_outside_the_worktree() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "one\n")]);
    fs::write(dir.path().join("outside.txt"), "secret\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    for path in [
        PathBuf::from("../outside.txt"),
        dir.path().join("outside.txt"),
    ] {
        let file = FileDiff {
            status: FileStatus::Untracked,
            old_path: path.clone(),
            new_path: path.clone(),
            binary: false,
            hunks: Vec::new(),
        };
        let versions = block_on(core.file_versions(&repo, DiffBase::Head, &file));
        assert!(
            matches!(&versions, Err(CoreError::OutsideWorktree(p)) if *p == path),
            "{versions:?}"
        );
    }
}

#[test]
fn file_versions_never_read_through_a_symlinked_folder_outside_the_worktree() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "one\n")]);
    fs::create_dir(dir.path().join("outside")).unwrap();
    fs::write(dir.path().join("outside/secret.txt"), "secret\n").unwrap();
    std::os::unix::fs::symlink(dir.path().join("outside"), repo.join("link")).unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let path = PathBuf::from("link/secret.txt");
    let file = FileDiff {
        status: FileStatus::Untracked,
        old_path: path.clone(),
        new_path: path.clone(),
        binary: false,
        hunks: Vec::new(),
    };

    let versions = block_on(core.file_versions(&repo, DiffBase::Head, &file));

    assert!(
        matches!(&versions, Err(CoreError::OutsideWorktree(p)) if *p == path),
        "{versions:?}"
    );
}

#[test]
fn a_file_with_a_merge_conflict_matches_git_diff_head() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "base\n")]);
    git(&repo, &["checkout", "-q", "-b", "other"]);
    fs::write(repo.join("a.txt"), "other\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "other"]);
    git(&repo, &["checkout", "-q", "main"]);
    fs::write(repo.join("a.txt"), "main\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "main"]);
    let merged = pinned_git(&repo)
        .args(["merge", "-q", "other"])
        .output()
        .unwrap();
    assert!(!merged.status.success(), "the merge should conflict");
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    assert_same_files(diff, git_diff(&repo, "HEAD"));
}

// A clean filter that turns every `o` into `0`, so a filtered file's blob differs from the file
// on disk. libgit2 cannot run it, and git2 alone would diff the raw file against the blob.
fn add_zero_filter(repo: &Path) {
    git(repo, &["config", "filter.zero.clean", "sed s/o/0/g"]);
    fs::write(repo.join(".gitattributes"), "*.z filter=zero\n").unwrap();
}

#[test]
fn a_file_with_a_filter_driver_matches_git_diff_through_the_git_cli() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("plain.txt", "one\n")]);
    add_zero_filter(&repo);
    fs::write(repo.join("a.z"), "one\ntwo\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "filtered"]);
    fs::write(repo.join("a.z"), "one\ntwo\nfour\n").unwrap();
    fs::write(repo.join("plain.txt"), "one\ntwo\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    assert_same_files(diff, git_diff(&repo, "HEAD"));
}

#[test]
fn a_file_renamed_away_from_a_filter_keeps_its_rename() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[]);
    add_zero_filter(&repo);
    // No `o`, so the filter leaves it as is, and git and git2 both see a rename.
    fs::write(repo.join("a.z"), "abc\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "filtered"]);
    git(&repo, &["mv", "a.z", "a.txt"]);
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    assert_eq!(diff.len(), 1, "{diff:?}");
    assert_same_files(diff, git_diff(&repo, "HEAD"));
}

// The git CLI must diff from the merge base too: here the commit after it changes the file.
#[test]
fn a_file_with_a_filter_driver_matches_git_from_the_merge_base() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[]);
    add_zero_filter(&repo);
    fs::write(repo.join("a.z"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "filtered"]);
    git(&repo, &["checkout", "-q", "-b", "topic"]);
    fs::write(repo.join("a.z"), "one\ntwo\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "committed on topic"]);
    fs::write(repo.join("a.z"), "one\ntwo\nfour\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::MergeBase)).unwrap();

    assert_same_files(diff, git_diff(&repo, &merge_base(&repo, "main")));
}

#[test]
fn a_file_with_a_diff_driver_matches_git_diff_through_the_git_cli() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[]);
    // git diffs the textconv output of both versions. libgit2 cannot run it.
    git(&repo, &["config", "diff.zero.textconv", "sed s/o/0/g"]);
    fs::write(repo.join(".gitattributes"), "*.z diff=zero\n").unwrap();
    fs::write(repo.join("a.z"), "one\ntwo\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "with a driver"]);
    fs::write(repo.join("a.z"), "one\ntwo\nfour\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    assert_same_files(diff, git_diff(&repo, "HEAD"));
}

#[test]
fn a_file_with_a_driver_that_becomes_a_symlink_is_deleted_then_added() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[(".gitattributes", "*.z diff=zero\n")]);
    fs::write(repo.join("a.z"), "one\n").unwrap();
    fs::write(repo.join("b.txt"), "one\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "files"]);
    // `a.z` goes through the git CLI and `b.txt` through git2. Both change type.
    for path in ["a.z", "b.txt"] {
        fs::remove_file(repo.join(path)).unwrap();
        std::os::unix::fs::symlink("target", repo.join(path)).unwrap();
    }
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    let deleted_then_added = |path: &str| {
        [
            FileDiff {
                status: FileStatus::Deleted,
                old_path: path.into(),
                new_path: path.into(),
                binary: false,
                hunks: vec![Hunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 0,
                    new_lines: 0,
                    lines: vec![line(LineKind::Removed, "one\n")],
                }],
            },
            FileDiff {
                status: FileStatus::Added,
                old_path: path.into(),
                new_path: path.into(),
                binary: false,
                hunks: vec![Hunk {
                    old_start: 0,
                    old_lines: 0,
                    new_start: 1,
                    new_lines: 1,
                    lines: vec![line(LineKind::Added, "target")],
                }],
            },
        ]
    };
    assert_eq!(
        diff,
        [deleted_then_added("a.z"), deleted_then_added("b.txt")].concat()
    );
}

#[test]
fn a_filter_driver_set_in_the_user_config_still_runs() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[]);
    fs::write(repo.join(".gitattributes"), "*.u filter=userzero\n").unwrap();
    fs::write(repo.join("a.u"), "one\n").unwrap();
    // The test's git hides the user config, so it gets the filter on its command line.
    git(
        &repo,
        &["-c", "filter.userzero.clean=sed s/o/0/g", "add", "-A"],
    );
    git(&repo, &["commit", "-q", "-m", "filtered"]);
    fs::write(repo.join("a.u"), "one\ntwo\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let diff = block_on(core.diff(&repo, DiffBase::Head)).unwrap();

    assert_eq!(
        diff,
        vec![FileDiff {
            status: FileStatus::Modified,
            old_path: "a.u".into(),
            new_path: "a.u".into(),
            binary: false,
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 2,
                lines: vec![
                    line(LineKind::Context, "0ne\n"),
                    line(LineKind::Added, "tw0\n"),
                ],
            }],
        }]
    );
}

// Files panel gutter (#124).

#[test]
fn line_changes_mark_the_files_lines_changed_since_head() {
    let dir = short_temp_dir();
    let repo = new_repo(
        dir.path(),
        &[
            ("a.txt", "one\ntwo\nthree\nfour\nfive\n"),
            ("b.txt", "same\n"),
        ],
    );
    fs::write(repo.join("a.txt"), "one\n2\nthree\nnew\nfour\n").unwrap();
    fs::write(repo.join("new.txt"), "a\nb\n").unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let marks = |file: &str| block_on(core.line_changes(&repo, Path::new(file))).unwrap();

    assert_eq!(
        marks("a.txt"),
        vec![
            LineChange::Modified { start: 2, end: 2 },
            LineChange::Added { start: 4, end: 4 },
            LineChange::Deleted { after: 5 },
        ]
    );
    assert_eq!(
        marks("new.txt"),
        vec![LineChange::Added { start: 1, end: 2 }]
    );
    assert_eq!(marks("b.txt"), vec![]);
}

#[test]
fn line_changes_fail_for_a_worktree_grove_does_not_track() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path(), &[("a.txt", "one\n")]);
    let (core, _events) = start(dir.path());

    let result = block_on(core.line_changes(&repo, Path::new("a.txt")));

    assert!(
        matches!(result, Err(CoreError::UnknownWorktree(_))),
        "{result:?}"
    );
}
