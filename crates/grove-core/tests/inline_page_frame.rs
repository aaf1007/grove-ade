//! Runs pages in a real browser, in the frame an inline page gets: `INLINE_PAGE_FRAME` served
//! with `INLINE_PAGE_CSP`, in an iframe that only allows scripts (ADR 0018).
//!
//! One page tries every way out. The same page in a frame with neither guard is the control: it
//! reaches everything. The others say how tall they are, with the script the UI puts in each
//! page.
//!
//! It needs Chrome or Chromium. `GROVE_TEST_CHROME` names one. With none found the test fails in
//! CI and says so and passes elsewhere.

use std::collections::HashMap;
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{INLINE_PAGE_CSP, INLINE_PAGE_FRAME};

/// The window: it holds what a page must not reach, and sends each frame the page as the app
/// does, once the frame says it is ready. It tells the server what both pages reported.
const WINDOW: &str = r#"<!doctype html><meta charset="utf-8">
<script>
window.__TAURI__ = { secret: "in the window" };
const reports = {};
addEventListener("message", function (e) {
  for (const name of ["guarded", "open"]) {
    const frame = document.getElementById(name);
    if (!frame || e.source !== frame.contentWindow) continue;
    if (e.data.type === "grove-page-ready") {
      frame.contentWindow.postMessage({ type: "grove-page", document: PAGE.replaceAll("NAME", name) }, "*");
    }
    if (e.data.type === "report") {
      for (const key in e.data.got) reports[name + "." + key] = e.data.got[key];
      reports[name] = "reported";
      if (reports.guarded && reports.open) fetch("/done?" + new URLSearchParams(reports));
    }
  }
});
const PAGE = PAGE_JSON;
</script>
<iframe id="guarded" sandbox="allow-scripts" src="/guarded"></iframe>
<iframe id="open" src="/open"></iframe>
"#;

/// The page: a script, an image and a style from a server, the Tauri object, storage, a
/// request, a beacon, a socket, a new window, and a link that sends its own frame elsewhere. It
/// reports what it got to the window, and the server sees what reached it.
const PAGE: &str = r#"<script src="ORIGIN/leak/NAME/script.js"></script>
<img src="ORIGIN/leak/NAME/image.png">
<link rel="stylesheet" href="ORIGIN/leak/NAME/style.css">
<a id="link" href="ORIGIN/leak/NAME/link">out</a>
<script>
(async function () {
  var got = {};
  got.script = typeof leaked;
  got.tauri = typeof window.__TAURI__;
  try { got.parent = typeof parent.__TAURI__; } catch (e) { got.parent = "blocked"; }
  try { localStorage.setItem("a", "b"); got.storage = "open"; } catch (e) { got.storage = "blocked"; }
  try { await fetch("ORIGIN/leak/NAME/fetch"); got.fetch = "loaded"; } catch (e) { got.fetch = "blocked"; }
  try { navigator.sendBeacon("ORIGIN/leak/NAME/beacon"); } catch (e) {}
  // Waits for the socket to fail or open, as the link below would end it first.
  await new Promise(function (settled) {
    try {
      var socket = new WebSocket("SOCKET/leak/NAME/socket");
      socket.onopen = socket.onerror = socket.onclose = settled;
    } catch (e) { settled(); }
    setTimeout(settled, 3000);
  });
  try { got.popup = window.open("ORIGIN/leak/NAME/popup") ? "opened" : "blocked"; } catch (e) { got.popup = "blocked"; }
  parent.postMessage({ type: "report", got: got }, "*");
  // Last, as it takes the frame away from this page.
  document.getElementById("link").click();
})();
</script>
"#;

