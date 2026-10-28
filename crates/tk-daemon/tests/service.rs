//! The daemon as a process: the binary, its files, its lock, a restart.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_tk-daemon");

struct Dirs {
    root: PathBuf,
}

impl Dirs {
    fn new() -> Dirs {
        let root = std::env::temp_dir().join(format!(
            "tkd-svc-{}-{}",
            std::process::id(),
            &tk_core::token::mint().unwrap()[..12]
        ));
        std::fs::create_dir_all(&root).unwrap();
        Dirs { root }
    }
    fn run(&self) -> PathBuf {
        self.root.join("run/tk")
    }
    fn socket(&self) -> PathBuf {
        self.run().join("daemon.sock")
    }
    fn data(&self) -> PathBuf {
        self.root.join("data")
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn start(d: &Dirs) -> Child {
    let child = Command::new(BIN)
        .args(["--foreground", "--socket"])
        .arg(d.socket())
        .arg("--data-dir")
        .arg(d.data())
        .env_remove("TK_LOG_CONTENT")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(d.socket()).is_err() {
        assert!(Instant::now() < deadline, "the daemon did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
}

fn session(d: &Dirs) -> (UnixStream, BufReader<UnixStream>) {
    let token = std::fs::read_to_string(d.run().join("daemon.token")).unwrap();
    let s = UnixStream::connect(d.socket()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    writeln!(
        w,
        "{}",
        json!({"op": "hello", "token": token.trim(), "client": "test"})
    )
    .unwrap();
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["scope"], "daemon", "{reply}");
    (w, r)
}

fn ask(w: &mut UnixStream, r: &mut BufReader<UnixStream>, v: Value) -> Value {
    writeln!(w, "{v}").unwrap();
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn files_are_private_and_a_second_daemon_is_refused() {
    let d = Dirs::new();
    let child = start(&d);
    assert_eq!(mode(&d.run()), 0o700);
    assert_eq!(mode(&d.data()), 0o700);
    for name in ["daemon.sock", "daemon.pid", "daemon.token", "otlp.endpoint"] {
        assert_eq!(mode(&d.run().join(name)), 0o600, "{name}");
    }
    let token = std::fs::read_to_string(d.run().join("daemon.token")).unwrap();
    assert!(tk_core::token::well_formed(token.trim()));
    let endpoint = std::fs::read_to_string(d.run().join("otlp.endpoint")).unwrap();
    assert!(
        tk_daemon::otel_receiver::loopback_endpoint(endpoint.trim()),
        "{endpoint}"
    );
    let pid = std::fs::read_to_string(d.run().join("daemon.pid")).unwrap();
    assert_eq!(pid.trim(), child.id().to_string());
    // A second daemon on the same folder stops before touching a file.
    let out = Command::new(BIN)
        .args(["--foreground", "--socket"])
        .arg(d.socket())
        .arg("--data-dir")
        .arg(d.data())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("already runs"));
    assert_eq!(
        std::fs::read_to_string(d.run().join("daemon.token")).unwrap(),
        token,
        "the running daemon's token is untouched"
    );
    stop(child);
}

#[test]
fn bad_arguments_exit_2() {
    for args in [
        vec![],
        vec!["--background"],
        vec!["--foreground", "--socket"],
        vec!["--foreground", "--socket", "relative/daemon.sock"],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn a_restart_keeps_the_log_chained_and_rotates_the_token() {
    let d = Dirs::new();
    let child = start(&d);
    let first_token = std::fs::read_to_string(d.run().join("daemon.token")).unwrap();
    let (mut w, mut r) = session(&d);
    for id in ["e1", "e2"] {
        let reply = ask(
            &mut w,
            &mut r,
            json!({"op": "event", "event": {
                "id": id, "at": 0, "chatId": "c", "harness": "pi", "source": "adapter",
                "kind": "tool.call", "summary": "", "detail": {}
            }}),
        );
        assert_eq!(reply["ok"], true);
    }
    // Let the writer thread reach the disk before the kill.
    std::thread::sleep(Duration::from_millis(300));
    stop(child);
    // A killed daemon leaves its socket and pid file; the next one replaces
    // them.
    assert!(d.socket().exists());
    let child = start(&d);
    let second_token = std::fs::read_to_string(d.run().join("daemon.token")).unwrap();
    assert_ne!(first_token, second_token, "a new token each start");
    let (mut w, mut r) = session(&d);
    ask(
        &mut w,
        &mut r,
        json!({"op": "event", "event": {
            "id": "e3", "at": 0, "chatId": "c", "harness": "pi", "source": "adapter",
            "kind": "tool.call", "summary": "", "detail": {}
        }}),
    );
    let mut report = Value::Null;
    for _ in 0..50 {
        report = ask(
            &mut w,
            &mut r,
            json!({"op": "report", "since": 0, "until": i64::MAX}),
        )["report"]
            .clone();
        if report["events"] == 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(report["events"], 3, "{report}");
    assert_eq!(
        report["chainOk"], true,
        "the chain continues across the restart"
    );
    stop(child);
}

fn hello_with(d: &Dirs, token: &str) -> Value {
    let s = UnixStream::connect(d.socket()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut w = s.try_clone().unwrap();
    let mut r = BufReader::new(s);
    ask(
        &mut w,
        &mut r,
        json!({"op": "hello", "token": token, "client": "test"}),
    )
}

/// Box finding BX8 (integration w2): a daemon restart used to kill every
/// chat token and move the OTLP port, so a running chat's feed ended. Chats
/// whose harness still runs keep their tokens and, when it is free, the
/// port; a chat whose harness has gone does not come back.
#[test]
fn a_restart_keeps_a_running_chat_and_its_otlp_port() {
    let d = Dirs::new();
    let daemon = start(&d);
    let endpoint = std::fs::read_to_string(d.run().join("otlp.endpoint")).unwrap();
    let (mut w, mut r) = session(&d);
    let mut harness = Command::new("sleep").arg("60").spawn().unwrap();
    let mut gone = Command::new("sleep").arg("60").spawn().unwrap();
    let mut chat = |id: &str, pid: u32| -> String {
        let t = ask(
            &mut w,
            &mut r,
            json!({"op": "token.mint", "chat": id, "harness": "pi"}),
        );
        let token = t["token"].as_str().unwrap().to_string();
        let started = ask(
            &mut w,
            &mut r,
            json!({"op": "event", "event": {
                "id": format!("start-{id}"), "at": 0, "chatId": id, "harness": "pi",
                "source": "run", "kind": "run.start", "summary": "", "detail": {"cwd": "/tmp"},
                "pid": pid
            }}),
        );
        assert_eq!(started["ok"], true, "{started}");
        token
    };
    let running = chat("chat-running", harness.id());
    let ended = chat("chat-gone", gone.id());
    drop((w, r));
    gone.kill().unwrap();
    gone.wait().unwrap();
    stop(daemon);
    let daemon = start(&d);
    let again = std::fs::read_to_string(d.run().join("otlp.endpoint")).unwrap();
    assert_eq!(again, endpoint, "the OTLP port came back");
    let back = hello_with(&d, &running);
    assert_eq!(back["scope"], "chat", "{back}");
    assert_eq!(back["chat"], "chat-running");
    assert_eq!(hello_with(&d, &ended)["error"], "bad token");
    // Its run.stop still revokes it.
    let (mut w, mut r) = session(&d);
    let stopped = ask(
        &mut w,
        &mut r,
        json!({"op": "event", "event": {
            "id": "stop-running", "at": 0, "chatId": "chat-running", "harness": "pi",
            "source": "run", "kind": "run.stop", "summary": "", "detail": {}, "pid": harness.id()
        }}),
    );
    assert_eq!(stopped["ok"], true, "{stopped}");
    assert_eq!(hello_with(&d, &running)["error"], "bad token");
    let state = d.run().join("chats.json");
    assert_eq!(mode(&state), 0o600);
    let text = std::fs::read_to_string(&state).unwrap();
    assert!(
        !text.contains(&running) && !text.contains(&ended),
        "no token in the file"
    );
    let _ = harness.kill();
    let _ = harness.wait();
    stop(daemon);
}
