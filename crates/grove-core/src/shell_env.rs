//! The environment every session starts with: the user's login shell environment, read once at
//! start, plus grove's terminal variables.

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::{self, PipeReader, PipeWriter, Read};
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::ptr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

#[cfg(not(target_os = "macos"))]
use libc::POSIX_SPAWN_SETSID;

use crate::Warning;

// The libc crate leaves it out on macOS. From <sys/spawn.h>.
#[cfg(target_os = "macos")]
const POSIX_SPAWN_SETSID: libc::c_short = 0x0400;

pub(crate) type Env = BTreeMap<OsString, OsString>;

// The PATH launchd gives an app started from the Dock.
const MINIMAL_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
// The login shell prints its environment between these, so profile output around it is ignored.
const START: &str = "__grove_env_start__";
const END: &str = "__grove_env_end__";

#[derive(Debug, thiserror::Error)]
enum ReadError {
    #[error("could not start {}: {source}", shell.display())]
    Start {
        shell: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} did not print its environment within {timeout:?}", shell.display())]
    TimedOut { shell: PathBuf, timeout: Duration },
    #[error("{} printed no environment", .0.display())]
    NoEnv(PathBuf),
}

/// The environment for every session. When the login shell's environment cannot be read, it
/// is the minimal one and the warning says why.
pub(crate) fn for_sessions(shell: Option<&Path>, timeout: Duration) -> (Env, Option<Warning>) {
    let base = base(shell);
    let (mut env, warning) = match shell.map(|shell| read(shell, &base, timeout)) {
        None => (base, None),
        Some(Ok(env)) => (env, None),
        Some(Err(e)) => (
            base,
            Some(Warning::LoginShellEnv {
                reason: e.to_string(),
            }),
        ),
    };
    // The login shell set these for itself. A session runs in another folder.
    for key in ["PWD", "OLDPWD", "SHLVL", "_"] {
        env.remove(OsStr::new(key));
    }
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("TERM_PROGRAM".into(), "grove".into());
    env.entry("LANG".into())
        .or_insert_with(|| "en_US.UTF-8".into());
    (env, warning)
}

// What the login shell starts from. Nothing else in grove's own environment gets through.
fn base(shell: Option<&Path>) -> Env {
    let mut env: Env = ["HOME", "USER", "LOGNAME", "SHELL", "TMPDIR"]
        .into_iter()
        .filter_map(|key| Some((key.into(), std::env::var_os(key)?)))
        .collect();
    if let Some(shell) = shell {
        env.insert("SHELL".into(), shell.into());
    }
    env.insert("PATH".into(), MINIMAL_PATH.into());
    env
}

fn read(shell: &Path, base: &Env, timeout: Duration) -> Result<Env, ReadError> {
    // Plain enough for zsh, bash and fish alike.
    let script = format!("printf '\\n%s\\n' {START}; /usr/bin/env -0; printf '\\n%s\\n' {END}");
    let start_failed = |source| ReadError::Start {
        shell: shell.to_path_buf(),
        source,
    };
    let (stdout, writer) = io::pipe().map_err(start_failed)?;
    let pid = spawn_shell_in_own_session(shell, &["-ilc", &script], base, None, &writer, None)
        .map_err(start_failed)?;
    drop(writer);
    let (tx, rx) = mpsc::channel();
    // Only the profile's own processes can keep the pipe open, so the thread may outlive this
    // call. The receiver is gone by then.
    let _ = thread::Builder::new()
        .name("grove-login-env".into())
        .spawn(move || {
            let _ = tx.send(read_env(stdout));
        });
    let result = match rx.recv_timeout(timeout) {
        // Empty when `env -0` itself failed.
        Ok(Some(env)) if !env.is_empty() => Ok(env),
        Ok(_) | Err(RecvTimeoutError::Disconnected) => Err(ReadError::NoEnv(shell.into())),
        Err(RecvTimeoutError::Timeout) => {
            // The shell leads its session, and so its process group.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            Err(ReadError::TimedOut {
                shell: shell.into(),
                timeout,
            })
        }
    };
    reap(pid);
    result
}

