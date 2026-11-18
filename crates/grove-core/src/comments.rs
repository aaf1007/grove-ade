//! Review comments. Each is anchored to the text of the lines it is on, and follows that text as
//! the worktree changes.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::diff::{self, Start};
use crate::state::SaveRequests;
use crate::{
    AgentMessage, Core, CoreError, CoreEvent, FileStatus, Result, SessionId, WorktreeInfo,
};

/// Names a review comment for as long as grove keeps it, across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommentId(u64);

impl fmt::Display for CommentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A comment for `Core::add_comment`, on lines of the worktree's current version of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReviewComment {
    /// Relative to the worktree.
    pub path: PathBuf,
    /// The first and last lines it is on, counting from 1.
    pub start_line: u32,
    pub end_line: u32,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: CommentId,
    /// Where its text is now, or was last seen. Relative to the worktree.
    pub path: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
    /// The lines it is on, as they were when it was added, joined by `\n`. grove looks for them
    /// after every change to the worktree.
    pub text: String,
    pub body: String,
    pub state: CommentState,
    /// Its text is gone, its file is deleted, or its file was renamed in a way git does not
    /// detect.
    pub possibly_addressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentState {
    Open,
    /// Sent to an agent session in a review batch.
    Sent,
    /// Hidden from the list, but kept.
    Resolved,
}

/// One review comment of a review batch, as sent, with control characters but newline and tab
/// stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchComment {
    /// Relative to the worktree.
    pub path: PathBuf,
    pub start_line: u32,
    pub end_line: u32,
    /// The lines it is on, joined by `\n`.
    pub text: String,
    /// What the comment says.
    pub body: String,
}

impl Core {
    /// Adds a comment on lines of the current version of a file in the worktree at `worktree`.
    pub async fn add_comment(&self, worktree: &Path, new: NewReviewComment) -> Result<CommentId> {
        let base = self
            .tracked()
            .iter()
            .find(|w| w.path == worktree)
            .ok_or_else(|| CoreError::UnknownWorktree(worktree.to_path_buf()))?
            .base_branch
            .clone();
        let (comments, dir) = (self.comments.clone(), worktree.to_path_buf());
        self.runtime
            .spawn_blocking(move || {
                // Diffed first, so the text is read just before the comment is stored.
                let renames = renamed(&dir, base.as_deref());
                let bytes = diff::worktree_file(&dir, &new.path)?;
                let (start, end) = (new.start_line, new.end_line);
                let text = text_of(&bytes, start, end).ok_or_else(|| CoreError::NoSuchLines {
                    path: dir.join(&new.path),
                    start,
                    end,
                })?;
                // A rename git already shows from this path left a new file here.
                let not_followed = renames.get(&new.path).cloned();
                let id = comments.add(&dir, new, text, not_followed);
                // A search that ran after the read missed the new comment, so place it again.
                let mut unresolved = comments.to_place(&dir);
                unresolved.retain(|(c, _)| c.id == id);
                comments.place(&dir, places(&dir, &renames, &unresolved));
                Ok(id)
            })
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("read a file in", worktree)(io::Error::other(e)))?
    }

    /// The open and sent comments on the worktree at `worktree`, oldest first.
    pub fn comments(&self, worktree: &Path) -> Result<Vec<ReviewComment>> {
        self.check_tracked(worktree)?;
        Ok(self.comments.list(worktree, false))
    }

    /// The resolved comments on the worktree at `worktree`, oldest first.
    pub fn resolved_comments(&self, worktree: &Path) -> Result<Vec<ReviewComment>> {
        self.check_tracked(worktree)?;
        Ok(self.comments.list(worktree, true))
    }

    /// Resolves a comment, open or sent. It leaves the list but is kept, and grove stops moving
    /// it.
    pub fn resolve_comment(&self, id: CommentId) -> Result<()> {
        self.comments.resolve(id)
    }

    /// Sends the open comments on the worktree at `worktree` as one review batch to its agent
    /// session `to`, or by default to its most recently focused agent session. The session must
    /// be `Idle` or `Done`. In chat mode the batch is one message, which the conversation shows
    /// as a `ReviewBatch`. In terminal mode it is a bracketed paste, so the session must have
    /// bracketed paste on. The comments become `Sent`. In chat mode they become `Open` again if
    /// the agent's process ends before it starts a turn on the batch.
    pub fn send_review_batch(&self, worktree: &Path, to: Option<SessionId>) -> Result<()> {
        self.check_tracked(worktree)?;
        let (session, chat) = self.batch_target(worktree, to)?;
        self.comments.send(worktree, |open| {
            let comments = batch(open);
            let text = message(&comments);
            let Some(chat) = chat else {
                // Enter is `\r` for both agents.
                return self.write(session, format!("\x1b[200~{text}\x1b[201~\r").as_bytes());
            };
            let input = self.input(session)?;
            let ids = open.iter().map(|c| c.id).collect();
            let shown = AgentMessage::ReviewBatch { comments };
            chat.send_as(session, &input, &text, shown, ids)
        })
    }
}