#[test]
fn a_page_that_reaches_for_tauri_fetch_or_a_remote_script_gets_nothing() {
    let Some(chrome) = chrome() else {
        assert!(
            env::var_os("CI").is_none(),
            "no Chrome or Chromium to run the page in: set GROVE_TEST_CHROME"
        );
        eprintln!("skipped: no Chrome or Chromium to run the page in");
        return;
    };
    // The window lets a frame go where the app's own policy does: to the frame document and
    // nowhere else. The control may also follow its link, to show the link is tried.
    let server = Server::start(|origin| Window {
        html: WINDOW.replace("PAGE_JSON", &in_a_script(&hostile_page(origin))),
        policy: Some(format!(
            "frame-src {origin}/guarded {origin}/open {origin}/leak/open/"
        )),
    });

    let _browser = Browser::open(&chrome, &server.origin);
    let got = server.reported();

    // The control shows the page does reach each thing when nothing stops it.
    assert_eq!(got["open.script"], "boolean", "{got:?}");
    assert_eq!(got["open.parent"], "object", "{got:?}");
    assert_eq!(got["open.storage"], "open", "{got:?}");
    assert_eq!(got["open.fetch"], "loaded", "{got:?}");
    assert_eq!(got["open.popup"], "opened", "{got:?}");
    for kind in [
        "script.js",
        "image.png",
        "style.css",
        "fetch",
        "beacon",
        "socket",
        "popup",
        "link",
    ] {
        server.wait_for(&format!("/leak/open/{kind}"));
    }

    // The page ran, and got none of it.
    assert_eq!(got["guarded.script"], "undefined", "{got:?}");
    assert_eq!(got["guarded.tauri"], "undefined", "{got:?}");
    assert_eq!(got["guarded.parent"], "blocked", "{got:?}");
    assert_eq!(got["guarded.storage"], "blocked", "{got:?}");
    assert_eq!(got["guarded.fetch"], "blocked", "{got:?}");
    assert_eq!(got["guarded.popup"], "blocked", "{got:?}");
    // Both pages ran the same steps at the same time, and the control's last request is in.
    // A little longer, for one from the guarded page that was still on its way.
    thread::sleep(Duration::from_millis(500));
    assert_eq!(server.requests("/leak/guarded/"), Vec::<String>::new());
}

/// The script the UI puts in each page, which tells the window how tall the page is.
const PAGE_SCRIPT: &str = include_str!("../../../app/src/inlinePageScript.js");

/// A window with one frame per page, each 150 pixels tall. It sends each its page, and tells
/// the server the last height each page said, once all have and none has said another since.
const HEIGHTS_WINDOW: &str = r#"<!doctype html><meta charset="utf-8">
<script>
const PAGES = PAGES_JSON;
const heights = {};
let settled;
addEventListener("message", function (e) {
  for (const name of Object.keys(PAGES)) {
    const frame = document.getElementById(name);
    if (!frame || e.source !== frame.contentWindow) continue;
    if (e.data.type === "grove-page-ready") {
      frame.contentWindow.postMessage({ type: "grove-page", document: PAGES[name] }, "*");
    }
    if (e.data.type === "grove-page-height") {
      heights[name] = e.data.height;
      if (Object.keys(heights).length < Object.keys(PAGES).length) continue;
      clearTimeout(settled);
      settled = setTimeout(function () { fetch("/done?" + new URLSearchParams(heights)); }, 300);
    }
  }
});
</script>
<iframe id="flow" sandbox="allow-scripts" src="/guarded" style="height:150px"></iframe>
<iframe id="short" sandbox="allow-scripts" src="/guarded" style="height:150px"></iframe>
<iframe id="placed" sandbox="allow-scripts" src="/guarded" style="height:150px"></iframe>
"#;

#[test]
fn a_page_says_how_tall_its_content_is_whatever_the_frames_height() {
    let Some(chrome) = chrome() else {
        assert!(
            env::var_os("CI").is_none(),
            "no Chrome or Chromium to run the page in: set GROVE_TEST_CHROME"
        );
        eprintln!("skipped: no Chrome or Chromium to run the page in");
        return;
    };
    let page = |body: &str| {
        format!(
            "<!doctype html><style id=\"grove-theme\"></style><style>body{{margin:0}}</style>\
             <script>{PAGE_SCRIPT}</script>{body}"
        )
    };
    let pages = serde_json::json!({
        // Taller than its frame.
        "flow": page("<div style=\"height:300px\">Chart</div>"),
        // Shorter than its frame: the frame's own height must not count.
        "short": page("<div style=\"height:100px\">Chart</div>"),
        // Out of the flow, so the page's root has no height of its own.
        "placed": page(
            "<div style=\"position:absolute;top:0;left:0;width:200px;height:400px\">Diagram</div>"
        ),
    });
    let server = Server::start(move |_| Window {
        html: HEIGHTS_WINDOW.replace("PAGES_JSON", &in_a_script(&pages)),
        policy: None,
    });

    let _browser = Browser::open(&chrome, &server.origin);
    let heights = server.reported();

    assert_eq!(heights["flow"], "300", "{heights:?}");
    assert_eq!(heights["short"], "100", "{heights:?}");
    assert_eq!(heights["placed"], "400", "{heights:?}");
}

