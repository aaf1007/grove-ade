//! A worktree's files for the Files panel: the list, and reading and writing one file.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Core, CoreError, Result, diff, worktree};

impl Core {
    /// Every file in the worktree at `worktree`, relative to it and sorted: the tracked files and
    /// the untracked ones `.gitignore` does not ignore. The same paths as
    /// `git ls-files --cached --others --exclude-standard`.
    pub async fn list_files(&self, worktree: &Path) -> Result<Vec<PathBuf>> {
        self.check_tracked(worktree)?;
        let dir = worktree.to_path_buf();
        self.runtime
            .spawn_blocking(move || list(&dir))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("list the files in", worktree)(io::Error::other(e)))?
    }

    /// The bytes of `file`, relative to the worktree at `worktree`. Symlinks are followed, and
    /// must end inside the worktree.
    pub async fn read_file(&self, worktree: &Path, file: &Path) -> Result<Vec<u8>> {
        self.check_tracked(worktree)?;
        let (dir, file) = (worktree.to_path_buf(), file.to_path_buf());
        self.runtime
            .spawn_blocking(move || read(&dir, &file))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("read a file in", worktree)(io::Error::other(e)))?
    }

    /// Replaces `file`, relative to the worktree at `worktree`, with `bytes`, or creates it in a
    /// folder that exists. Atomic: readers see the old bytes or the new ones, never part. A
    /// symlink is followed and stays a symlink. The worktree's watcher sends `DiffChanged`.
    pub async fn write_file(&self, worktree: &Path, file: &Path, bytes: &[u8]) -> Result<()> {
        self.check_tracked(worktree)?;
        let (dir, file, bytes) = (worktree.to_path_buf(), file.to_path_buf(), bytes.to_vec());
        self.runtime
            .spawn_blocking(move || write(&dir, &file, &bytes))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("write a file in", worktree)(io::Error::other(e)))?
    }
}

// git prints a conflicted path once per stage, and the untracked files apart from the rest, so
// this sorts and dedups. It blocks while git runs.
fn list(worktree: &Path) -> Result<Vec<PathBuf>> {
    let out = worktree::run(
        worktree,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    if !out.status.success() {
        return Err(CoreError::Git {
            path: worktree.to_path_buf(),
            reason: worktree::stderr(&out),
        });
    }
    let mut files: Vec<PathBuf> = out
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(OsStr::from_bytes(p)))
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

fn read(worktree: &Path, file: &Path) -> Result<Vec<u8>> {
    let real = real_path(worktree, file)?;
    fs::read(&real).map_err(CoreError::io("read", &real))
}

// Writes a temp file next to the real target, then renames it over the target.
fn write(worktree: &Path, file: &Path, bytes: &[u8]) -> Result<()> {
    let target = real_path(worktree, file)?;
    let temp = temp_path(&target).ok_or_else(|| CoreError::OutsideWorktree(file.to_path_buf()))?;
    let written = write_temp(&temp, &target, bytes)
        .and_then(|()| fs::rename(&temp, &target).map_err(CoreError::io("replace", &target)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

// An existing target keeps its mode, so a script stays executable. Until then the temp file is
// private, so a private file's new bytes are never readable by others. A new file gets the usual
// mode for a new file.
fn write_temp(temp: &Path, target: &Path, bytes: &[u8]) -> Result<()> {
    let io = |action| CoreError::io(action, temp);
    let existing = fs::metadata(target).ok().map(|meta| meta.permissions());
    let mode = if existing.is_some() { 0o600 } else { 0o666 };
    let mut out = File::options()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(temp)
        .map_err(io("create"))?;
    out.write_all(bytes).map_err(io("write"))?;
    if let Some(permissions) = existing {
        out.set_permissions(permissions)
            .map_err(io("set permissions on"))?;
    }
    out.sync_all().map_err(io("flush"))
}

// `.<name>.grove-<pid>-<n>` next to `target`. None when `target` has no folder or name.
fn temp_path(target: &Path) -> Option<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = OsString::from(".");
    name.push(target.file_name()?);
    name.push(format!(".grove-{}-{n}", process::id()));
    Some(target.parent()?.join(name))
}

// The real path `file` stands for, symlinks followed, `file`'s own too. Fails unless it is
// relative, has no `..` and ends inside the worktree. `file` itself may not exist yet, but its
// folder must.
fn real_path(worktree: &Path, file: &Path) -> Result<PathBuf> {
    diff::check_inside(file)?;
    let root = fs::canonicalize(worktree).map_err(CoreError::io("resolve", worktree))?;
    let path = worktree.join(file);
    let real = match fs::canonicalize(&path) {
        Ok(real) => real,
        // Nothing at `path`, not even a dangling symlink: a new file.
        Err(e) if e.kind() == ErrorKind::NotFound && fs::symlink_metadata(&path).is_err() => {
            let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
                return Err(CoreError::OutsideWorktree(file.to_path_buf()));
            };
            fs::canonicalize(folder)
                .map_err(CoreError::io("resolve", folder))?
                .join(name)
        }
        Err(e) => return Err(CoreError::io("resolve", &path)(e)),
    };
    if real.starts_with(&root) && real != root {
        Ok(real)
    } else {
        Err(CoreError::OutsideWorktree(file.to_path_buf()))
    }
}
