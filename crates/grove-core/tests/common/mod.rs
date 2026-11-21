//! Helpers shared by test files. A file that needs one declares `mod common;`.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The grove-hook binary, at a path that no build replaces while a test runs it.
///
/// `cargo test -p grove-core` does not build other packages, so this builds grove-hook. On macOS
/// every `cargo build` copies the binary out of `deps` again, even when nothing changed, and a
/// process started from it mid-copy fails or hangs. Every test binary builds, and under
/// cargo-nextest every test does, each in its own process. So one process builds at a time, and
/// tests run a copy that changes only when the binary does.
// Not every test file that declares `mod common;` uses it.
#[allow(dead_code)]
pub fn grove_hook() -> &'static Path {
    static RELAY: OnceLock<PathBuf> = OnceLock::new();
    RELAY.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("grove-hook");
        fs::create_dir_all(&dir).unwrap();
        // Held until this returns. Other test processes wait here.
        let build_lock = File::create(dir.join("build.lock")).unwrap();
        build_lock.lock().unwrap();

        // Its own target folder, so this never replaces the binary the workspace build made,
        // which the grove-hook tests run.
        let target = dir.join("target");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "--package", "grove-hook"])
            .arg("--target-dir")
            .arg(&target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .unwrap();
        assert!(status.success());
        let built = target.join("debug/grove-hook");

        let relay = dir.join("grove-hook");
        if fs::read(&relay).ok() != Some(fs::read(&built).unwrap()) {
            // A rename swaps the file in one step, so no process finds it missing or half written.
            let new_relay = dir.join("grove-hook.new");
            fs::copy(&built, &new_relay).unwrap();
            fs::rename(&new_relay, &relay).unwrap();
        }
        // On macOS the first run of the binary from a new process is slow. Pay that here, not in
        // a test's first hook. With no arguments it exits at once.
        assert!(Command::new(&relay).status().unwrap().success());
        relay
    })
}

/// Writes an executable `/bin/sh` script that runs `body`, ready to run with no first-run wait.
///
/// macOS checks a new executable on its first run. The checks run one at a time for the whole
/// machine, about 0.2 s each, so while other tests start scripts a first run can wait 10 s or
/// more, inside whatever deadline the test or the core gives it. On macOS this runs the script
/// once first and waits for it however long that takes. On that run the script exits at once.
// Not every test file that declares `mod common;` uses it.
#[allow(dead_code)]
pub fn write_script(path: &Path, body: &str) {
    fs::write(
        path,
        format!("#!/bin/sh\nif [ -n \"$GROVE_TEST_WARM_UP\" ]; then exit 0; fi\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    // Linux has no such check, and running a file just written there can fail with ETXTBSY
    // while a child another thread forked still holds the write fd.
    #[cfg(target_os = "macos")]
    assert!(
        Command::new(path)
            .env("GROVE_TEST_WARM_UP", "1")
            .status()
            .unwrap()
            .success()
    );
}

/// `messages` with their times taken out, to compare with messages a test writes, which cannot
/// know when the core got them. `times` checks the times.
#[allow(dead_code)]
pub fn untimed(
    messages: impl IntoIterator<Item = grove_core::AgentMessage>,
) -> Vec<grove_core::AgentMessage> {
    use grove_core::AgentMessage;
    messages
        .into_iter()
        .map(|mut message| {
            match &mut message {
                AgentMessage::TurnStarted { time_unix_ms }
                | AgentMessage::ToolCall { time_unix_ms, .. }
                | AgentMessage::TurnEnded { time_unix_ms, .. } => *time_unix_ms = None,
                _ => {}
            }
            message
        })
        .collect()
}

/// `messages` without the agent's command list, which a subscribe returns after the
/// conversation.
#[allow(dead_code)]
pub fn conversation_only(
    messages: impl IntoIterator<Item = grove_core::AgentMessage>,
) -> Vec<grove_core::AgentMessage> {
    messages
        .into_iter()
        .filter(|m| !matches!(m, grove_core::AgentMessage::Commands { .. }))
        .collect()
}

/// The times of the messages in `messages` that have one: turn starts, tool calls and turn ends.
#[allow(dead_code)]
pub fn times(messages: &[grove_core::AgentMessage]) -> Vec<Option<u64>> {
    use grove_core::AgentMessage;
    messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::TurnStarted { time_unix_ms }
            | AgentMessage::ToolCall { time_unix_ms, .. }
            | AgentMessage::TurnEnded { time_unix_ms, .. } => Some(*time_unix_ms),
            _ => None,
        })
        .collect()
}

/// The time now, in milliseconds since the Unix epoch.
#[allow(dead_code)]
pub fn now_unix_ms() -> u64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970");
    since_epoch.as_millis() as u64
}
