//! The daemon over its socket, as clients use it (`contracts/daemon-protocol.md`).
//! Each test runs its own daemon on a temporary socket and log folder.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tk_daemon::activity::store::ActivityLog;
use tk_daemon::daemon::{Daemon, MAX_PENDING_LINES};

const DAEMON_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Fixture {
    root: PathBuf,
    socket: PathBuf,
    daemon: Arc<Daemon>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture() -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "tkd-{}-{}",
        std::process::id(),
        &tk_core::token::mint().unwrap()[..12]
    ));
    std::fs::create_dir_all(&root).unwrap();
    let data = root.join("data");
    let log: &'static ActivityLog = Box::leak(Box::new(ActivityLog::start(data.join("activity"))));
    let daemon = Daemon::new(DAEMON_TOKEN, log, &data, false).unwrap();
    let socket = root.join("run/tk/daemon.sock");
    let listener = tk_daemon::socket::bind(&socket).unwrap();
    let serving = daemon.clone();
    std::thread::spawn(move || tk_daemon::socket::serve(listener, serving));
    Fixture {
        root,
        socket,
        daemon,
    }
}

struct Client {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl Client {
    fn connect(f: &Fixture) -> Client {
        let s = UnixStream::connect(&f.socket).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Client {
            w: s.try_clone().unwrap(),
            r: BufReader::new(s),
        }
    }

    fn hello(f: &Fixture, token: &str) -> (Client, Value) {
        let mut c = Client::connect(f);
        let reply = c.ask(json!({"op": "hello", "token": token, "client": "test"}));
        (c, reply)
    }

    fn daemon(f: &Fixture) -> Client {
        let (c, reply) = Client::hello(f, DAEMON_TOKEN);
        assert_eq!(reply["scope"], "daemon", "{reply}");
        c
    }

