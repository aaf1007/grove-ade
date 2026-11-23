//! Runs a page that tries every way out in a real browser, in the frame an inline page gets:
//! `INLINE_PAGE_FRAME` served with `INLINE_PAGE_CSP`, in an iframe that only allows scripts
//! (ADR 0018). The same page in a frame with neither is the control: it reaches everything.
//!
//! It needs Chrome or Chromium. `GROVE_TEST_CHROME` names one. With none found the test fails in
//! CI and says so and passes elsewhere.

use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{INLINE_PAGE_CSP, INLINE_PAGE_FRAME};

/// The window: it holds what a page must not reach, and sends each frame the page as the app
/// does, once the frame says it is ready.
const WINDOW: &str = r#"<!doctype html><meta charset="utf-8">
<script>
window.__TAURI__ = { secret: "in the window" };
addEventListener("message", function (e) {
  for (const name of ["guarded", "open"]) {
    const frame = document.getElementById(name);
    if (!frame || e.source !== frame.contentWindow) continue;
    if (e.data.type === "grove-page-ready") {
      frame.contentWindow.postMessage({ type: "grove-page", document: PAGE.replaceAll("NAME", name) }, "*");
    }
    if (e.data.type === "report") {
      document.getElementById(name + "-report").textContent = JSON.stringify(e.data.got);
    }
  }
});
const PAGE = PAGE_JSON;
</script>
<iframe id="guarded" sandbox="allow-scripts" src="/guarded"></iframe>
<iframe id="open" src="/open"></iframe>
<pre id="guarded-report"></pre>
<pre id="open-report"></pre>
"#;

/// The page: a script, an image and a style from a server, the Tauri object, storage, and a
/// request. It reports what it got to the window, and the server sees what reached it.
const PAGE: &str = r#"<script src="ORIGIN/leak/NAME/script.js"></script>
<img src="ORIGIN/leak/NAME/image.png">
<link rel="stylesheet" href="ORIGIN/leak/NAME/style.css">
<script>
(async function () {
  var got = {};
  got.script = typeof leaked;
  got.tauri = typeof window.__TAURI__;
  try { got.parent = typeof parent.__TAURI__; } catch (e) { got.parent = "blocked"; }
  try { localStorage.setItem("a", "b"); got.storage = "open"; } catch (e) { got.storage = "blocked"; }
  try { await fetch("ORIGIN/leak/NAME/fetch"); got.fetch = "loaded"; } catch (e) { got.fetch = "blocked"; }
  try { navigator.sendBeacon("ORIGIN/leak/NAME/beacon"); } catch (e) {}
  parent.postMessage({ type: "report", got: got }, "*");
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
    let server = Server::start();

    let dom = dump_dom(&chrome, &server.origin);

    let guarded = report(&dom, "guarded");
    let open = report(&dom, "open");
    // The control shows the page does reach each thing when nothing stops it.
    assert_eq!(open["script"], "boolean", "{dom}");
    assert_eq!(open["parent"], "object", "{dom}");
    assert_eq!(open["storage"], "open", "{dom}");
    assert_eq!(open["fetch"], "loaded", "{dom}");
    let reached = |name: &str| server.requests(&format!("/leak/{name}/"));
    for kind in ["script.js", "image.png", "style.css", "fetch", "beacon"] {
        assert!(
            reached("open").contains(&format!("/leak/open/{kind}")),
            "the control never asked for {kind}: {:?}",
            reached("open")
        );
    }

    // The page ran, and got none of it.
    assert_eq!(guarded["script"], "undefined", "{dom}");
    assert_eq!(guarded["tauri"], "undefined", "{dom}");
    assert_eq!(guarded["parent"], "blocked", "{dom}");
    assert_eq!(guarded["storage"], "blocked", "{dom}");
    assert_eq!(guarded["fetch"], "blocked", "{dom}");
    assert_eq!(reached("guarded"), Vec::<String>::new());
}

/// What the frame called `name` reported, by key.
fn report(dom: &str, name: &str) -> HashMap<String, String> {
    let open = format!("<pre id=\"{name}-report\">");
    let start = dom
        .find(&open)
        .unwrap_or_else(|| panic!("no report of {name} in {dom}"))
        + open.len();
    let end = start + dom[start..].find("</pre>").unwrap();
    serde_json::from_str(&dom[start..end])
        .unwrap_or_else(|e| panic!("the {name} frame reported nothing: {e}\n{dom}"))
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

/// The window's DOM once its frames have run, as headless Chrome prints it.
fn dump_dom(chrome: &Path, origin: &str) -> String {
    let home = tempfile::tempdir().unwrap();
    let printed = home.path().join("dom.html");
    let mut browser = Command::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--virtual-time-budget=10000",
            "--dump-dom",
        ])
        .arg(format!(
            "--user-data-dir={}",
            home.path().join("profile").display()
        ))
        .arg(origin)
        .stdin(Stdio::null())
        .stdout(File::create(&printed).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    while browser.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            browser.kill().unwrap();
            panic!("Chrome did not finish");
        }
        thread::sleep(Duration::from_millis(50));
    }
    fs::read_to_string(&printed).unwrap()
}

/// A local server for the window, both frames, and everything a page tries to load.
struct Server {
    origin: String,
    paths: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let paths = Arc::new(Mutex::new(Vec::new()));
        let (seen, served) = (Arc::clone(&paths), origin.clone());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (seen, served) = (Arc::clone(&seen), served.clone());
                thread::spawn(move || answer(stream, &served, &seen));
            }
        });
        Self { origin, paths }
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

fn answer(mut stream: TcpStream, origin: &str, seen: &Mutex<Vec<String>>) {
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return;
    }
    let Some(path) = line.split(' ').nth(1) else {
        return;
    };
    seen.lock().unwrap().push(path.to_owned());
    let policy = format!("Content-Security-Policy: {INLINE_PAGE_CSP}\r\n");
    let (kind, headers, body) = match path {
        "/" => {
            let page = serde_json::to_string(&PAGE.replace("ORIGIN", origin)).unwrap();
            // A closing tag in the string would end the window's own script.
            let page = page.replace("</", "<\\/");
            ("text/html", "", WINDOW.replace("PAGE_JSON", &page))
        }
        "/guarded" => ("text/html", policy.as_str(), INLINE_PAGE_FRAME.to_owned()),
        "/open" => ("text/html", "", INLINE_PAGE_FRAME.to_owned()),
        _ if path.ends_with(".js") => ("text/javascript", "", "var leaked = true;".to_owned()),
        _ => ("text/plain", "", String::new()),
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}; charset=utf-8\r\n{headers}\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}
