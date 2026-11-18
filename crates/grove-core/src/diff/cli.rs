//! The git CLI's diff, for files libgit2 cannot diff (ADR 0012).

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{DiffLine, FileDiff, FileStatus, Hunk, LineKind};
use crate::{CoreError, Result};

/// `git diff <from>` for `paths` only, in the structured form. git runs with the user's and the
/// system's config, attributes and ignore files hidden, as ADR 0012 pins them, plus `-c` for each
/// pair in `config`. It blocks while git runs.
pub(super) fn diff(
    worktree: &Path,
    from: &str,
    paths: &[PathBuf],
    config: &[OsString],
) -> Result<Vec<FileDiff>> {
    let mut cmd = Command::new("git");
    // An inherited `GIT_DIR` or `GIT_INDEX_FILE` would point git somewhere else.
    for (key, _) in std::env::vars_os() {
        if key.as_bytes().starts_with(b"GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd.envs([
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("XDG_CONFIG_HOME", "/dev/null"),
    ])
    .arg("-C")
    .arg(worktree);
    for pair in config {
        cmd.arg("-c").arg(pair);
    }
    // `--raw -z` gives each file's status and paths unquoted. `--no-ext-diff` keeps the patch
    // unified even when a driver sets an external `command`.
    cmd.args([
        "diff",
        from,
        "--raw",
        "-p",
        "-z",
        "-M",
        "--no-color",
        "--no-ext-diff",
        "--",
    ]);
    for path in paths {
        let mut spec = OsString::from(":(literal)");
        spec.push(path);
        cmd.arg(spec);
    }
    let out = cmd
        .output()
        .map_err(CoreError::io("run git in", worktree))?;
    let failed = |reason: String| CoreError::Git {
        path: worktree.to_path_buf(),
        reason,
    };
    if !out.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    parse(&out.stdout).ok_or_else(|| failed("could not read the output of git diff".into()))
}

// `--raw -p -z` prints NUL-terminated raw records, an empty field, then one patch per record in
// the same order.
fn parse(out: &[u8]) -> Option<Vec<FileDiff>> {
    if out.is_empty() {
        return Some(Vec::new());
    }
    let mut rest = out;
    let mut files = Vec::new();
    loop {
        let meta = next_field(&mut rest)?;
        if meta.is_empty() {
            break;
        }
        let letter = *meta.rsplit(|&b| b == b' ').next()?.first()?;
        let old_path = PathBuf::from(OsStr::from_bytes(next_field(&mut rest)?));
        let new_path = match letter {
            b'R' => PathBuf::from(OsStr::from_bytes(next_field(&mut rest)?)),
            _ => old_path.clone(),
        };
        // A typechange prints two patches for one record, the old file deleted and then the new
        // one added, so it becomes two files as git2 splits it. The rest cannot occur in a
        // `git diff <commit>`, so anything else is output grove does not read.
        let statuses: &[FileStatus] = match letter {
            b'A' => &[FileStatus::Added],
            b'D' => &[FileStatus::Deleted],
            b'M' => &[FileStatus::Modified],
            b'R' => &[FileStatus::Renamed],
            b'T' => &[FileStatus::Deleted, FileStatus::Added],
            _ => return None,
        };
        for &status in statuses {
            files.push(FileDiff {
                status,
                old_path: old_path.clone(),
                new_path: new_path.clone(),
                binary: false,
                hunks: Vec::new(),
            });
        }
    }

    let mut patches: Vec<Vec<&[u8]>> = Vec::new();
    for line in rest.split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b"diff --git ") {
            patches.push(Vec::new());
        }
        patches.last_mut()?.push(line);
    }
    if patches.len() != files.len() {
        return None;
    }
    for (file, patch) in files.iter_mut().zip(patches) {
        (file.binary, file.hunks) = parse_patch(&patch)?;
    }
    Some(files)
}

fn next_field<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let end = rest.iter().position(|&b| b == 0)?;
    let field = &rest[..end];
    *rest = &rest[end + 1..];
    Some(field)
}

// The binary flag and hunks of one file's patch. Each line keeps its newline.
fn parse_patch(patch: &[&[u8]]) -> Option<(bool, Vec<Hunk>)> {
    let mut lines = patch.iter().copied().peekable();
    let (mut binary, mut hunks) = (false, Vec::new());
    while let Some(line) = lines.next() {
        if hunks.is_empty() && line.starts_with(b"Binary files ") {
            binary = true;
        }
        let Some(header) = line.strip_prefix(b"@@ -") else {
            continue;
        };
        let end = header.windows(3).position(|w| w == b" @@")?;
        let (old, new) = std::str::from_utf8(&header[..end]).ok()?.split_once(" +")?;
        let ((old_start, old_lines), (new_start, new_lines)) = (range(old)?, range(new)?);
        let mut hunk = Hunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            lines: Vec::new(),
        };
        let (mut old_left, mut new_left) = (old_lines, new_lines);
        while old_left > 0 || new_left > 0 || lines.peek().is_some_and(|l| l.starts_with(b"\\")) {
            let (&origin, text) = lines.next()?.split_first()?;
            let kind = match origin {
                b' ' => {
                    old_left = old_left.checked_sub(1)?;
                    new_left = new_left.checked_sub(1)?;
                    LineKind::Context
                }
                b'-' => {
                    old_left = old_left.checked_sub(1)?;
                    LineKind::Removed
                }
                b'+' => {
                    new_left = new_left.checked_sub(1)?;
                    LineKind::Added
                }
                // "\ No newline at end of file": the line before has no newline.
                b'\\' => {
                    hunk.lines.last_mut()?.text.pop();
                    continue;
                }
                _ => return None,
            };
            hunk.lines.push(DiffLine {
                kind,
                text: text.to_vec(),
            });
        }
        hunks.push(hunk);
    }
    Some((binary, hunks))
}

// `start,count`, or `start` alone for a count of 1.
fn range(r: &str) -> Option<(u32, u32)> {
    match r.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((r.parse().ok()?, 1)),
    }
}
