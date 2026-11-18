use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    CommentState, Core, CoreConfig, CoreError, CoreEvent, NewReviewComment, NewWorktree,
    ReviewComment,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

// Waits this long for an event that should come. The watcher sends DiffChanged within 500 ms.
const WAIT: Duration = Duration::from_secs(5);

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

// "line 1\n" to "line <n>\n".
fn numbered(n: u32) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

// A repo on `main` whose one commit has `a.txt` with 20 numbered lines. Returns its real path,
// as git reports it. On macOS /tmp is a link to /private/tmp.
fn new_repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), numbered(20)).unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    repo.canonicalize().unwrap()
}

fn comment(path: &str, start_line: u32, end_line: u32) -> NewReviewComment {
    NewReviewComment {
        path: PathBuf::from(path),
        start_line,
        end_line,
        body: "rename this".into(),
    }
}

// Waits for the next `CommentsChanged` for `worktree`. Panics after WAIT.
fn wait_comments_changed(events: &mut broadcast::Receiver<CoreEvent>, worktree: &Path) {
    let start = Instant::now();
    loop {
        match events.try_recv() {
            Ok(CoreEvent::CommentsChanged { worktree: w }) if w == worktree => return,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(
                    start.elapsed() < WAIT,
                    "no CommentsChanged for {} within {WAIT:?}",
                    worktree.display()
                );
                thread::sleep(Duration::from_millis(5));
            }
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

// Counts the `CommentsChanged` events for any worktree sent during the next `window`.
fn count_comments_changed(events: &mut broadcast::Receiver<CoreEvent>, window: Duration) -> usize {
    let end = Instant::now() + window;
    let mut count = 0;
    while Instant::now() < end {
        match events.try_recv() {
            Ok(CoreEvent::CommentsChanged { .. }) => count += 1,
            Ok(_) => {}
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            Err(e) => panic!("{e}"),
        }
    }
    count
}

// The one open comment on `worktree`.
fn only_open(core: &Core, worktree: &Path) -> ReviewComment {
    let open = core.comments(worktree).unwrap();
    assert_eq!(open.len(), 1, "{open:?}");
    open.into_iter().next().unwrap()
}

// Waits until the one open comment on `worktree` is at `path`, `start` to `end`, and not
// flagged, checking after each `CommentsChanged`. Panics after WAIT. On Linux the watcher can
// split one git command across two bursts, so a pass may see it half done: the file moved,
// but the index or HEAD not yet written.
fn wait_open_at(
    core: &Core,
    events: &mut broadcast::Receiver<CoreEvent>,
    worktree: &Path,
    (path, start, end): (&str, u32, u32),
) {
    let begin = Instant::now();
    loop {
        match events.try_recv() {
            Ok(CoreEvent::CommentsChanged { worktree: w }) if w == worktree => {
                let c = only_open(core, worktree);
                let at = (
                    c.path.as_path(),
                    c.start_line,
                    c.end_line,
                    c.possibly_addressed,
                );
                if at == (Path::new(path), start, end, false) {
                    return;
                }
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(
                    begin.elapsed() < WAIT,
                    "comment not at {path} {start}-{end} within {WAIT:?}: {:?}",
                    core.comments(worktree)
                );
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_comment_moves_down_with_lines_inserted_above_it() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);

    fs::write(
        repo.join("a.txt"),
        format!("{}{}", "new\n".repeat(5), numbered(20)),
    )
    .unwrap();

    wait_comments_changed(&mut events, &repo);
    let moved = only_open(&core, &repo);
    assert_eq!((moved.start_line, moved.end_line), (15, 17));
    assert_eq!(moved.text, "line 10\nline 11\nline 12");
    assert!(!moved.possibly_addressed);
}

#[test]
fn deleting_the_commented_lines_flags_the_comment_possibly_addressed() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);

    let kept: String = numbered(20)
        .lines()
        .filter(|line| !["line 10", "line 11", "line 12"].contains(line))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(repo.join("a.txt"), kept).unwrap();

    wait_comments_changed(&mut events, &repo);
    let flagged = only_open(&core, &repo);
    assert!(flagged.possibly_addressed);
    // It stays where its text was last seen.
    assert_eq!(
        (flagged.path.as_path(), flagged.start_line, flagged.end_line),
        (Path::new("a.txt"), 10, 12)
    );
}

#[test]
fn deleting_the_file_flags_the_comment_possibly_addressed() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);

    fs::remove_file(repo.join("a.txt")).unwrap();

    wait_comments_changed(&mut events, &repo);
    let flagged = only_open(&core, &repo);
    assert!(flagged.possibly_addressed);
    assert_eq!(flagged.path, Path::new("a.txt"));
}

