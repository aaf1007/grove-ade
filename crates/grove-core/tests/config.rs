use std::path::{Path, PathBuf};
use std::time::Duration;

use grove_core::CoreConfig;

#[test]
fn the_user_default_home_folder_is_grovecode_in_the_home_directory() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());

    let cfg = CoreConfig::for_user().unwrap();

    assert_eq!(cfg.home, home.join(".grovecode"));
}

#[test]
fn the_user_default_login_shell_is_shell_or_zsh() {
    let shell = std::env::var_os("SHELL").map_or_else(|| "/bin/zsh".into(), PathBuf::from);

    let cfg = CoreConfig::for_user().unwrap();

    assert_eq!(cfg.login_shell, Some(shell));
}

#[test]
fn defaults_follow_the_architecture_doc() {
    let cfg = CoreConfig::new("/g");

    assert_eq!(cfg.socket, Path::new("/g/grove.sock"));
    assert_eq!(cfg.hook_relay, Path::new("/g/bin/grove-hook"));
    assert_eq!(cfg.claude_command, Path::new("claude"));
    assert_eq!(cfg.codex_command, Path::new("codex"));
    assert_eq!(cfg.output_flush_window, Duration::from_millis(8));
    assert_eq!(cfg.output_high_watermark, 128 * 1024);
    assert_eq!(cfg.output_low_watermark, 32 * 1024);
    assert_eq!(cfg.needs_input_to_working, Duration::from_secs(2));
    assert_eq!(cfg.stuck_working_after, Duration::from_secs(10));
    assert_eq!(cfg.login_shell, None);
    assert_eq!(cfg.login_shell_timeout, Duration::from_secs(5));
}