// Every worktree's comments, oldest first. Every change sends `CommentsChanged` and asks for a
// save, except dropping a removed worktree's.
pub(crate) struct Comments {
    state: Mutex<State>,
    events: broadcast::Sender<CoreEvent>,
    saves: SaveRequests,
}

/// Each worktree's comments, oldest first, each with the rename it does not follow, as the
/// state file keeps them.
pub(crate) type Kept = HashMap<PathBuf, Vec<(ReviewComment, Option<PathBuf>)>>;

#[derive(Default)]
struct State {
    next: u64,
    by_worktree: HashMap<PathBuf, Vec<ReviewComment>>,
    // For a comment added on a path git already showed renamed away, where that rename went. The
    // comment is on the new file at the old path, so it does not follow that rename.
    not_followed: HashMap<CommentId, PathBuf>,
}

// Where a comment's text is now. When it is gone, where it was last seen, flagged.
struct Place {
    path: PathBuf,
    start_line: u32,
    possibly_addressed: bool,
}

impl Comments {
    /// Starts with the comments a restart brings back. A new comment's id comes after theirs.
    pub(crate) fn new(
        events: broadcast::Sender<CoreEvent>,
        saves: SaveRequests,
        kept: Kept,
    ) -> Comments {
        let mut state = State::default();
        for (worktree, comments) in kept {
            for (comment, not_followed) in comments {
                state.next = state.next.max(comment.id.0 + 1);
                if let Some(path) = not_followed {
                    state.not_followed.insert(comment.id, path);
                }
                state
                    .by_worktree
                    .entry(worktree.clone())
                    .or_default()
                    .push(comment);
            }
        }
        Comments {
            state: Mutex::new(state),
            events,
            saves,
        }
    }

    /// Every worktree's comments, as `new` takes them back.
    pub(crate) fn kept(&self) -> Kept {
        let state = self.lock();
        state
            .by_worktree
            .iter()
            .map(|(worktree, comments)| {
                let comments = comments
                    .iter()
                    .map(|c| (c.clone(), state.not_followed.get(&c.id).cloned()))
                    .collect();
                (worktree.clone(), comments)
            })
            .collect()
    }

    fn add(
        &self,
        worktree: &Path,
        new: NewReviewComment,
        text: String,
        not_followed: Option<PathBuf>,
    ) -> CommentId {
        let mut state = self.lock();
        let id = CommentId(state.next);
        state.next += 1;
        if let Some(path) = not_followed {
            state.not_followed.insert(id, path);
        }
        state
            .by_worktree
            .entry(worktree.to_path_buf())
            .or_default()
            .push(ReviewComment {
                id,
                path: new.path,
                start_line: new.start_line,
                end_line: new.end_line,
                text,
                body: new.body,
                state: CommentState::Open,
                possibly_addressed: false,
            });
        drop(state);
        self.changed(worktree);
        id
    }

    fn list(&self, worktree: &Path, resolved: bool) -> Vec<ReviewComment> {
        self.lock()
            .by_worktree
            .get(worktree)
            .into_iter()
            .flatten()
            .filter(|c| (c.state == CommentState::Resolved) == resolved)
            .cloned()
            .collect()
    }

    // Runs `write` on the worktree's open comments, oldest first, and marks them `Sent` once it
    // succeeds. `write` runs under the lock, so two batches never send the same comment.
    fn send(
        &self,
        worktree: &Path,
        write: impl FnOnce(&[ReviewComment]) -> Result<()>,
    ) -> Result<()> {
        let mut state = self.lock();
        let comments = state
            .by_worktree
            .get_mut(worktree)
            .map(Vec::as_mut_slice)
            .unwrap_or_default();
        let open: Vec<ReviewComment> = comments
            .iter()
            .filter(|c| c.state == CommentState::Open)
            .cloned()
            .collect();
        if open.is_empty() {
            return Err(CoreError::NothingToSend(worktree.to_path_buf()));
        }
        write(&open)?;
        for comment in comments
            .iter_mut()
            .filter(|c| c.state == CommentState::Open)
        {
            comment.state = CommentState::Sent;
        }
        drop(state);
        self.changed(worktree);
        Ok(())
    }