#[test]
fn a_rename_git_does_not_detect_flags_the_comment_possibly_addressed() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);

    // Nothing is staged, so git sees a.txt deleted and b.txt untracked.
    fs::rename(repo.join("a.txt"), repo.join("b.txt")).unwrap();

    wait_comments_changed(&mut events, &repo);
    let flagged = only_open(&core, &repo);
    assert!(flagged.possibly_addressed);
    assert_eq!(flagged.path, Path::new("a.txt"));
}

#[test]
fn a_rename_git_detects_keeps_the_comment_on_the_new_path() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);

    git(&repo, &["mv", "a.txt", "b.txt"]);

    wait_open_at(&core, &mut events, &repo, ("b.txt", 10, 12));
}

#[test]
fn a_rename_git_detects_wins_over_a_new_file_at_the_old_path() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);

    // Within one debounce, so the watcher sees both at once.
    git(&repo, &["mv", "a.txt", "b.txt"]);
    fs::write(repo.join("a.txt"), "new\n").unwrap();

    wait_open_at(&core, &mut events, &repo, ("b.txt", 10, 12));
}

#[test]
fn a_comment_on_a_new_file_at_a_renamed_path_stays_on_that_file() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    git(&repo, &["mv", "a.txt", "b.txt"]);
    fs::write(repo.join("a.txt"), "new\n").unwrap();
    wait_diff_changed(&mut events, &repo);
    block_on(core.add_comment(&repo, comment("a.txt", 1, 1))).unwrap();
    wait_comments_changed(&mut events, &repo);

    // git still shows a.txt renamed to b.txt.
    fs::write(repo.join("a.txt"), "above\nnew\n").unwrap();

    wait_comments_changed(&mut events, &repo);
    let moved = only_open(&core, &repo);
    assert_eq!(
        (moved.path.as_path(), moved.start_line, moved.end_line),
        (Path::new("a.txt"), 2, 2)
    );
    assert!(!moved.possibly_addressed);
}

#[test]
fn a_comment_on_a_new_file_at_a_renamed_path_stays_on_it_after_a_second_rename() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    git(&repo, &["mv", "a.txt", "b.txt"]);
    fs::write(repo.join("a.txt"), "new\n").unwrap();
    wait_diff_changed(&mut events, &repo);
    block_on(core.add_comment(&repo, comment("a.txt", 1, 1))).unwrap();
    wait_comments_changed(&mut events, &repo);

    // git now shows a.txt renamed to c.txt.
    git(&repo, &["mv", "b.txt", "c.txt"]);
    fs::write(repo.join("a.txt"), "above\nnew\n").unwrap();

    wait_comments_changed(&mut events, &repo);
    let moved = only_open(&core, &repo);
    assert_eq!(
        (moved.path.as_path(), moved.start_line, moved.end_line),
        (Path::new("a.txt"), 2, 2)
    );
    assert!(!moved.possibly_addressed);
}

#[test]
fn a_rename_committed_on_the_branch_keeps_the_comment_on_the_new_path() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let new = NewWorktree {
        branch: "topic".into(),
        ..NewWorktree::default()
    };
    let topic = block_on(core.create_worktree(&repo, new))
        .unwrap()
        .worktree
        .path;
    block_on(core.add_comment(&topic, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &topic);
    // The rename is committed somewhere grove does not watch, so the topic worktree gets it in
    // one step and never shows it staged.
    let scratch = dir.path().join("scratch");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "renamed",
            &scratch.to_string_lossy(),
            "topic",
        ],
    );
    git(&scratch, &["mv", "a.txt", "b.txt"]);
    git(&scratch, &["commit", "-q", "-m", "rename"]);

    git(&topic, &["merge", "-q", "--ff-only", "renamed"]);

    wait_open_at(&core, &mut events, &topic, ("b.txt", 10, 12));
}

#[test]
fn resolving_hides_the_comment_from_the_open_list_and_keeps_it() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let first = block_on(core.add_comment(&repo, comment("a.txt", 1, 1))).unwrap();
    let second = block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);
    wait_comments_changed(&mut events, &repo);

    core.resolve_comment(first).unwrap();

    wait_comments_changed(&mut events, &repo);
    assert_eq!(only_open(&core, &repo).id, second);
    let resolved = core.resolved_comments(&repo).unwrap();
    assert_eq!(resolved.len(), 1, "{resolved:?}");
    assert_eq!(resolved[0].id, first);
    assert_eq!(resolved[0].state, CommentState::Resolved);
    assert_eq!(resolved[0].text, "line 1");
}

