use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use grove_core::{Core, CoreConfig, CoreError};
use tempfile::TempDir;

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
// Tests keep the home folder, and the socket in it, under /tmp.
fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn accepts_connections(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

#[test]
fn start_on_an_empty_folder_creates_a_private_home_and_socket() {
    let dir = short_temp_dir();
    let cfg = CoreConfig::new(dir.path().join("home"));

    let (_core, _events) = Core::start(cfg.clone()).unwrap();

    assert_eq!(mode(&cfg.home), 0o700);
    assert!(fs::metadata(&cfg.socket).unwrap().file_type().is_socket());
    assert_eq!(mode(&cfg.socket), 0o600);
    assert!(accepts_connections(&cfg.socket));
}

#[test]
fn start_makes_an_existing_home_folder_private() {
    let dir = short_temp_dir();
    let cfg = CoreConfig::new(dir.path().join("home"));
    fs::create_dir(&cfg.home).unwrap();
    fs::set_permissions(&cfg.home, fs::Permissions::from_mode(0o755)).unwrap();

    let (_core, _events) = Core::start(cfg.clone()).unwrap();

    assert_eq!(mode(&cfg.home), 0o700);
}

#[test]
fn second_start_on_the_same_home_fails_while_the_first_runs() {
    let dir = short_temp_dir();
    let cfg = CoreConfig::new(dir.path().join("home"));
    let (_first, _events) = Core::start(cfg.clone()).unwrap();

    let second = Core::start(cfg.clone());

    assert!(matches!(second, Err(CoreError::AlreadyRunning(_))));
    assert!(accepts_connections(&cfg.socket));
}

#[test]
fn start_removes_a_socket_left_by_a_crashed_grove() {
    let dir = short_temp_dir();
    let cfg = CoreConfig::new(dir.path().join("home"));
    fs::create_dir(&cfg.home).unwrap();
    // A dropped std listener leaves its socket file behind, as a crash does.
    drop(UnixListener::bind(&cfg.socket).unwrap());
    assert!(!accepts_connections(&cfg.socket));

    let (_core, _events) = Core::start(cfg.clone()).unwrap();

    assert!(accepts_connections(&cfg.socket));
}

#[test]
fn stopping_the_core_removes_the_socket_and_releases_the_lock() {
    let dir = short_temp_dir();
    let cfg = CoreConfig::new(dir.path().join("home"));
    let (core, _events) = Core::start(cfg.clone()).unwrap();

    drop(core);

    assert!(!cfg.socket.exists());
    assert!(Core::start(cfg).is_ok());
}