    /// Puts the comments among `ids` that are still `Sent` back to `Open`: their review batch
    /// never reached the agent.
    pub(crate) fn reopen(&self, worktree: &Path, ids: &[CommentId]) {
        let mut state = self.lock();
        let mut reopened = false;
        let comments = state.by_worktree.get_mut(worktree).into_iter().flatten();
        for comment in comments.filter(|c| c.state == CommentState::Sent && ids.contains(&c.id)) {
            comment.state = CommentState::Open;
            reopened = true;
        }
        drop(state);
        if reopened {
            self.changed(worktree);
        }
    }

    fn resolve(&self, id: CommentId) -> Result<()> {
        let mut state = self.lock();
        let (worktree, comment) = state
            .by_worktree
            .iter_mut()
            .find_map(|(worktree, comments)| {
                let comment = comments.iter_mut().find(|c| c.id == id)?;
                Some((worktree.clone(), comment))
            })
            .ok_or(CoreError::UnknownComment(id))?;
        let resolved = comment.state == CommentState::Resolved;
        comment.state = CommentState::Resolved;
        drop(state);
        if !resolved {
            self.changed(&worktree);
        }
        Ok(())
    }

    // The worktree's comments not resolved, each with the rename it does not follow.
    fn to_place(&self, worktree: &Path) -> Vec<(ReviewComment, Option<PathBuf>)> {
        let state = self.lock();
        state
            .by_worktree
            .get(worktree)
            .into_iter()
            .flatten()
            .filter(|c| c.state != CommentState::Resolved)
            .map(|c| (c.clone(), state.not_followed.get(&c.id).cloned()))
            .collect()
    }

    // Every worktree with a comment.
    fn worktrees(&self) -> Vec<PathBuf> {
        self.lock().by_worktree.keys().cloned().collect()
    }

    /// Drops every comment on `worktree`, which is being removed, so a worktree added at the same
    /// path starts with none. It sends no `CommentsChanged`, and the removal saves.
    pub(crate) fn drop_worktree(&self, worktree: &Path) {
        let mut state = self.lock();
        for comment in state.by_worktree.remove(worktree).unwrap_or_default() {
            state.not_followed.remove(&comment.id);
        }
    }

    // Moves each comment still not resolved to its place, and says so if any changed.
    fn place(&self, worktree: &Path, places: Vec<(CommentId, Place)>) {
        let mut state = self.lock();
        let Some(comments) = state.by_worktree.get_mut(worktree) else {
            return;
        };
        let mut moved = false;
        for (id, place) in places {
            let Some(comment) = comments
                .iter_mut()
                .find(|c| c.id == id && c.state != CommentState::Resolved)
            else {
                continue;
            };
            let end_line = place.start_line + (comment.end_line - comment.start_line);
            let now = (
                &comment.path,
                comment.start_line,
                comment.possibly_addressed,
            );
            if now != (&place.path, place.start_line, place.possibly_addressed) {
                comment.path = place.path;
                comment.start_line = place.start_line;
                comment.end_line = end_line;
                comment.possibly_addressed = place.possibly_addressed;
                moved = true;
            }
        }
        drop(state);
        if moved {
            self.changed(worktree);
        }
    }