    fn send(&mut self, v: Value) {
        writeln!(self.w, "{v}").unwrap();
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        self.r.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

    fn ask(&mut self, v: Value) -> Value {
        self.send(v);
        self.recv()
    }

    fn closed(&mut self) -> bool {
        let mut buf = [0u8; 1];
        matches!(self.r.read(&mut buf), Ok(0))
    }

    fn try_recv(&mut self, wait: Duration) -> Option<Value> {
        self.r.get_ref().set_read_timeout(Some(wait)).unwrap();
        let mut line = String::new();
        let got = self.r.read_line(&mut line);
        self.r
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        match got {
            Ok(n) if n > 0 => Some(serde_json::from_str(&line).unwrap()),
            _ => None,
        }
    }
}

fn mint(c: &mut Client, chat: &str, harness: &str) -> String {
    let r = c.ask(json!({"op": "token.mint", "chat": chat, "harness": harness}));
    assert_eq!(r["ok"], true, "{r}");
    r["token"].as_str().unwrap().to_string()
}

fn event(id: &str, chat: &str, source: &str, kind: &str, detail: Value) -> Value {
    json!({"op": "event", "event": {
        "id": id, "at": 0, "chatId": chat, "harness": "pi", "source": source,
        "kind": kind, "summary": id, "detail": detail
    }})
}

#[test]
fn a_first_line_that_is_not_hello_or_a_bad_token_closes() {
    let f = fixture();
    let mut c = Client::connect(&f);
    assert_eq!(c.ask(json!({"op": "tail"}))["error"], "hello first");
    assert!(c.closed());
    let mut c = Client::connect(&f);
    c.send(json!("not an object"));
    assert_eq!(c.recv()["error"], "hello first");
    assert!(c.closed());
    // Well formed, but no daemon or chat token.
    let (mut c, r) = Client::hello(&f, &"2".repeat(64));
    assert_eq!(r["error"], "bad token");
    assert!(c.closed());
    let (mut c, r) = Client::hello(&f, "short");
    assert_eq!(r["error"], "bad token");
    assert!(c.closed());
    let (mut c, r) = Client::hello(&f, &DAEMON_TOKEN.to_uppercase());
    assert_eq!(r["error"], "bad token", "tokens are lowercase hex");
    assert!(c.closed());
}

#[test]
fn the_daemon_token_gets_daemon_scope_and_a_minted_one_chat_scope() {
    let f = fixture();
    let (mut d, r) = Client::hello(&f, DAEMON_TOKEN);
    assert_eq!(r["ok"], true);
    assert_eq!(r["daemon"]["protocol"], 1);
    assert_eq!(r["daemon"]["pid"], std::process::id());
    assert_eq!(
        d.ask(json!({"op": "hello", "token": DAEMON_TOKEN, "client": "x"}))["error"],
        "hello once"
    );
    let token = mint(&mut d, "chat-b", "pi");
    assert!(tk_core::token::well_formed(&token));
    assert_ne!(mint(&mut d, "chat-b", "pi"), token, "every mint is fresh");
    let (_c, r) = Client::hello(&f, &token);
    assert_eq!(r, json!({"ok": true, "scope": "chat", "chat": "chat-b"}));
    // Mint refuses what it cannot name.
    let r = d.ask(json!({"op": "token.mint", "chat": "chat b", "harness": "pi"}));
    assert_eq!(r["ok"], false);
    let r = d.ask(json!({"op": "token.mint", "chat": "c", "harness": "Pi"}));
    assert_eq!(r["error"], "unknown harness");
}

#[test]
fn a_chat_token_cannot_mint_report_or_approve_as_the_daemon() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let token = mint(&mut d, "chat-b", "codex");
    let (mut c, _) = Client::hello(&f, &token);
    assert_eq!(
        c.ask(json!({"op": "token.mint", "chat": "x", "harness": "pi"}))["error"],
        "token.mint needs daemon scope"
    );
    assert_eq!(
        c.ask(json!({"op": "report", "since": 0, "until": 1}))["error"],
        "report needs daemon scope"
    );
    assert_eq!(
        c.ask(json!({"op": "approver"}))["error"],
        "approver needs daemon scope"
    );
    assert_eq!(
        c.ask(json!({"op": "answer", "id": "a", "answer": "yes"}))["error"],
        "not an approver"
    );
    // The connection stays open after each refusal.
    assert_eq!(
        c.ask(json!({"op": "tail"}))["error"],
        "tail needs daemon scope or a chat filter"
    );
}

#[test]
fn bad_lines_are_answered_and_a_long_line_closes() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let r = d.ask(json!({"op": "shutdown"}));
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().starts_with("bad line"));
    let r = d.ask(json!({"op": "approver", "force": true}));
    assert_eq!(r["ok"], false, "unknown fields are refused, not ignored");
    writeln!(d.w, "{{not json").unwrap();
    assert_eq!(d.recv()["ok"], false);
    // Still serving.
    assert_eq!(d.ask(json!({"op": "approver"}))["ok"], true);
    let mut d = Client::daemon(&f);
    let long = format!(
        "{{\"op\":\"tail\",\"chat\":\"{}\"}}\n",
        "x".repeat(1024 * 1024)
    );
    d.w.write_all(long.as_bytes()).unwrap();
    assert_eq!(d.recv()["error"], "line too long");
    assert!(d.closed());
}