// Starts `shell` with `args` and only `env`, in `folder` when given, with stdin on /dev/null,
// stdout on `stdout`, and stderr on `stderr` or else /dev/null. It leads a session of its own,
// so `kill_session` with its pid ends everything it starts, and it has no controlling
// terminal: an interactive shell in a background group of grove's terminal would stop itself.
// posix_spawn, not fork: on macOS a child forked while another grove thread first sets up
// libnotify dies in fork's own child handler, before it execs.
pub(crate) fn spawn_shell_in_own_session(
    shell: &Path,
    args: &[&str],
    env: &Env,
    folder: Option<&Path>,
    stdout: &PipeWriter,
    stderr: Option<&PipeWriter>,
) -> io::Result<libc::pid_t> {
    let program = CString::new(locate(shell, env, folder)?.as_os_str().as_bytes())?;
    let args = [Ok(program.clone())]
        .into_iter()
        .chain(args.iter().map(|arg| CString::new(*arg)))
        .collect::<Result<Vec<_>, _>>()?;
    let folder = folder
        .map(|folder| CString::new(folder.as_os_str().as_bytes()))
        .transpose()?;
    let vars = env
        .iter()
        .map(|(key, value)| CString::new([key.as_bytes(), b"=", value.as_bytes()].concat()))
        .collect::<Result<Vec<_>, _>>()?;
    let argv: Vec<_> = args
        .iter()
        .map(|arg| arg.as_ptr().cast_mut())
        .chain([ptr::null_mut()])
        .collect();
    let envp: Vec<_> = vars
        .iter()
        .map(|var| var.as_ptr().cast_mut())
        .chain([ptr::null_mut()])
        .collect();
    let check = |code: libc::c_int| match code {
        0 => Ok(()),
        code => Err(io::Error::from_raw_os_error(code)),
    };
    let mut actions = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
    let mut attrs = MaybeUninit::<libc::posix_spawnattr_t>::uninit();
    // Each is destroyed below once its init succeeded.
    unsafe {
        check(libc::posix_spawn_file_actions_init(actions.as_mut_ptr()))?;
        if let Err(e) = check(libc::posix_spawnattr_init(attrs.as_mut_ptr())) {
            libc::posix_spawn_file_actions_destroy(actions.as_mut_ptr());
            return Err(e);
        }
        let spawned = (|| -> io::Result<libc::pid_t> {
            let (actions, attrs) = (actions.as_mut_ptr(), attrs.as_mut_ptr());
            let null = c"/dev/null".as_ptr();
            check(libc::posix_spawn_file_actions_addopen(
                actions,
                0,
                null,
                libc::O_RDONLY,
                0,
            ))?;
            check(libc::posix_spawn_file_actions_adddup2(
                actions,
                stdout.as_raw_fd(),
                1,
            ))?;
            check(match stderr {
                Some(stderr) => {
                    libc::posix_spawn_file_actions_adddup2(actions, stderr.as_raw_fd(), 2)
                }
                None => libc::posix_spawn_file_actions_addopen(actions, 2, null, libc::O_WRONLY, 0),
            })?;
            if let Some(folder) = &folder {
                check(libc::posix_spawn_file_actions_addchdir_np(
                    actions,
                    folder.as_ptr(),
                ))?;
            }
            // Rust ignores SIGPIPE, and a spawned process keeps an ignored signal.
            let mut signals = MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigemptyset(signals.as_mut_ptr());
            check(libc::posix_spawnattr_setsigmask(attrs, signals.as_ptr()))?;
            libc::sigaddset(signals.as_mut_ptr(), libc::SIGPIPE);
            check(libc::posix_spawnattr_setsigdefault(attrs, signals.as_ptr()))?;
            let flags = POSIX_SPAWN_SETSID as libc::c_short
                | libc::POSIX_SPAWN_SETSIGMASK as libc::c_short
                | libc::POSIX_SPAWN_SETSIGDEF as libc::c_short;
            // Without it a descriptor grove opened without close-on-exec would leak in.
            #[cfg(target_os = "macos")]
            let flags = flags | libc::POSIX_SPAWN_CLOEXEC_DEFAULT as libc::c_short;
            check(libc::posix_spawnattr_setflags(attrs, flags))?;
            let mut pid = 0;
            check(libc::posix_spawn(
                &mut pid,
                program.as_ptr(),
                actions,
                attrs,
                argv.as_ptr(),
                envp.as_ptr(),
            ))?;
            Ok(pid)
        })();
        libc::posix_spawnattr_destroy(attrs.as_mut_ptr());
        libc::posix_spawn_file_actions_destroy(actions.as_mut_ptr());
        spawned
    }
}