    fn changed(&self, worktree: &Path) {
        self.saves.request();
        let _ = self.events.send(CoreEvent::CommentsChanged {
            worktree: worktree.to_path_buf(),
        });
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Moves each comment not resolved to where its text is after every `DiffChanged`. Runs until
/// the core drops.
pub(crate) async fn follow(
    comments: Arc<Comments>,
    worktrees: Arc<Mutex<Vec<WorktreeInfo>>>,
    mut events: broadcast::Receiver<CoreEvent>,
) {
    // Files may have changed while grove was closed, so it places the comments a restart brought
    // back first.
    let mut changed = comments.worktrees();
    loop {
        for worktree in changed {
            let unresolved = comments.to_place(&worktree);
            if unresolved.is_empty() {
                continue;
            }
            // A worktree grove forgot keeps its comments, but they stay where they are.
            let base = worktrees
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|w| w.path == worktree)
                .map(|w| w.base_branch.clone());
            let Some(base) = base else {
                continue;
            };
            let dir = worktree.clone();
            // One worktree at a time, so an older search never overwrites a newer one.
            let found = tokio::task::spawn_blocking(move || {
                places(&dir, &renamed(&dir, base.as_deref()), &unresolved)
            });
            let Ok(places) = found.await else {
                continue;
            };
            comments.place(&worktree, places);
        }
        changed = match events.recv().await {
            Ok(CoreEvent::DiffChanged { worktree }) => vec![worktree],
            Ok(_) => Vec::new(),
            // The events it missed may have been DiffChanged.
            Err(RecvError::Lagged(_)) => comments.worktrees(),
            Err(RecvError::Closed) => return,
        };
    }
}

// Where each comment's text is now. A comment follows a rename git detects of its file, even when
// a new file took the old path. A comment added on such a new file never follows the rename git
// showed then, and follows a later one only when its text is gone from its own file but is at the
// new path. `renames` comes from `renamed`. Blocks while it reads the files.
fn places(
    worktree: &Path,
    renames: &HashMap<PathBuf, PathBuf>,
    unresolved: &[(ReviewComment, Option<PathBuf>)],
) -> Vec<(CommentId, Place)> {
    unresolved
        .iter()
        .map(|(c, not_followed)| {
            let search = |path: &Path| {
                let bytes = diff::worktree_file(worktree, path).ok()?;
                find(&bytes, &c.text, c.start_line)
            };
            let (path, found) = match renames.get(&c.path) {
                Some(new) if not_followed.is_none() => (new, search(new)),
                Some(new) if Some(new) != not_followed.as_ref() => match search(&c.path) {
                    Some(line) => (&c.path, Some(line)),
                    None => search(new).map_or((&c.path, None), |line| (new, Some(line))),
                },
                _ => (&c.path, search(&c.path)),
            };
            let place = Place {
                path: path.clone(),
                start_line: found.unwrap_or(c.start_line),
                possibly_addressed: found.is_none(),
            };
            (c.id, place)
        })
        .collect()
}

// The files the worktree's diffs show renamed, from old path to new. The diff against HEAD has
// renames not committed yet, and wins. The one against the merge base with `base` has those
// committed on the branch. A diff that fails finds none.
fn renamed(worktree: &Path, base: Option<&str>) -> HashMap<PathBuf, PathBuf> {
    let starts = [
        Some(Start::Head),
        base.map(|b| Start::MergeBase(b.to_owned())),
    ];
    let mut renamed = HashMap::new();
    for start in starts.into_iter().flatten() {
        for file in diff::diff_from(worktree, &start).unwrap_or_default() {
            if file.status == FileStatus::Renamed {
                renamed.entry(file.old_path).or_insert(file.new_path);
            }
        }
    }
    renamed
}

// The file's lines without their line endings. A file that is not UTF-8 is read lossily, the
// same way when a comment is added as when its text is searched for.
fn lines(file: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(file)
        .lines()
        .map(str::to_owned)
        .collect()
}

// Lines `start` to `end` of the file, joined by `\n`, or None when it does not have them.
fn text_of(file: &[u8], start: u32, end: u32) -> Option<String> {
    let lines = lines(file);
    let (first, last) = (start.checked_sub(1)? as usize, end as usize);
    (first < last && last <= lines.len()).then(|| lines[first..last].join("\n"))
}

// The line where `text` starts in the file. Of several copies, the one that starts closest to
// line `near`, and of two as close, the earlier.
fn find(file: &[u8], text: &str, near: u32) -> Option<u32> {
    let lines = lines(file);
    let want: Vec<&str> = text.split('\n').collect();
    lines
        .windows(want.len())
        .enumerate()
        .filter(|(_, window)| *window == want.as_slice())
        .map(|(i, _)| i as u32 + 1)
        // `min_by_key` keeps the first of equal keys.
        .min_by_key(|line| line.abs_diff(near))
}

// The review batch for `comments`, oldest first, stripped of control characters.
fn batch(comments: &[ReviewComment]) -> Vec<BatchComment> {
    comments
        .iter()
        .map(|c| BatchComment {
            path: PathBuf::from(plain(&c.path.to_string_lossy())),
            start_line: c.start_line,
            end_line: c.end_line,
            text: plain(&c.text),
            body: plain(&c.body),
        })
        .collect()
}

// The review batch as the agent reads it. For each comment it has where it is, its lines quoted,
// then what it says.
fn message(comments: &[BatchComment]) -> String {
    comments
        .iter()
        .map(|c| {
            let path = c.path.display();
            let place = if c.start_line == c.end_line {
                format!("{path}:{}", c.start_line)
            } else {
                format!("{path}:{}-{}", c.start_line, c.end_line)
            };
            let snippet: Vec<String> = c.text.split('\n').map(|l| format!("> {l}")).collect();
            format!("{place}\n{}\n{}", snippet.join("\n"), c.body)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

// `text` without ESC and the other control characters but newline and tab, so it cannot end
// a bracketed paste early or reach the agent as a key.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
        .collect()
}
