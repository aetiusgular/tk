//! Starting the child and waiting for it, with the terminal's signals left
//! to the child and `SIGTERM`/`SIGHUP` passed on, so `tk run` lives long
//! enough to record `run.stop`.
//!
//! Handlers, not `SIG_IGN`: an ignored signal stays ignored across `exec`
//! (POSIX `execve`: "Signals set to be ignored ... shall be set to be
//! ignored by the new process image"), which would leave the harness deaf to
//! Ctrl-C; a caught signal is reset to its default in the new image.

use std::process::{Child, Command, ExitStatus, Stdio};

use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use super::plan::Plan;

static PENDING: AtomicI32 = AtomicI32::new(0);
/// For the last SIGINT: 1 when a process sent it to `tk` (kill(2)), 0 when
/// the kernel did (a terminal's Ctrl-C), -1 when the platform cannot say.
static INT_FROM_PROCESS: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    // Only atomic stores and reads of the siginfo the kernel passed:
    // async-signal-safe.
    if sig == libc::SIGINT {
        INT_FROM_PROCESS.store(from_process(info), Ordering::SeqCst);
    }
    PENDING.store(sig, Ordering::SeqCst);
}

/// Linux: a terminal's signal comes from `kill_pgrp(.., priv = 1)`
/// (`drivers/tty/n_tty.c` `__isig`), which `kernel/signal.c` delivers with
/// `si_code = SI_KERNEL` and `si_pid = 0`; kill(2) gives `SI_USER` and the
/// sender's pid (both read at torvalds/linux master, 27 Oct 2026).
#[cfg(target_os = "linux")]
fn from_process(info: *const libc::siginfo_t) -> i32 {
    if info.is_null() {
        return -1;
    }
    // SAFETY: the kernel passed a valid siginfo for this SA_SIGINFO handler.
    let (code, pid) = unsafe { ((*info).si_code, (*info).si_pid()) };
    i32::from(code != libc::SI_KERNEL && pid != 0)
}

/// macOS: kill(2) and a terminal's signal both reach `psignal` in XNU
/// (`bsd/kern/kern_sig.c`: `kill` and `tty_pgsignal`), so the siginfo is not
/// read as proof either way (not found); `wait` falls back to the terminal's
/// foreground group.
#[cfg(not(target_os = "linux"))]
fn from_process(_info: *const libc::siginfo_t) -> i32 {
    -1
}

/// Catches SIGINT, SIGTERM and SIGHUP in this process.
pub fn install_handlers() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: a zeroed sigaction with an SA_SIGINFO handler that only
        // stores to atomics is a valid argument; the previous action is not
        // needed.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = on_signal as *const () as usize;
            libc::sigemptyset(&mut act.sa_mask);
            act.sa_flags = libc::SA_RESTART | libc::SA_SIGINFO;
            libc::sigaction(sig, &act, std::ptr::null_mut());
        }
    }
}

/// Whether `tk` runs in its terminal's foreground process group. No terminal
/// on stdin, or a background group, means no Ctrl-C from a terminal can have
/// reached the child.
fn in_terminal_foreground() -> bool {
    // SAFETY: plain calls on fd 0 and this process.
    unsafe {
        let fg = libc::tcgetpgrp(libc::STDIN_FILENO);
        fg != -1 && fg == libc::getpgrp()
    }
}

/// Box test w1, finding BX7: a SIGINT a process sends to `tk` alone never
/// reached the harness. Forward it when a process sent it; when the platform
/// cannot say, forward only when no terminal can have sent it to the whole
/// group (a terminal Ctrl-C reaches the child already, and twice would read
/// as a double Ctrl-C).
pub fn forward_sigint(from_process: i32, foreground: bool) -> bool {
    match from_process {
        1 => true,
        0 => false,
        _ => !foreground,
    }
}

pub fn spawn(plan: &Plan, cwd: &str) -> std::io::Result<Child> {
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args).current_dir(cwd);
    for name in &plan.env_remove {
        cmd.env_remove(name);
    }
    for (k, v) in &plan.env_set {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd.spawn()
}

