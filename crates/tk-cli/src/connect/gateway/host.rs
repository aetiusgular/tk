//! The three helpers the copied `docker_mcp.rs` imported from the fork's
//! `lib.rs` and `harness.rs` (`contracts/docker-args.md`, section 4),
//! rewritten for a command-line program.
//!
//! - `dirs_home`: as the fork's (`lib.rs:139` at `159ef3e`), `HOME` first.
//! - `gui_search_path`: the fork prepends the login shell's PATH because a
//!   Finder-launched app inherits launchd's bare PATH (`harness.rs:3290`).
//!   A CLI already runs with the user's PATH, so this skips the login-shell
//!   read (which would start the user's shell and run their rc files) and
//!   keeps the fork's fixed fallback folders after the inherited PATH.
//! - `exec_output_with_stdin`: argv only, no shell; stdin is the only input
//!   and is never logged; the child gets its own process group, and past
//!   the timeout the whole group is sent SIGTERM, then SIGKILL after two
//!   seconds, as the fork's `run_isolated_output` does (`harness.rs:1895`).

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const KILL_ESCALATE: Duration = Duration::from_secs(2);

pub fn dirs_home() -> Option<String> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(key) {
            let home = home.to_string_lossy().into_owned();
            if !home.is_empty() {
                return Some(home);
            }
        }
    }
    None
}

pub fn gui_search_path() -> String {
    search_path_from(dirs_home(), std::env::var("PATH").ok())
}

fn search_path_from(home: Option<String>, existing: Option<String>) -> String {
    let mut parts: Vec<std::path::PathBuf> = Vec::new();
    if let Some(existing) = existing {
        parts.extend(std::env::split_paths(&existing));
    }
    if let Some(home) = home {
        parts.push(format!("{home}/.local/bin").into());
        parts.push(format!("{home}/.docker/bin").into());
    }
    for fixed in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/snap/bin",
    ] {
        parts.push(fixed.into());
    }
    std::env::join_paths(parts)
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn terminate(pid: u32) {
    let pgid = pid as libc::pid_t;
    // SAFETY: kill(2) with a negative pid signals the process group the
    // child leads (it was started with process_group(0)). No memory is
    // shared; a stale group id fails with ESRCH, which is ignored.
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    thread::spawn(move || {
        thread::sleep(KILL_ESCALATE);
        // SAFETY: as above.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    });
}

pub fn exec_output_with_stdin(
    command: &str,
    args: &[String],
    cwd: Option<&str>,
    timeout: Duration,
    input: Option<&[u8]>,
) -> Result<Output, String> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run {command}: {e}"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = stdin.write_all(input);
    }
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(format!("Failed to run {command}: {e}")),
        Err(_) => {
            terminate(pid);
            Err(format!("{command} timed out"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_path_keeps_the_inherited_path_first() {
        let p = search_path_from(Some("/h".into()), Some("/a:/b".into()));
        assert!(p.starts_with("/a:/b:/h/.local/bin"), "{p}");
        assert!(p.ends_with("/snap/bin"));
    }

    #[test]
    fn runs_with_stdin_and_captures_output() {
        let out = exec_output_with_stdin("cat", &[], None, Duration::from_secs(5), Some(b"secret"))
            .unwrap();
        assert_eq!(out.stdout, b"secret");
        assert!(out.status.success());
    }

    #[test]
    fn times_out_and_stops_the_group() {
        let t = std::time::Instant::now();
        let e = exec_output_with_stdin(
            "sleep",
            &["30".into()],
            None,
            Duration::from_millis(200),
            None,
        )
        .unwrap_err();
        assert!(e.contains("timed out"));
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn missing_binary_is_an_error_not_a_panic() {
        assert!(exec_output_with_stdin(
            "/nonexistent/tk-x",
            &[],
            None,
            Duration::from_secs(1),
            None
        )
        .is_err());
    }
}
