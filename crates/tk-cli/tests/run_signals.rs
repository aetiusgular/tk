//! `tk run` and the signals a supervisor sends (box test w1, finding BX7):
//! a SIGINT sent to the `tk` process alone must reach the harness, or a
//! script that stops `tk` leaves the agent running. A terminal's Ctrl-C
//! reaches both already (same process group) and is not sent twice; that
//! half needs a terminal and is a box and Mac step.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn fake_pi(dir: &Path) {
    let p = dir.join("pi");
    std::fs::write(
        &p,
        "#!/bin/sh\ntrap 'echo int >> \"$OUT\"; exit 130' INT\ntrap 'echo term >> \"$OUT\"; exit 143' TERM\necho started >> \"$OUT\"\nwhile :; do sleep 0.05; done\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn wait_for(path: &Path, word: &str, limit: Duration) -> String {
    let t = Instant::now();
    loop {
        let s = std::fs::read_to_string(path).unwrap_or_default();
        if s.contains(word) || t.elapsed() > limit {
            return s;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn signal_tk_alone(sig: libc::c_int, name: &str) {
    let dir = std::env::temp_dir().join(format!("tk-sig-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    fake_pi(&dir);
    let out = dir.join("seen");
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
    let mut tk = Command::new(env!("CARGO_BIN_EXE_tk"))
        .args(["run", "pi", "--boundary", "none", "--no-daemon", "--cwd"])
        .arg(&dir)
        .env("PATH", path)
        .env("OUT", &out)
        // Its own process group, so nothing but this test's kill reaches it.
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_for(&out, "started", Duration::from_secs(10)).contains("started"),
        "the fake harness started"
    );
    // SAFETY: kill with tk's own pid, not its group.
    unsafe {
        libc::kill(tk.id() as libc::pid_t, sig);
    }
    let seen = wait_for(&out, if sig == libc::SIGINT { "int" } else { "term" }, Duration::from_secs(5));
    // Whatever happened, do not leave the fake running.
    let _ = Command::new("pkill").args(["-f", &dir.display().to_string()]).status();
    let status = tk.wait().unwrap();
    assert!(
        seen.contains(if sig == libc::SIGINT { "int" } else { "term" }),
        "the harness never got the signal sent to tk: {seen:?}, tk {status:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sigint_to_tk_alone_reaches_the_harness() {
    signal_tk_alone(libc::SIGINT, "int");
}

#[test]
fn sigterm_to_tk_alone_reaches_the_harness() {
    signal_tk_alone(libc::SIGTERM, "term");
}

/// The terminal half on Linux: Ctrl-C typed into a terminal reaches the
/// harness once, not twice (tk sees `SI_KERNEL` and does not forward). Runs
/// under util-linux `script`, which gives `tk` a pty; skipped without it.
#[test]
fn a_terminal_ctrl_c_reaches_the_harness_once() {
    use std::io::Write;
    if Command::new("script").arg("--version").output().is_err() {
        eprintln!("script not installed: the terminal half is a box step");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tk-sig-tty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A harness that survives the first SIGINT and counts each one.
    let p = dir.join("pi");
    std::fs::write(
        &p,
        "#!/bin/sh\ntrap 'echo int >> \"$OUT\"' INT\necho started >> \"$OUT\"\ni=0; while [ $i -lt 60 ]; do sleep 0.05; i=$((i+1)); done\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = dir.join("seen");
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
    let cmd = format!(
        "'{}' run pi --boundary none --no-daemon --cwd '{}'",
        env!("CARGO_BIN_EXE_tk"),
        dir.display()
    );
    let mut sc = Command::new("script")
        .args(["-qec", &cmd, "/dev/null"])
        .env("PATH", path)
        .env("OUT", &out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_for(&out, "started", Duration::from_secs(10)).contains("started"));
    sc.stdin.as_mut().unwrap().write_all(b"\x03").unwrap();
    let _ = wait_for(&out, "int", Duration::from_secs(3));
    std::thread::sleep(Duration::from_millis(500));
    let seen = std::fs::read_to_string(&out).unwrap_or_default();
    drop(sc.stdin.take());
    let _ = sc.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(seen.matches("int").count(), 1, "{seen:?}");
}
