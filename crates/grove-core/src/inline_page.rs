//! Inline pages: small HTML pages an agent session in chat mode shows in its conversation with
//! `html_show`, through grove-mcp (ADR 0018). grove keeps what each page held when it was shown,
//! in the session's folder, so the page comes back whenever the conversation does.

use std::collections::HashSet;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::random_bytes;
use crate::{AgentMessage, Core, CoreError, Result, SessionId};

/// The Content Security Policy of the frame an inline page renders in. Nothing loads from
/// anywhere: scripts and styles run only inline, and images, fonts and media come only from
/// `data:` and `blob:` URLs the page makes itself. `sandbox` gives the page scripts and nothing
/// else: its origin is its own, so it reaches no storage and no other frame.
pub const INLINE_PAGE_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; \
    style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; \
    form-action 'none'; base-uri 'none'; sandbox allow-scripts";

/// The document an inline page's frame starts with. The app's own policy would apply to a
/// document the window wrote into the frame, so the frame loads this one, served with
/// `INLINE_PAGE_CSP`. It tells the window it is ready, and the window sends it the page, which
/// replaces it. Only the window that holds the frame can send one.
pub const INLINE_PAGE_FRAME: &str = "<!doctype html><meta charset=\"utf-8\"><script>\
    addEventListener(\"message\",function(e){\
    if(e.source!==parent||!e.data||e.data.type!==\"grove-page\")return;\
    document.open();document.write(e.data.document);document.close()});\
    parent.postMessage({type:\"grove-page-ready\"},\"*\")\
    </script>";

/// The most bytes of HTML one page holds.
const MAX_PAGE_BYTES: usize = 1024 * 1024;
/// The tallest a page's frame gets, in CSS pixels. Past it the frame scrolls.
const MAX_PAGE_HEIGHT: u32 = 2000;

/// The folder in a session's folder that holds its pages, one JSON file each.
const PAGES: &str = "pages";
/// A page's id is this and 16 hex digits. The reply to `html_show` names it, so the agent's
/// record of the call does too, and a resumed conversation finds the page by it.
const ID_PREFIX: &str = "page-";
const ID_DIGITS: usize = 16;

/// A page as grove keeps it.
#[derive(Serialize, Deserialize)]
struct SavedPage {
    title: String,
    html: String,
    height: Option<u32>,
}

impl SavedPage {
    fn into_message(self, id: String) -> AgentMessage {
        AgentMessage::InlinePage {
            id,
            title: self.title,
            html: self.html,
            height: self.height,
        }
    }
}

impl Core {
    /// Shows a page in the conversation of agent session `session`, which must be in chat mode:
    /// `html`, or what the `.html` file at `path` in the session's worktree holds now. `height`
    /// is the frame's height in CSS pixels, capped at 2000. None or 0 fits the page's content.
    /// Returns the page's id.
    pub(crate) fn show_inline_page(
        &self,
        session: SessionId,
        title: String,
        html: Option<String>,
        path: Option<PathBuf>,
        height: Option<u32>,
    ) -> Result<String> {
        let chat = self.agents.chat(session)?;
        if title.trim().is_empty() {
            return Err(CoreError::PageNeedsTitle);
        }
        let html = match (html, path) {
            (Some(html), None) => html,
            (None, Some(path)) => read_page(&self.caller_worktree(session)?, &path)?,
            _ => return Err(CoreError::PageNeedsOneSource),
        };
        if html.len() > MAX_PAGE_BYTES {
            return Err(too_large(html.len() as u64));
        }
        if html.trim().is_empty() {
            return Err(CoreError::EmptyPage);
        }
        let page = SavedPage {
            title,
            html,
            height: height.filter(|&h| h > 0).map(|h| h.min(MAX_PAGE_HEIGHT)),
        };
        let id = new_id()?;
        // Saved before it shows, so a page in the conversation is always one a resume finds.
        save(&self.agents.session_folder(session), &id, &page)?;
        chat.add(page.into_message(id.clone()));
        Ok(id)
    }
}

fn too_large(bytes: u64) -> CoreError {
    CoreError::PageTooLarge {
        bytes,
        limit: MAX_PAGE_BYTES,
    }
}

fn new_id() -> Result<String> {
    let bytes: [u8; ID_DIGITS / 2] = random_bytes()?;
    let digits: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{ID_PREFIX}{digits}"))
}

