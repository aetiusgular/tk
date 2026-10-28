//! `tk watch` and `tk status` against lane B's real daemon, started in this
//! test process on a temporary socket, as `crates/tk-daemon/tests/socket.rs`
//! starts it. Nothing here touches your own daemon or files.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tk_daemon::activity::store::ActivityLog;
use tk_daemon::daemon::Daemon;

use super::approve::{self, In};
use super::conn::Conn;
use super::{report, status, tail};

const TOKEN: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

struct Live {
    root: PathBuf,
    socket: PathBuf,
    _daemon: Arc<Daemon>,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn live(name: &str) -> Live {
    let root = std::env::temp_dir().join(format!(
        "tkj-{name}-{}-{}",
        std::process::id(),
        &tk_core::token::mint().unwrap()[..8]
    ));
    std::fs::create_dir_all(&root).unwrap();
    let data = root.join("data");
    let log: &'static ActivityLog = Box::leak(Box::new(ActivityLog::start(data.join("activity"))));
    let daemon = Daemon::new(TOKEN, log, &data, false).unwrap();
    let socket = root.join("run/tk/daemon.sock");
    let listener = tk_daemon::socket::bind(&socket).unwrap();
    let tok = socket.parent().unwrap().join("daemon.token");
    std::fs::write(&tok, format!("{TOKEN}\n")).unwrap();
    std::fs::set_permissions(&tok, std::fs::Permissions::from_mode(0o600)).unwrap();
    let serving = daemon.clone();
    std::thread::spawn(move || tk_daemon::socket::serve(listener, serving));
    Live {
        root,
        socket,
        _daemon: daemon,
    }
}

fn post(l: &Live, ev: Value) -> Value {
    let mut c = Conn::open(&l.socket, "test").unwrap();
    c.request(&json!({"op": "event", "event": ev})).unwrap()
}

fn ev(id: &str, chat: &str, source: &str, kind: &str, summary: &str) -> Value {
    json!({"id": id, "at": 0, "chatId": chat, "harness": "pi", "source": source,
        "kind": kind, "summary": summary, "detail": {}})
}

/// A `Write` the test can read while `tail::run` still holds it.
#[derive(Clone, Default)]
struct Shared(Arc<std::sync::Mutex<Vec<u8>>>);
impl Write for Shared {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
    fn wait_for(&self, needle: &str) -> String {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            let s = self.text();
            if s.contains(needle) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{needle:?} never appeared in:\n{}", self.text());
    }
}

#[test]
fn tail_shows_the_backlog_then_live_events_and_cleans_them() {
    let l = live("tail");
    post(&l, ev("pi-old", "tk-chat-a", "adapter", "tool.call", "old call"));
    let out = Shared::default();
    let mut w = out.clone();
    let conn = Conn::open(&l.socket, "test tail").unwrap();
    std::thread::spawn(move || {
        let _ = tail::run(conn, Some(0), None, false, &mut w);
    });
    out.wait_for("old call");
    post(
        &l,
        ev("pi-new", "tk-chat-a", "adapter", "tool.call", "new call\x1b]52;c;eA==\x07"),
    );
    let text = out.wait_for("new call");
    assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text:?}");
    assert!(text.contains("tk-chat-a"), "{text}");
}

#[test]
fn tail_filters_by_chat_and_json_mode_prints_events() {
    let l = live("tailchat");
    let out = Shared::default();
    let mut w = out.clone();
    let conn = Conn::open(&l.socket, "test tail").unwrap();
    std::thread::spawn(move || {
        let _ = tail::run(conn, None, Some("tk-chat-b"), true, &mut w);
    });
    // The tail registers before its reply; give it that moment.
    std::thread::sleep(Duration::from_millis(200));
    post(&l, ev("x-1", "tk-chat-a", "adapter", "tool.call", "not mine"));
    post(&l, ev("x-2", "tk-chat-b", "adapter", "tool.call", "mine"));
    let text = out.wait_for("mine");
    assert!(!text.contains("not mine"), "{text}");
    let line = text.lines().find(|l| l.contains("\"mine\"")).unwrap();
    let v: Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["chatId"], "tk-chat-b");
}