#[test]
fn a_chat_posts_only_into_its_own_chat_and_only_telemetry() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let token = mint(&mut d, "chat-b", "pi");
    let (mut c, _) = Client::hello(&f, &token);
    let r = c.ask(json!({"op": "event", "event": {
        "id": "pi-7", "at": 42, "chatId": "someone-else", "harness": "claude-code",
        "source": "adapter", "kind": "tool.call", "summary": "exa/search ok",
        "detail": {"trust": "client"}
    }}));
    assert_eq!(r, json!({"ok": true, "id": "pi-7"}));
    for source in ["eslogger", "auditd", "cua", "run", "approval"] {
        let r = c.ask(event("x", "chat-b", source, "exec", json!({})));
        assert_eq!(r["ok"], false, "{source}");
    }
    let r = c.ask(event("", "chat-b", "adapter", "tool.call", json!({})));
    assert_eq!(r["error"], "event id is empty");
    // The daemon scope may post a kernel feed, never an approval record.
    assert_eq!(
        d.ask(event("a1", "chat-b", "auditd", "exec", json!({})))["ok"],
        true
    );
    assert_eq!(
        d.ask(event("a2", "chat-b", "approval", "approval", json!({})))["ok"],
        false
    );
    let (mut t, _) = Client::hello(&f, DAEMON_TOKEN);
    assert_eq!(
        t.ask(json!({"op": "tail", "since": 0, "chat": "chat-b"}))["ok"],
        true
    );
    let first = t.recv();
    let e = &first["event"];
    assert_eq!(e["id"], "pi-7");
    assert_eq!(e["chatId"], "chat-b", "overwritten from the token");
    assert_eq!(e["harness"], "pi", "overwritten from the token");
    assert_eq!(
        e["detail"]["harnessAt"], 42,
        "the client's time is kept apart"
    );
    assert!(
        e["at"].as_i64().unwrap() > 1_700_000_000_000,
        "stamped on receipt"
    );
}

#[test]
fn tail_streams_a_backlog_then_live_events_for_its_filter() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    d.ask(event("old-1", "chat-a", "adapter", "tool.call", json!({})));
    d.ask(event("old-2", "chat-b", "adapter", "tool.call", json!({})));
    let token = mint(&mut d, "chat-a", "pi");
    let (mut c, _) = Client::hello(&f, &token);
    assert_eq!(
        c.ask(json!({"op": "tail", "chat": "chat-b"}))["error"],
        "a chat token tails only its own chat"
    );
    assert_eq!(
        c.ask(json!({"op": "tail", "since": 0, "chat": "chat-a"}))["ok"],
        true
    );
    assert_eq!(c.recv()["event"]["id"], "old-1");
    d.ask(event("live-b", "chat-b", "adapter", "tool.call", json!({})));
    d.ask(event("live-a", "chat-a", "adapter", "tool.call", json!({})));
    assert_eq!(
        c.recv()["event"]["id"],
        "live-a",
        "chat-b's event never reaches chat-a"
    );
    // A daemon tail with no filter and no `since`: live only, every chat.
    let mut all = Client::daemon(&f);
    assert_eq!(all.ask(json!({"op": "tail"}))["ok"], true);
    assert!(
        all.try_recv(Duration::from_millis(200)).is_none(),
        "no backlog"
    );
    d.ask(event("live-c", "chat-c", "adapter", "tool.call", json!({})));
    assert_eq!(all.recv()["event"]["id"], "live-c");
}

#[test]
fn a_tail_backlog_holds_the_last_copy_per_id() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    d.ask(event(
        "call-1",
        "c1",
        "adapter",
        "tool.call",
        json!({"n": 1}),
    ));
    d.ask(event("call-1", "c1", "otel", "tool.call", json!({"n": 2})));
    // Another chat's record under the same id stays its own.
    d.ask(event(
        "call-1",
        "c2",
        "adapter",
        "tool.call",
        json!({"n": 3}),
    ));
    let mut t = Client::daemon(&f);
    t.ask(json!({"op": "tail", "since": 0}));
    let a = t.recv();
    let b = t.recv();
    assert!(t.try_recv(Duration::from_millis(200)).is_none());
    let mut got = vec![
        (
            a["event"]["chatId"].clone(),
            a["event"]["detail"]["n"].clone(),
        ),
        (
            b["event"]["chatId"].clone(),
            b["event"]["detail"]["n"].clone(),
        ),
    ];
    got.sort_by_key(|(c, _)| c.to_string());
    assert_eq!(got, vec![(json!("c1"), json!(2)), (json!("c2"), json!(3))]);
}