/// Waits for the child. SIGTERM and SIGHUP sent to `tk run` go on to the
/// child; SIGINT goes on when a process sent it (`forward_sigint`), and not
/// when the terminal did, which already reached the child (same process
/// group).
pub fn wait(child: &mut Child) -> std::io::Result<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let sig = PENDING.swap(0, Ordering::SeqCst);
        let forward = sig == libc::SIGTERM
            || sig == libc::SIGHUP
            || (sig == libc::SIGINT
                && forward_sigint(
                    INT_FROM_PROCESS.load(Ordering::SeqCst),
                    in_terminal_foreground(),
                ));
        if forward {
            // SAFETY: kill with the child's pid and a valid signal number.
            unsafe {
                libc::kill(child.id() as libc::pid_t, sig);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The exit code `tk run` returns: the child's, or 128 + the signal.
pub fn code(status: &ExitStatus) -> (u8, Option<i32>) {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(c), _) => (u8::try_from(c).unwrap_or(1), None),
        (None, Some(s)) => (u8::try_from(128 + s).unwrap_or(1), Some(s)),
        _ => (1, None),
    }
}

/// Runs a short command to completion with a deadline, returning its
/// stdout. For `sbx policy log`; never a shell, an argv list.
pub fn output_with_timeout(program: &str, args: &[String], limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = (&mut out).take(4 * 1024 * 1024).read_to_end(&mut buf);
        buf
    });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let buf = reader.join().ok()?;
                return status
                    .success()
                    .then(|| String::from_utf8_lossy(&buf).into_owned());
            }
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(program: &str, args: &[&str]) -> Plan {
        Plan {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env_set: vec![("TK_CHAT_ID".into(), "c1".into())],
            env_remove: vec!["TK_CHAT_TOKEN".into()],
            kit: None,
            sandbox_name: None,
            srt_settings: None,
            folders: vec![],
            state_dirs: vec![],
            notes: vec![],
        }
    }

    #[test]
    fn sigint_goes_on_only_when_no_terminal_sent_it() {
        assert!(forward_sigint(1, true), "kill(2) to tk, Linux");
        assert!(!forward_sigint(0, true), "the terminal's Ctrl-C, Linux");
        assert!(!forward_sigint(0, false));
        assert!(forward_sigint(-1, false), "macOS, no terminal: a process");
        assert!(
            !forward_sigint(-1, true),
            "macOS in the foreground: unknown"
        );
    }

    #[test]
    fn exit_codes_and_signals_pass_through() {
        let mut c = spawn(&plan("sh", &["-c", "exit 7"]), "/").unwrap();
        assert_eq!(code(&wait(&mut c).unwrap()), (7, None));
        let mut c = spawn(&plan("sh", &["-c", "kill -9 $$"]), "/").unwrap();
        assert_eq!(code(&wait(&mut c).unwrap()), (137, Some(9)));
    }

    #[test]
    fn the_environment_is_set_and_inherited_tokens_cleared() {
        // A token left by an outer run must not reach the child.
        std::env::set_var("TK_CHAT_TOKEN", "left-over");
        let mut c = spawn(
            &plan(
                "sh",
                &[
                    "-c",
                    "test \"$TK_CHAT_ID\" = c1 && test -z \"$TK_CHAT_TOKEN\"",
                ],
            ),
            "/",
        )
        .unwrap();
        assert_eq!(code(&wait(&mut c).unwrap()).0, 0);
        std::env::remove_var("TK_CHAT_TOKEN");
    }

    #[test]
    fn arguments_reach_the_child_literally() {
        let mut c = spawn(
            &plan(
                "sh",
                &[
                    "-c",
                    "test \"$1\" = 'a b; $(x)' && test $# = 1",
                    "sh",
                    "a b; $(x)",
                ],
            ),
            "/",
        )
        .unwrap();
        assert_eq!(code(&wait(&mut c).unwrap()).0, 0);
    }

    #[test]
    fn a_slow_command_is_killed_at_its_deadline() {
        let t = std::time::Instant::now();
        assert!(output_with_timeout("sleep", &["10".into()], Duration::from_millis(200)).is_none());
        assert!(t.elapsed() < Duration::from_secs(3));
        assert!(output_with_timeout("/nonexistent/sbx", &[], Duration::from_secs(1)).is_none());
    }
}