#[test]
fn report_counts_what_was_posted() {
    let l = live("report");
    post(&l, ev("r-1", "tk-chat-a", "adapter", "tool.call", "a"));
    post(&l, ev("r-2", "tk-chat-a", "otel", "tool.call", "b"));
    // The log writer is asynchronous; give it a moment to land on disk.
    std::thread::sleep(Duration::from_millis(500));
    let now = super::text::now_ms();
    let r = report::fetch(&l.socket, None, now - 60_000, now + 1_000).unwrap();
    assert!(r["events"].as_u64().unwrap() >= 2, "{r}");
    let text = report::render(&r);
    assert!(text.contains("by source:") && text.contains("adapter"), "{text}");
    assert!(text.contains("chain ok"), "{text}");
}

#[test]
fn watch_never_takes_a_chat_token() {
    let l = live("scope");
    let mut c = Conn::open(&l.socket, "test").unwrap();
    let chat = c
        .request(&json!({"op": "token.mint", "chat": "tk-chat-z", "harness": "pi"}))
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let e = Conn::open_with(&l.socket, &chat, "test").err().expect("refused");
    assert!(e.contains("daemon scope"), "{e}");
}

#[test]
fn a_token_file_others_can_read_is_refused() {
    let l = live("mode");
    let tok = l.socket.parent().unwrap().join("daemon.token");
    std::fs::set_permissions(&tok, std::fs::Permissions::from_mode(0o644)).unwrap();
    let e = Conn::open(&l.socket, "test").err().expect("refused");
    assert!(e.contains("readable by others"), "{e}");
}