#[test]
fn a_chat_token_cannot_replace_a_gate_decision() {
    // Integration w1's security finding S9. The hook posts its decision
    // with the chat token; the agent holds the same token (case 66).
    let f = fixture();
    let mut d = Client::daemon(&f);
    let token = mint(&mut d, "c1", "claude-code");
    let (mut hook, _) = Client::hello(&f, &token);
    let r = hook.ask(event(
        "claude-code-t1-decision",
        "c1",
        "adapter",
        "approval",
        json!({"decision": "deny"}),
    ));
    assert_eq!(r["ok"], true, "{r}");
    let (mut agent, _) = Client::hello(&f, &token);
    for (source, kind) in [
        ("otel", "approval"),
        ("adapter", "approval"),
        ("otel", "tool.call"),
    ] {
        let r = agent.ask(event(
            "claude-code-t1-decision",
            "c1",
            source,
            kind,
            json!({"decision": "allow"}),
        ));
        assert_eq!(r["ok"], false, "{source} {kind}: {r}");
        assert_eq!(r["error"], "a gate decision is written once", "{r}");
    }
    let mut t = Client::daemon(&f);
    t.ask(json!({"op": "tail", "since": 0}));
    let only = t.recv();
    assert_eq!(only["event"]["kind"], "approval", "{only}");
    assert_eq!(only["event"]["detail"]["decision"], "deny", "{only}");
    assert!(t.try_recv(Duration::from_millis(200)).is_none());
}

#[test]
fn a_tail_that_falls_behind_is_dropped_and_ingest_never_waits() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let mut slow = Client::daemon(&f);
    assert_eq!(slow.ask(json!({"op": "tail"}))["ok"], true);
    // Never read `slow` again. Big events fill the socket buffer, then the
    // daemon's queue for it.
    let detail = json!({"pad": "x".repeat(8 * 1024)});
    let started = std::time::Instant::now();
    for i in 0..(MAX_PENDING_LINES + 400) {
        f.daemon.ingest(
            serde_json::from_value(json!({
                "id": format!("e{i}"), "at": 0, "chatId": "c", "harness": "pi",
                "source": "adapter", "kind": "tool.call", "summary": "", "detail": detail
            }))
            .unwrap(),
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "ingest did not wait on the slow tail"
    );
    assert!(
        f.daemon
            .counters
            .tails_dropped
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1
    );
    // The daemon still answers.
    assert_eq!(d.ask(json!({"op": "approver"}))["ok"], true);
}