/// The text of the `.html` file at `path`, which must be absolute and, symlinks and `..`
/// followed, inside `worktree`.
fn read_page(worktree: &Path, path: &Path) -> Result<String> {
    let refused = |reason| CoreError::NotAPage {
        path: path.to_path_buf(),
        reason,
    };
    if !path.is_absolute() {
        return Err(refused("it is not an absolute path"));
    }
    let is_html = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("html"));
    if !is_html {
        return Err(refused("it is not an .html file"));
    }
    let root = fs::canonicalize(worktree).map_err(CoreError::io("resolve", worktree))?;
    let real = fs::canonicalize(path).map_err(CoreError::io("resolve", path))?;
    if !real.starts_with(&root) {
        return Err(CoreError::OutsideWorktree(path.to_path_buf()));
    }
    let metadata = fs::metadata(&real).map_err(CoreError::io("read", &real))?;
    if !metadata.is_file() {
        return Err(refused("it is not a file"));
    }
    // Checked before it is read, so a huge file is never read whole.
    if metadata.len() > MAX_PAGE_BYTES as u64 {
        return Err(too_large(metadata.len()));
    }
    fs::read_to_string(&real).map_err(|e| match e.kind() {
        ErrorKind::InvalidData => refused("it is not UTF-8 text"),
        _ => CoreError::io("read", &real)(e),
    })
}

fn save(session_folder: &Path, id: &str, page: &SavedPage) -> Result<()> {
    let folder = session_folder.join(PAGES);
    fs::create_dir_all(&folder).map_err(CoreError::io("create", &folder))?;
    let file = folder.join(format!("{id}.json"));
    let json = serde_json::to_vec(page).map_err(|e| CoreError::io("write", &file)(e.into()))?;
    fs::write(&file, json).map_err(CoreError::io("write", &file))
}

fn load(session_folder: &Path, id: &str) -> io::Result<SavedPage> {
    let json = fs::read(session_folder.join(PAGES).join(format!("{id}.json")))?;
    Ok(serde_json::from_slice(&json)?)
}

/// Brings a session's pages back into a conversation read from the agent's record. The record
/// holds each `html_show` call and its result, which names the page.
pub(crate) struct SavedPages {
    session_folder: PathBuf,
    restored: HashSet<String>,
}

impl SavedPages {
    pub(crate) fn of(session_folder: PathBuf) -> Self {
        Self {
            session_folder,
            restored: HashSet::new(),
        }
    }

    /// The pages `output`, the result of a tool call that worked, names for the first time.
    /// The first result to name a page is the call that showed it, as its id is random. An id
    /// with no page saved, or one that cannot be read, gives none: the call's row still shows.
    pub(crate) fn named_in(&mut self, output: &str) -> Vec<AgentMessage> {
        ids_in(output)
            .filter_map(|id| {
                if self.restored.contains(id) {
                    return None;
                }
                let page = load(&self.session_folder, id).ok()?;
                self.restored.insert(id.to_owned());
                Some(page.into_message(id.to_owned()))
            })
            .collect()
    }
}

/// Every page id in `text`, in order.
fn ids_in(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices(ID_PREFIX).filter_map(|(at, _)| {
        let id = text.get(at..at + ID_PREFIX.len() + ID_DIGITS)?;
        let digits = &id[ID_PREFIX.len()..];
        digits
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            .then_some(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_id_is_one_ids_in_finds() {
        let id = new_id().unwrap();

        let text = format!("Showed the page in the conversation as {id}.");
        assert_eq!(ids_in(&text).collect::<Vec<_>>(), [id.as_str()]);
    }

    #[test]
    fn only_the_prefix_and_16_hex_digits_are_an_id() {
        let ids = |text| ids_in(text).collect::<Vec<_>>();

        assert_eq!(
            ids("page-0123456789abcdef and page-fedcba9876543210."),
            ["page-0123456789abcdef", "page-fedcba9876543210"]
        );
        assert!(ids("page-0123456789abcde").is_empty());
        assert!(ids("page-0123456789ABCDEF").is_empty());
        assert!(ids("page-../../../../token").is_empty());
        assert!(ids("page-0123456789abcdé").is_empty());
    }

    #[test]
    fn the_frame_policy_loads_nothing_and_sandboxes_the_page() {
        let directives: Vec<&str> = INLINE_PAGE_CSP.split("; ").collect();

        // Every kind of load falls back to this one unless a directive below names it.
        assert!(directives.contains(&"default-src 'none'"));
        // The ones named allow only what the page itself holds: no host, no scheme that loads.
        for directive in &directives {
            let sources = directive.split(' ').skip(1);
            for source in sources {
                assert!(
                    [
                        "'none'",
                        "'unsafe-inline'",
                        "data:",
                        "blob:",
                        "allow-scripts"
                    ]
                    .contains(&source),
                    "{directive}"
                );
            }
        }
        // Scripts run, and nothing else the sandbox can give: no same origin, forms, popups
        // or navigation of the window.
        assert!(directives.contains(&"sandbox allow-scripts"));
    }
}