// Waits for `pid`, a process grove started, to exit.
pub(crate) fn wait_for_exit(pid: libc::pid_t) -> io::Result<ExitStatus> {
    let mut status = 0;
    while unsafe { libc::waitpid(pid, &mut status, 0) } == -1 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(ExitStatus::from_raw(status))
}

// posix_spawn does not search PATH, so a bare name such as `zsh` is looked up in the PATH the
// shell starts with. Not there is an error: posix_spawn would run it from grove's own folder.
// A relative path, or one found through a relative PATH entry, counts from `folder`, where the
// shell starts, as exec after a chdir would. Joining it here keeps posix_spawn from resolving it
// after its own chdir, which older macOS gets wrong.
fn locate(shell: &Path, env: &Env, folder: Option<&Path>) -> io::Result<PathBuf> {
    let from_folder = |path: PathBuf| match folder {
        Some(folder) => folder.join(path),
        None => path,
    };
    if shell.as_os_str().as_bytes().contains(&b'/') {
        return Ok(from_folder(shell.into()));
    }
    env.get(OsStr::new("PATH"))
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| from_folder(dir.join(shell)))
        .find(|path| {
            fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.mode() & 0o111 != 0)
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not found on PATH"))
}

// Reads until the environment has been printed, or end of file.
fn read_env(mut stdout: PipeReader) -> Option<Env> {
    let start = format!("\n{START}\n");
    let mut out = Vec::new();
    let mut buf = [0; 16 * 1024];
    loop {
        if let Some(env) = parse(&out) {
            return Some(env);
        }
        match stdout.read(&mut buf) {
            Ok(0) => return None,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                // Drops profile output before the start marker, keeping just enough to find a
                // marker split across two reads.
                let keep = find(&out, start.as_bytes())
                    .unwrap_or_else(|| out.len().saturating_sub(start.len() - 1));
                out.drain(..keep);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

// `env -0` ends each `KEY=value` with a NUL, so values can hold newlines.
fn parse(out: &[u8]) -> Option<Env> {
    let start = format!("\n{START}\n");
    let body = &out[find(out, start.as_bytes())? + start.len()..];
    let body = &body[..find(body, format!("\n{END}\n").as_bytes())?];
    Some(
        body.split(|&b| b == 0)
            .filter_map(|entry| {
                let eq = entry.iter().position(|&b| b == b'=')?;
                let key = OsStr::from_bytes(&entry[..eq]);
                let value = OsStr::from_bytes(&entry[eq + 1..]);
                Some((key.to_owned(), value.to_owned()))
            })
            .collect(),
    )
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub(crate) fn kill_group(child: &Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
}

// The shell may still be running its logout files, so it is waited for off this thread.
fn reap(pid: libc::pid_t) {
    let _ = thread::Builder::new()
        .name("grove-login-env-wait".into())
        .spawn(move || {
            let _ = wait_for_exit(pid);
        });
}