#[test]
fn approve_with_no_approver_is_no_at_once() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let token = mint(&mut d, "chat-b", "pi");
    let (mut c, _) = Client::hello(&f, &token);
    let started = std::time::Instant::now();
    let r = c.ask(json!({"op": "approve", "request": {
        "id": "ap-1", "chat": "chat-x", "harness": "pi", "tool": "cu/open_app",
        "summary": "Open Safari", "timeoutSeconds": 30
    }}));
    assert_eq!(
        r,
        json!({"ok": true, "id": "ap-1", "answer": "no", "by": "none"})
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn the_first_approver_answer_wins_and_the_outcome_is_recorded() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let token = mint(&mut d, "chat-b", "claude-code");
    let mut a1 = Client::daemon(&f);
    let mut a2 = Client::daemon(&f);
    assert_eq!(a1.ask(json!({"op": "approver"}))["ok"], true);
    assert_eq!(a2.ask(json!({"op": "approver"}))["ok"], true);
    let (mut c, _) = Client::hello(&f, &token);
    let mut t = Client::daemon(&f);
    t.ask(json!({"op": "tail", "chat": "chat-b"}));
    let asker = std::thread::spawn(move || {
        c.ask(json!({"op": "approve", "request": {
            "id": "ap-2", "chat": "ignored", "harness": "ignored",
            "tool": "github/create_issue", "summary": "Create issue", "timeoutSeconds": 30
        }}))
    });
    let p1 = a1.recv();
    let p2 = a2.recv();
    assert_eq!(p1, p2);
    assert_eq!(
        p1["approve"]["chat"], "chat-b",
        "the chat comes from the token"
    );
    assert_eq!(p1["approve"]["harness"], "claude-code");
    assert_eq!(
        a2.ask(json!({"op": "answer", "id": "ap-2", "answer": "yes"}))["ok"],
        true
    );
    let reply = asker.join().unwrap();
    assert_eq!(
        reply,
        json!({"ok": true, "id": "ap-2", "answer": "yes", "by": "approver"})
    );
    // The late answer is told the request is gone.
    assert_eq!(
        a1.ask(json!({"op": "answer", "id": "ap-2", "answer": "no"}))["ok"],
        false
    );
    let asked = t.recv();
    let answered = t.recv();
    assert_eq!(asked["event"]["source"], "approval");
    assert_eq!(
        asked["event"]["id"], answered["event"]["id"],
        "the outcome replaces the request"
    );
    assert_eq!(answered["event"]["detail"]["answer"], "yes");
    assert_eq!(answered["event"]["detail"]["by"], "approver");
    assert_eq!(answered["event"]["approvalId"], "ap-2");
}

#[test]
fn an_unanswered_approve_is_no_at_its_timeout() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let mut a = Client::daemon(&f);
    a.ask(json!({"op": "approver"}));
    let started = std::time::Instant::now();
    let r = d.ask(json!({"op": "approve", "request": {
        "id": "ap-3", "chat": "c", "harness": "pi", "tool": "bash",
        "summary": "rm", "timeoutSeconds": 1
    }}));
    assert_eq!(r["answer"], "no");
    assert_eq!(r["by"], "timeout");
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(a.recv()["approve"]["id"], "ap-3");
    // A timeout of 0 is held to 1 second, never "wait forever".
    let r = d.ask(json!({"op": "approve", "request": {
        "id": "ap-4", "chat": "c", "harness": "pi", "tool": "bash",
        "summary": "rm", "timeoutSeconds": 0
    }}));
    assert_eq!(r["by"], "timeout");
}

#[test]
fn an_approver_that_left_is_not_waited_on() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let mut a = Client::daemon(&f);
    a.ask(json!({"op": "approver"}));
    drop(a);
    std::thread::sleep(Duration::from_millis(200));
    let r = d.ask(json!({"op": "approve", "request": {
        "id": "ap-5", "chat": "c", "harness": "pi", "tool": "bash",
        "summary": "x", "timeoutSeconds": 30
    }}));
    assert_eq!(r["by"], "none");
}

#[test]
fn otlp_from_a_minted_pi_token_reaches_tail_as_a_tool_call() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let token = mint(&mut d, "chat-p", "pi");
    let mut t = Client::daemon(&f);
    t.ask(json!({"op": "tail", "chat": "chat-p"}));
    let body = json!({"resourceLogs": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "pi"}}]},
        "scopeLogs": [{"logRecords": [{
            "timeUnixNano": "5000000",
            "body": {"stringValue": "pi.tool_result"},
            "attributes": [
                {"key": "tool_name", "value": {"stringValue": "mcp__exa__search"}},
                {"key": "tool_use_id", "value": {"stringValue": "t1"}},
                {"key": "success", "value": {"stringValue": "true"}}
            ]
        }]}]
    }]})
    .to_string();
    let endpoint = f.daemon.endpoint();
    let port = endpoint.rsplit(':').next().unwrap();
    let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    write!(
        s,
        "POST /v1/logs HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nx-tk-token: {token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut reply = String::new();
    s.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    let e = &t.recv()["event"];
    assert_eq!(e["source"], "otel");
    assert_eq!(
        e["kind"], "tool.call",
        "`pi.` is stripped like `claude_code.`"
    );
    assert_eq!(e["chatId"], "chat-p");
    assert_eq!(e["harness"], "pi");
    // The old header name is not a token header any more.
    let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
    write!(
        s,
        "POST /v1/logs HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nx-monocode-token: {token}\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{{}}"
    )
    .unwrap();
    let mut reply = String::new();
    let _ = s.read_to_string(&mut reply);
    assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
}