#[test]
fn no_daemon_is_a_clear_error() {
    let dir = std::env::temp_dir().join(format!("tkj-none-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(dir.join("daemon.token"), format!("{TOKEN}\n")).unwrap();
    std::fs::set_permissions(dir.join("daemon.token"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let e = Conn::open(&dir.join("daemon.sock"), "test").err().expect("error");
    assert!(e.contains("not running"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Asks the daemon on its own connection, as a hook does, and returns the
/// reply.
fn ask(l: &Live, id: &str, secs: u32) -> std::thread::JoinHandle<Value> {
    let socket = l.socket.clone();
    let id = id.to_string();
    std::thread::spawn(move || {
        let mut c = Conn::open(&socket, "test hook").unwrap();
        c.quiet_timeout(Duration::from_secs(u64::from(secs) + 5)).unwrap();
        c.request(&json!({"op": "approve", "request": {"id": id, "chat": "tk-chat-a",
            "harness": "claude-code", "tool": "bash", "summary": "rm -rf build",
            "timeoutSeconds": secs}}))
            .unwrap()
    })
}

#[test]
fn the_approver_answers_a_hooks_request_end_to_end() {
    let l = live("approve");
    let (tx, rx) = mpsc::channel::<In>();
    let conn = Conn::open(&l.socket, "test approver").unwrap();
    let writer = approve::register(conn, tx.clone()).unwrap();
    let out = Shared::default();
    let mut o = out.clone();
    let h = std::thread::spawn(move || {
        let mut flush = || {};
        approve::serve(&rx, writer, &mut o, &mut flush)
    });
    let asked = ask(&l, "ap-yes", 20);
    out.wait_for("Allow this one call? [y/N]");
    std::thread::sleep(approve::SETTLE + Duration::from_millis(50));
    tx.send(In::Typed("yes\n".into(), Instant::now())).unwrap();
    let reply = asked.join().unwrap();
    assert_eq!(reply["answer"], "yes", "{reply}");
    assert_eq!(reply["by"], "approver", "{reply}");
    out.wait_for("yes sent for ap-yes");

    // A second request, denied with Enter.
    let asked = ask(&l, "ap-no", 20);
    out.wait_for("ap-no");
    let shown = out.text().matches("Allow this one call?").count();
    assert_eq!(shown, 2);
    std::thread::sleep(approve::SETTLE + Duration::from_millis(50));
    tx.send(In::Typed("\n".into(), Instant::now())).unwrap();
    let reply = asked.join().unwrap();
    assert_eq!((reply["answer"].as_str(), reply["by"].as_str()), (Some("no"), Some("approver")));

    tx.send(In::TtyGone).unwrap();
    assert!(matches!(h.join().unwrap(), approve::Ended::TtyGone));
    // Both outcomes are in the daemon's own log.
    std::thread::sleep(Duration::from_millis(500));
    let now = super::text::now_ms();
    let r = report::fetch(&l.socket, None, now - 60_000, now + 1_000).unwrap();
    assert_eq!(r["approvals"]["yes:approver"], 1, "{r}");
    assert_eq!(r["approvals"]["no:approver"], 1, "{r}");
}

#[test]
fn with_no_approver_the_daemon_says_no_at_once() {
    let l = live("noapprover");
    let t = Instant::now();
    let reply = ask(&l, "ap-1", 30).join().unwrap();
    assert_eq!((reply["answer"].as_str(), reply["by"].as_str()), (Some("no"), Some("none")));
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn an_unanswered_request_times_out_to_no_and_is_not_answered_late() {
    let l = live("timeout");
    let (tx, rx) = mpsc::channel::<In>();
    let conn = Conn::open(&l.socket, "test approver").unwrap();
    let writer = approve::register(conn, tx.clone()).unwrap();
    let out = Shared::default();
    let mut o = out.clone();
    std::thread::spawn(move || {
        let mut flush = || {};
        approve::serve(&rx, writer, &mut o, &mut flush)
    });
    let reply = ask(&l, "ap-slow", 2).join().unwrap();
    assert_eq!((reply["answer"].as_str(), reply["by"].as_str()), (Some("no"), Some("timeout")));
    out.wait_for("timed out");
    // A "y" now lands on nothing.
    tx.send(In::Typed("y\n".into(), Instant::now())).unwrap();
    out.wait_for("no request is waiting");
}

#[test]
fn status_sees_the_daemon_its_server_and_an_open_chat() {
    let l = live("status");
    // A child stands in for a harness `tk run` started.
    let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    post(
        &l,
        json!({"id": "run-tk-chat-s-1", "at": 0, "chatId": "tk-chat-s", "harness": "pi",
            "source": "run", "kind": "run.start", "summary": "pi under none",
            "detail": {"boundary": "none", "argv": ["pi"], "cwd": "/tmp", "folders": ["/tmp"],
                "dangerousFlags": []}, "pid": pid}),
    );
    post(
        &l,
        json!({"id": "run-tk-chat-t-1", "at": 0, "chatId": "tk-chat-t", "harness": "pi",
            "source": "run", "kind": "run.start", "summary": "done", "detail": {"boundary": "srt"}, "pid": 1}),
    );
    post(
        &l,
        json!({"id": "run-tk-chat-t-2", "at": 0, "chatId": "tk-chat-t", "harness": "pi",
            "source": "run", "kind": "run.stop", "summary": "done", "detail": {"exitCode": 0}, "pid": 1}),
    );
    let d = status::daemon_state(&l.socket);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(d["running"], true, "{d}");
    assert_eq!(d["servedBy"]["pid"], std::process::id(), "{d}");
    assert_eq!(d["servedBy"]["yours"], true);
    assert_eq!(d["servedBy"]["matchesHello"], true);
    let chats = d["chats"].as_array().unwrap();
    assert_eq!(chats.len(), 1, "{d}");
    assert_eq!(chats[0]["chat"], "tk-chat-s");
    assert_eq!(chats[0]["alive"], true);
    assert_eq!(chats[0]["boundary"], "none");
    let text = status::render(&[], &d);
    assert!(text.contains("daemon: running"), "{text}");
    assert!(text.contains("tk-chat-s") && !text.contains("tk-chat-t"), "{text}");
}