#[test]
fn when_the_text_appears_twice_the_comment_moves_to_the_closer_one() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 15, 15))).unwrap();
    wait_comments_changed(&mut events, &repo);

    // A copy of line 15 goes in at line 3, which pushes the original to line 16.
    let mut lines: Vec<String> = numbered(20).lines().map(str::to_owned).collect();
    lines.insert(2, "line 15".into());
    fs::write(repo.join("a.txt"), lines.join("\n") + "\n").unwrap();

    wait_comments_changed(&mut events, &repo);
    let moved = only_open(&core, &repo);
    assert_eq!((moved.start_line, moved.end_line), (16, 16));
}

#[test]
fn when_two_copies_are_as_close_the_comment_moves_to_the_earlier_one() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 10, 10))).unwrap();
    wait_comments_changed(&mut events, &repo);

    // Line 10 is gone, and copies of it sit 2 lines above and 2 lines below where it was.
    let mut lines: Vec<String> = numbered(20).lines().map(str::to_owned).collect();
    lines[9] = "changed".into();
    lines[7] = "line 10".into();
    lines[11] = "line 10".into();
    fs::write(repo.join("a.txt"), lines.join("\n") + "\n").unwrap();

    wait_comments_changed(&mut events, &repo);
    let moved = only_open(&core, &repo);
    assert_eq!((moved.start_line, moved.end_line), (8, 8));
}

#[test]
fn adding_and_resolving_each_send_one_comments_changed() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let id = block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    assert_eq!(
        count_comments_changed(&mut events, Duration::from_millis(300)),
        1
    );

    core.resolve_comment(id).unwrap();
    assert_eq!(
        count_comments_changed(&mut events, Duration::from_millis(300)),
        1
    );

    // Nothing changes the second time.
    core.resolve_comment(id).unwrap();
    assert_eq!(
        count_comments_changed(&mut events, Duration::from_millis(300)),
        0
    );
}

#[test]
fn a_change_that_moves_no_comment_sends_no_comments_changed() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    let new = NewWorktree {
        branch: "topic".into(),
        ..NewWorktree::default()
    };
    let topic = block_on(core.create_worktree(&repo, new))
        .unwrap()
        .worktree
        .path;
    block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);

    // Below the comment, and in a worktree with no comments.
    fs::write(repo.join("a.txt"), numbered(25)).unwrap();
    fs::write(topic.join("a.txt"), "other\n").unwrap();

    wait_diff_changed(&mut events, &repo);
    assert_eq!(
        count_comments_changed(&mut events, Duration::from_secs(1)),
        0
    );
}

#[test]
fn a_flagged_comment_whose_text_comes_back_is_no_longer_flagged() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, mut events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();
    block_on(core.add_comment(&repo, comment("a.txt", 10, 12))).unwrap();
    wait_comments_changed(&mut events, &repo);
    fs::remove_file(repo.join("a.txt")).unwrap();
    wait_comments_changed(&mut events, &repo);
    assert!(only_open(&core, &repo).possibly_addressed);

    git(&repo, &["checkout", "--", "a.txt"]);

    wait_comments_changed(&mut events, &repo);
    let back = only_open(&core, &repo);
    assert!(!back.possibly_addressed);
    assert_eq!((back.start_line, back.end_line), (10, 12));
}

#[test]
fn a_comment_on_lines_the_file_does_not_have_is_refused() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    for (start, end) in [(0, 1), (3, 2), (20, 21)] {
        let refused = block_on(core.add_comment(&repo, comment("a.txt", start, end)));
        assert!(
            matches!(refused, Err(CoreError::NoSuchLines { .. })),
            "{start} to {end}: {refused:?}"
        );
    }
    let outside = block_on(core.add_comment(&repo, comment("../a.txt", 1, 1)));
    assert!(
        matches!(outside, Err(CoreError::OutsideWorktree(_))),
        "{outside:?}"
    );
    assert!(core.comments(&repo).unwrap().is_empty());
}

#[test]
fn a_comment_through_a_symlinked_folder_outside_the_worktree_is_refused() {
    let dir = short_temp_dir();
    let repo = new_repo(dir.path());
    fs::create_dir(dir.path().join("outside")).unwrap();
    fs::write(dir.path().join("outside/secret.txt"), "secret\n").unwrap();
    std::os::unix::fs::symlink(dir.path().join("outside"), repo.join("link")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("outside/secret.txt"), repo.join("alias")).unwrap();
    let (core, _events) = start(dir.path());
    block_on(core.add_repo(&repo)).unwrap();

    let refused = block_on(core.add_comment(&repo, comment("link/secret.txt", 1, 1)));

    assert!(
        matches!(refused, Err(CoreError::OutsideWorktree(_))),
        "{refused:?}"
    );
    // A symlink itself is fine. Its version is its target path, as git stores it.
    block_on(core.add_comment(&repo, comment("alias", 1, 1))).unwrap();
    let alias = only_open(&core, &repo);
    assert_eq!(
        alias.text,
        dir.path().join("outside/secret.txt").to_string_lossy()
    );
}