#[test]
fn report_counts_and_flags_a_linux_write_outside_the_chat_folder() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let r = d.ask(json!({"op": "event", "event": {
        "id": "run-1", "at": 0, "chatId": "chat-r", "harness": "pi", "source": "run",
        "kind": "run.start", "summary": "pi",
        "detail": {"boundary": "none", "argv": ["pi"], "host": "box", "cwd": "/home/t/proj"}
    }}));
    assert_eq!(r["ok"], true);
    for (id, path) in [("w-in", "/home/t/proj/a.txt"), ("w-out", "/home/t/.bashrc")] {
        let r = d.ask(json!({"op": "event", "event": {
            "id": id, "at": 0, "chatId": "chat-r", "harness": "pi", "source": "auditd",
            "kind": "write", "summary": "", "detail": {"path": path, "tier": "child"}, "pid": 77
        }}));
        assert_eq!(r["ok"], true);
    }
    // The writer thread is asynchronous: wait for the log to hold all three.
    let mut report = Value::Null;
    for _ in 0..50 {
        report = d.ask(json!({"op": "report", "chat": "chat-r", "since": 0, "until": i64::MAX}))
            ["report"]
            .clone();
        if report["events"] == 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(report["events"], 3, "{report}");
    assert_eq!(report["bySource"], json!({"run": 1, "auditd": 2}));
    assert_eq!(report["flags"], 1, "{report}");
    assert_eq!(report["check"]["flags"][0]["eventId"], "w-out");
    assert_eq!(report["check"]["flags"][0]["reason"], "writeOutsideFolders");
    assert_eq!(report["chainOk"], true);
    assert_eq!(
        d.ask(json!({"op": "report", "since": 5, "until": 1}))["error"],
        "until is before since"
    );
}

#[test]
fn ids_the_daemon_makes_never_repeat_across_daemons() {
    // Case 35: a restarted daemon must not reuse an approval record's id.
    let mut ids = std::collections::HashSet::new();
    for _ in 0..2 {
        let f = fixture();
        let mut d = Client::daemon(&f);
        let mut t = Client::daemon(&f);
        t.ask(json!({"op": "tail"}));
        for n in 0..3 {
            d.ask(json!({"op": "approve", "request": {
                "id": format!("ap-{n}"), "chat": "c", "harness": "pi", "tool": "bash",
                "summary": "", "timeoutSeconds": 1
            }}));
            let id = t.recv()["event"]["id"].as_str().unwrap().to_string();
            t.recv();
            assert!(ids.insert(id), "an id repeated");
        }
    }
    assert_eq!(ids.len(), 6);
}

#[test]
fn retention_removes_logs_past_seven_days() {
    let f = fixture();
    let dir = f.root.join("data/activity");
    std::fs::create_dir_all(&dir).unwrap();
    let now = tk_daemon::activity::store::now_ms();
    let day = |ms: i64| {
        tk_daemon::activity::retention::day_name(tk_daemon::activity::retention::day_of_ms(ms))
    };
    let old = dir.join(format!("app-{}.jsonl", day(now - 9 * 86_400_000)));
    let recent = dir.join(format!("app-{}.jsonl", day(now - 2 * 86_400_000)));
    std::fs::write(&old, "").unwrap();
    std::fs::write(&recent, "").unwrap();
    assert!(f.daemon.retention_pass(now) >= 1);
    assert!(!old.exists());
    assert!(recent.exists());
}

