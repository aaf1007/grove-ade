//! Opening a worktree's file in the user's editor, at a line, showing a worktree in Finder, and
//! opening a web link in the user's browser.

use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

use crate::{Core, CoreError, Result, diff, worktree};

/// An editor grove can open a file in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editor {
    Zed,
    VsCode,
}

impl Core {
    /// Opens `file`, relative to the worktree at `worktree`, in `editor` at `line`.
    pub async fn open_in_editor(
        &self,
        editor: Editor,
        worktree: &Path,
        file: &Path,
        line: u32,
    ) -> Result<()> {
        self.check_tracked(worktree)?;
        diff::check_inside(file)?;
        let (dir, file) = (worktree.to_path_buf(), file.to_path_buf());
        self.runtime
            .spawn_blocking(move || open(editor, &dir, &file, line))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("open a file in", worktree)(io::Error::other(e)))?
    }

    /// Shows the worktree at `worktree` selected in a Finder window of its parent folder.
    pub async fn reveal_in_finder(&self, worktree: &Path) -> Result<()> {
        self.check_tracked(worktree)?;
        let dir = worktree.to_path_buf();
        self.runtime
            .spawn_blocking(move || reveal(&dir))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("reveal in Finder", worktree)(io::Error::other(e)))?
    }

    /// Opens `link` in the user's default browser. Only `http` and `https` links open.
    pub async fn open_link_in_default_browser(&self, link: &str) -> Result<()> {
        if !is_http_or_https(link) {
            return Err(CoreError::LinkIsNotHttpOrHttps {
                link: link.to_owned(),
            });
        }
        let owned = link.to_owned();
        self.runtime
            .spawn_blocking(move || open_link(&owned))
            .await
            // This fails only if the task panics.
            .map_err(|e| {
                CoreError::io("open in the browser", Path::new(link))(io::Error::other(e))
            })?
    }
}

// A link that starts with the scheme can be neither an option to `open` nor a file path.
fn is_http_or_https(link: &str) -> bool {
    let link = link.as_bytes();
    [b"http://".as_slice(), b"https://"].iter().any(|scheme| {
        link.len() > scheme.len() && link[..scheme.len()].eq_ignore_ascii_case(scheme)
    })
}

// Blocks while `open` runs. `open` hands a web link to the default browser.
fn open_link(link: &str) -> Result<()> {
    let path = Path::new(link);
    let out = Command::new("/usr/bin/open")
        .arg(link)
        .output()
        .map_err(CoreError::io("run open for", path))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(CoreError::io("open in the browser", path)(
            io::Error::other(worktree::stderr(&out)),
        ))
    }
}

// Blocks while `open` runs.
fn reveal(path: &Path) -> Result<()> {
    let out = Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path)
        .output()
        .map_err(CoreError::io("run open for", path))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(CoreError::RevealInFinder {
            reason: worktree::stderr(&out),
            path: path.to_path_buf(),
        })
    }
}

// Blocks while it resolves the path and `open` runs.
fn open(editor: Editor, worktree: &Path, file: &Path, line: u32) -> Result<()> {
    check_target_inside(worktree, file)?;
    let path = worktree.join(file);
    let out = Command::new("/usr/bin/open")
        .arg(url(editor, &path, line))
        .output()
        .map_err(CoreError::io("run open for", &path))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(CoreError::OpenInEditor {
            reason: worktree::stderr(&out),
            path,
        })
    }
}

// The editor follows a symlink, so the file it ends at must be inside the worktree too.
fn check_target_inside(worktree: &Path, file: &Path) -> Result<()> {
    let path = worktree.join(file);
    let root = fs::canonicalize(worktree).map_err(CoreError::io("resolve", worktree))?;
    let target = fs::canonicalize(&path).map_err(CoreError::io("resolve", &path))?;
    if target.starts_with(&root) {
        Ok(())
    } else {
        Err(CoreError::OutsideWorktree(file.to_path_buf()))
    }
}

// The editor's own URL scheme, which `open` hands to the app. The `zed` and `code` CLIs may not
// be installed, and an app started from Finder does not have them on its PATH.
fn url(editor: Editor, path: &Path, line: u32) -> String {
    let scheme = match editor {
        Editor::Zed => "zed",
        Editor::VsCode => "vscode",
    };
    let mut url = format!("{scheme}://file");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url.push_str(&format!(":{line}"));
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_opens_the_file_at_the_line_with_the_path_percent_encoded() {
        let path = Path::new("/code/my repo").join("src/a#b%.rs");

        assert_eq!(
            url(Editor::Zed, &path, 12),
            "zed://file/code/my%20repo/src/a%23b%25.rs:12"
        );
        assert_eq!(
            url(Editor::VsCode, &path, 1),
            "vscode://file/code/my%20repo/src/a%23b%25.rs:1"
        );
    }

    #[test]
    fn web_links_of_either_case_pass_the_check() {
        for link in [
            "http://example.com",
            "https://example.com/a?b#c",
            "HTTPS://Example.com",
        ] {
            assert!(is_http_or_https(link), "{link}");
        }
        for link in [
            "https://",
            "http:/example.com",
            "ftp://example.com",
            " https://example.com",
        ] {
            assert!(!is_http_or_https(link), "{link}");
        }
    }

    // Through the Core API this would start an editor, so it checks the path alone.
    #[test]
    fn a_symlink_to_a_file_in_the_same_worktree_can_be_opened() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/a.rs"), "a\n").unwrap();
        std::os::unix::fs::symlink("src/a.rs", dir.path().join("link")).unwrap();

        assert!(check_target_inside(dir.path(), Path::new("link")).is_ok());
        assert!(check_target_inside(dir.path(), Path::new("src/a.rs")).is_ok());
    }
}