fn hostile_page(origin: &str) -> String {
    PAGE.replace("ORIGIN", origin)
        .replace("SOCKET", &origin.replace("http://", "ws://"))
}

/// `value` as JSON that can sit in a `<script>`: a closing tag in it would end the script.
fn in_a_script(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap().replace("</", "<\\/")
}

fn chrome() -> Option<PathBuf> {
    if let Some(named) = env::var_os("GROVE_TEST_CHROME") {
        return Some(named.into());
    }
    let mac = Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    if mac.exists() {
        return Some(mac.to_path_buf());
    }
    let path = env::var_os("PATH")?;
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ];
    env::split_paths(&path)
        .flat_map(|folder| names.map(|name| folder.join(name)))
        .find(|candidate| candidate.is_file())
}

/// A headless Chrome showing one window, until it is dropped.
struct Browser {
    process: Child,
    _home: tempfile::TempDir,
}

impl Browser {
    fn open(chrome: &Path, url: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let process = Command::new(chrome)
            .args([
                "--headless=new",
                "--disable-gpu",
                "--no-first-run",
                "--no-default-browser-check",
                // The control opens a window with no click. Chrome would stop that itself.
                "--disable-popup-blocking",
            ])
            .arg(format!("--user-data-dir={}", home.path().display()))
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self {
            process,
            _home: home,
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// The window a test's server serves at `/`, and the policy it comes with, if any.
struct Window {
    html: String,
    policy: Option<String>,
}

/// A local server for a window, its frames, and everything a page tries to load.
struct Server {
    origin: String,
    paths: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// Serves the window `window` makes for the server's origin.
    fn start(window: impl FnOnce(&str) -> Window) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let paths = Arc::new(Mutex::new(Vec::new()));
        let (seen, window) = (Arc::clone(&paths), Arc::new(window(&origin)));
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (seen, window) = (Arc::clone(&seen), Arc::clone(&window));
                thread::spawn(move || answer(stream, &window, &seen));
            }
        });
        Self { origin, paths }
    }

    /// What the window reported: the fields of its `/done?` request, by name.
    fn reported(&self) -> HashMap<String, String> {
        let done = self.wait_for("/done?");
        let fields = done["/done?".len()..].split('&');
        fields
            .filter_map(|field| field.split_once('='))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }

    /// The first path asked for that starts with `prefix`, once one is.
    fn wait_for(&self, prefix: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(path) = self.requests(prefix).into_iter().next() {
                return path;
            }
            assert!(
                Instant::now() < deadline,
                "nothing asked for {prefix}: {:?}",
                self.requests("/")
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Every path asked for so far that starts with `prefix`.
    fn requests(&self, prefix: &str) -> Vec<String> {
        let paths = self.paths.lock().unwrap();
        paths
            .iter()
            .filter(|path| path.starts_with(prefix))
            .cloned()
            .collect()
    }
}

fn answer(mut stream: TcpStream, window: &Window, seen: &Mutex<Vec<String>>) {
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return;
    }
    let Some(path) = line.split(' ').nth(1) else {
        return;
    };
    seen.lock().unwrap().push(path.to_owned());
    let policy = |policy: &str| format!("Content-Security-Policy: {policy}\r\n");
    let (kind, headers, body) = match path {
        "/" => (
            "text/html",
            window.policy.as_deref().map(policy).unwrap_or_default(),
            window.html.clone(),
        ),
        "/guarded" => (
            "text/html",
            policy(INLINE_PAGE_CSP),
            INLINE_PAGE_FRAME.to_owned(),
        ),
        "/open" => ("text/html", String::new(), INLINE_PAGE_FRAME.to_owned()),
        _ if path.ends_with(".js") => (
            "text/javascript",
            String::new(),
            "var leaked = true;".to_owned(),
        ),
        _ => ("text/plain", String::new(), String::new()),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}; charset=utf-8\r\n{headers}\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}