#[test]
fn one_chat_cannot_hold_every_approval_slot_or_reuse_a_waiting_id() {
    use tk_daemon::daemon::MAX_PENDING_PER_CHAT;
    let f = fixture();
    let mut d = Client::daemon(&f);
    let token = mint(&mut d, "chat-z", "pi");
    let mut a = Client::daemon(&f);
    a.ask(json!({"op": "approver"}));
    let mut waiting = Vec::new();
    for n in 0..MAX_PENDING_PER_CHAT {
        let (mut c, _) = Client::hello(&f, &token);
        c.send(json!({"op": "approve", "request": {
            "id": format!("z-{n}"), "chat": "", "harness": "pi", "tool": "bash",
            "summary": "", "timeoutSeconds": 30
        }}));
        a.recv();
        waiting.push(c);
    }
    let (mut c, _) = Client::hello(&f, &token);
    let r = c.ask(json!({"op": "approve", "request": {
        "id": "z-extra", "chat": "", "harness": "pi", "tool": "bash",
        "summary": "", "timeoutSeconds": 30
    }}));
    assert_eq!(r["answer"], "no");
    assert_eq!(r["by"], "none", "refused at once, never a hang");
    // The same id while it waits: refused too.
    let r = d.ask(json!({"op": "approve", "request": {
        "id": "z-0", "chat": "other", "harness": "pi", "tool": "bash",
        "summary": "", "timeoutSeconds": 30
    }}));
    assert_eq!(r["by"], "none");
    // Another chat still gets through.
    let asker = std::thread::spawn(move || {
        d.ask(json!({"op": "approve", "request": {
            "id": "o-1", "chat": "other", "harness": "pi", "tool": "bash",
            "summary": "", "timeoutSeconds": 30
        }}))
    });
    assert_eq!(a.recv()["approve"]["id"], "o-1");
    a.ask(json!({"op": "answer", "id": "o-1", "answer": "yes"}));
    assert_eq!(asker.join().unwrap()["answer"], "yes");
    for n in 0..MAX_PENDING_PER_CHAT {
        a.ask(json!({"op": "answer", "id": format!("z-{n}"), "answer": "no"}));
    }
    for mut c in waiting {
        assert_eq!(c.recv()["answer"], "no");
    }
}

/// Integration w1, lane D's request 2: a chat token sits in the harness's
/// environment and, under sbx, in the sandbox's saved config. Once its run
/// stops, the token opens nothing: no new hello, no OTLP post. Only the
/// daemon scope (tk run) can stop a run; another chat's tokens stay valid.
#[test]
fn a_chat_token_is_revoked_when_its_run_stops() {
    let f = fixture();
    let mut d = Client::daemon(&f);
    let tok = mint(&mut d, "chat-r", "pi");
    let tok2 = mint(&mut d, "chat-r", "pi");
    let other = mint(&mut d, "chat-o", "pi");
    let r = d.ask(event(
        "run-1",
        "chat-r",
        "run",
        "run.start",
        json!({"cwd": "/w"}),
    ));
    assert_eq!(r["ok"], true, "{r}");
    let (mut c, r) = Client::hello(&f, &tok);
    assert_eq!(r["ok"], true, "{r}");
    let r = c.ask(event("run-2", "chat-r", "run", "run.stop", json!({})));
    assert_eq!(r["ok"], false, "a chat cannot stop its own run: {r}");
    assert_eq!(Client::hello(&f, &tok).1["ok"], true);
    let r = d.ask(event(
        "run-2",
        "chat-r",
        "run",
        "run.stop",
        json!({"exitCode": 0}),
    ));
    assert_eq!(r["ok"], true, "{r}");
    for t in [&tok, &tok2] {
        let (_c, r) = Client::hello(&f, t);
        assert_eq!(r["ok"], false, "revoked: {r}");
    }
    assert_eq!(
        Client::hello(&f, &other).1["ok"],
        true,
        "another chat's token stays"
    );
}
