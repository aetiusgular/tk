//! `tk watch --approve`: a terminal approver on the daemon's `approve`
//! channel (`contracts/daemon-protocol.md`, `approver` and `answer`). The
//! hooks of Claude Code, Codex and OpenCode send an `approve` request when a
//! rule says `ask` and the harness was started with `tk run`; the daemon
//! sends it to every approver and the first answer wins. With no approver
//! connected the daemon answers `no` at once; with no answer in time, `no`.
//!
//! What this approver does not prove (integration w1, security finding S2,
//! open): the daemon takes any process that presents `daemon.token` as an
//! approver, and under `tk run --boundary none` the agent runs as you and
//! can read that file. This client refuses to run inside a chat (its
//! environment or its process ancestry names one), reads answers only from
//! its controlling terminal, drops anything typed before a request is shown,
//! and says S2 in its banner. None of that stops an agent that writes its
//! own client; only a daemon change can (the request in the lane J status
//! doc).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::conn::{check_ok, classify, read_line, Conn, Incoming};
use super::peer;
use super::text::clean;
use super::Args;

pub const FLAGS: &[super::Flag] = &[("--socket", true)];

pub const USAGE: &str = "usage: tk watch --approve [--socket <path>]
       tk watch approve [--socket <path>]

Answers the daemon's approval requests from this terminal: y or yes allows
one call, anything else (Enter too) denies it. Ctrl-C stops; requests then
get no from the daemon.";

/// Typed input this soon after a prompt is shown is dropped: it was typed
/// for something else (a request that just timed out, or the shell).
pub const SETTLE: Duration = Duration::from_millis(300);

pub const BANNER: &str = "tk watch --approve: answering approval requests from this terminal.
Known gap (finding S2, open): the daemon takes any process that can read
daemon.token as an approver, and under `tk run --boundary none` an agent runs
as you and can read it. Allow only calls you expect to see.";

/// Lines into the approver's loop: from the daemon's socket or the terminal.
#[derive(Debug)]
pub enum In {
    Daemon(Incoming),
    DaemonGone(String),
    Typed(String, Instant),
    TtyGone,
}

pub fn main(args: &Args) -> u8 {
    if let Some(why) = inside_a_chat() {
        eprintln!(
            "tk watch --approve: refused: this process is inside a tk chat ({why}). \
             Start the approver from your own terminal, not from an agent's shell."
        );
        return 2;
    }
    let socket = match args.socket() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tk watch --approve: {e}");
            return 2;
        }
    };
    let tty = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!("tk watch --approve: needs a terminal (/dev/tty: {e})");
            return 2;
        }
    };
    let Ok(tty_in) = tty.try_clone() else {
        eprintln!("tk watch --approve: cannot read the terminal");
        return 2;
    };
    let mut out = tty;
    let _ = writeln!(out, "{BANNER}");

    let (tx, rx) = mpsc::channel::<In>();
    let typed = tx.clone();
    std::thread::spawn(move || {
        let mut r = BufReader::new(tty_in);
        loop {
            let mut line = String::new();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = typed.send(In::TtyGone);
                    return;
                }
                Ok(_) => {
                    if typed.send(In::Typed(line, Instant::now())).is_err() {
                        return;
                    }
                }
            }
        }
    });

    // Reconnect when the daemon restarts: its token rotates on every start,
    // so `Conn::open` reads `daemon.token` again each time.
    let mut first = true;
    loop {
        let conn = match Conn::open(&socket, "tk watch --approve") {
            Ok(c) => c,
            Err(e) => {
                if first {
                    let _ = writeln!(out, "tk watch --approve: {e}");
                    return 1;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let writer = match register(conn, tx.clone()) {
            Ok(w) => w,
            Err(e) => {
                let _ = writeln!(out, "tk watch --approve: {e}");
                return 1;
            }
        };
        if !first {
            let _ = writeln!(out, "reconnected to the daemon");
        }
        first = false;
        let mut flush = || flush_tty_input();
        match serve(&rx, writer, &mut out, &mut flush) {
            Ended::DaemonGone(why) => {
                let _ = writeln!(
                    out,
                    "the daemon went away ({why}); waiting for it to come back. Ctrl-C to stop."
                );
            }
            Ended::TtyGone => return 0,
        }
    }
}

/// Says `approver`, checks the reply, and starts the reader thread that
/// feeds the daemon's lines into `tx`. Returns the writing half.
pub fn register(mut conn: Conn, tx: Sender<In>) -> Result<std::os::unix::net::UnixStream, String> {
    conn.send(&json!({"op": "approver"}))?;
    check_ok(conn.line()?)?;
    conn.stream_mode()?;
    let writer = conn.writer()?;
    let mut reader = conn.into_reader();
    std::thread::spawn(move || loop {
        match read_line(&mut reader) {
            Ok(v) => {
                if tx.send(In::Daemon(classify(v))).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(In::DaemonGone(e));
                return;
            }
        }
    });
    Ok(writer)
}

pub enum Ended {
    DaemonGone(String),
    TtyGone,
}

struct Waiting {
    req: Value,
    deadline: Instant,
}

/// The approver's loop: shows one request at a time, sends the answer typed
/// for it, and keeps the rest in order. `flush` drops typed-ahead input
/// when a prompt is shown (the real one calls `tcflush` on the terminal).
pub fn serve(
    rx: &Receiver<In>,
    mut writer: impl Write,
    out: &mut impl Write,
    flush: &mut dyn FnMut(),
) -> Ended {
    let mut queue: VecDeque<Waiting> = VecDeque::new();
    let mut shown: Option<(Waiting, Instant)> = None;
    // The ids of answers sent, oldest first, to match the daemon's replies.
    let mut sent: VecDeque<(String, &'static str)> = VecDeque::new();
    loop {
        if shown.is_none() {
            // Skip requests that timed out while another was on screen.
            while let Some(w) = queue.pop_front() {
                if w.deadline > Instant::now() {
                    flush();
                    drain_typed(rx, &mut queue, &mut sent, out);
                    prompt(out, &w, queue.len());
                    shown = Some((w, Instant::now()));
                    break;
                }
                let _ = writeln!(
                    out,
                    "{}: timed out before it was shown; the daemon answers no",
                    label(&w.req)
                );
            }
        }
        let wait = match &shown {
            Some((w, _)) => w.deadline.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(In::Daemon(Incoming::Approve(req))) => {
                let secs = req
                    .get("timeoutSeconds")
                    .and_then(Value::as_u64)
                    .unwrap_or(60)
                    .clamp(1, 3600);
                // Keep a second of margin: an answer that reaches the
                // daemon after its own timer is refused.
                let deadline = Instant::now()
                    + Duration::from_secs(secs).saturating_sub(Duration::from_secs(1));
                queue.push_back(Waiting { req, deadline });
                if let Some((w, _)) = &shown {
                    let _ = writeln!(
                        out,
                        "  ({} more waiting after {})",
                        queue.len(),
                        label(&w.req)
                    );
                }
            }
            Ok(In::Daemon(Incoming::Reply(reply))) => {
                let (id, word) = sent.pop_front().unwrap_or_default();
                match check_ok(reply) {
                    Ok(_) => {
                        let _ = writeln!(out, "  {word} sent for {}", clean(&id, 64));
                    }
                    Err(e) => {
                        let _ = writeln!(out, "  too late for {}: {e}", clean(&id, 64));
                    }
                }
            }
            Ok(In::Daemon(_)) => {}
            Ok(In::Typed(text, at)) => {
                let Some((w, since)) = shown.take() else {
                    let _ = writeln!(out, "  (no request is waiting; that input was ignored)");
                    continue;
                };
                if at < since + SETTLE {
                    let _ = writeln!(
                        out,
                        "  (input typed before the request was shown was ignored)"
                    );
                    shown = Some((w, since));
                    continue;
                }
                let yes = is_yes(&text);
                let word = if yes { "yes" } else { "no" };
                let id = w
                    .req
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let line = json!({"op": "answer", "id": id, "answer": word});
                if writeln!(writer, "{line}").is_err() {
                    return Ended::DaemonGone("the answer could not be sent".into());
                }
                sent.push_back((id, word));
            }
            Ok(In::DaemonGone(why)) => return Ended::DaemonGone(why),
            Ok(In::TtyGone) => return Ended::TtyGone,
            Err(RecvTimeoutError::Timeout) => {
                if let Some((w, _)) = shown.take() {
                    let _ = writeln!(out, "  {}: timed out; the daemon answers no", label(&w.req));
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ended::TtyGone,
        }
    }
}

/// Drops typed lines already queued, keeping the daemon's lines in order.
fn drain_typed(
    rx: &Receiver<In>,
    queue: &mut VecDeque<Waiting>,
    sent: &mut VecDeque<(String, &'static str)>,
    out: &mut impl Write,
) {
    let mut keep = Vec::new();
    while let Ok(m) = rx.try_recv() {
        match m {
            In::Typed(..) => {}
            In::Daemon(Incoming::Approve(req)) => {
                let secs = req
                    .get("timeoutSeconds")
                    .and_then(Value::as_u64)
                    .unwrap_or(60)
                    .clamp(1, 3600);
                let deadline = Instant::now()
                    + Duration::from_secs(secs).saturating_sub(Duration::from_secs(1));
                queue.push_back(Waiting { req, deadline });
            }
            other => keep.push(other),
        }
    }
    // Anything else (a reply, a close) cannot be pushed back into an mpsc
    // queue; it is rare here and handled now.
    for m in keep {
        if let In::Daemon(Incoming::Reply(reply)) = m {
            let (id, word) = sent.pop_front().unwrap_or_default();
            let note = match check_ok(reply) {
                Ok(_) => format!("  {word} sent for {}", clean(&id, 64)),
                Err(e) => format!("  too late for {}: {e}", clean(&id, 64)),
            };
            let _ = writeln!(out, "{note}");
        }
    }
}

/// Only `y` or `yes`, in any letter case, allows. Enter, `n`, anything else
/// denies.
pub fn is_yes(text: &str) -> bool {
    matches!(text.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn label(req: &Value) -> String {
    let s = |k: &str| req.get(k).and_then(Value::as_str).unwrap_or("");
    format!("{} in {}", clean(s("tool"), 60), clean(s("chat"), 40))
}

fn prompt(out: &mut impl Write, w: &Waiting, more: usize) {
    let s = |k: &str| w.req.get(k).and_then(Value::as_str).unwrap_or("");
    let left = w
        .deadline
        .saturating_duration_since(Instant::now())
        .as_secs();
    let after = if more > 0 {
        format!(", {more} more waiting")
    } else {
        String::new()
    };
    // The labels are tk's; every value after them came from a hook or a
    // chat token and is cleaned (text::clean) so it cannot draw a line of
    // its own.
    let _ = writeln!(out, "\n-- approval request, {left} s left{after} --");
    let _ = writeln!(out, "  chat     {}", clean(s("chat"), 64));
    let _ = writeln!(out, "  harness  {}", clean(s("harness"), 32));
    let _ = writeln!(out, "  tool     {}", clean(s("tool"), 200));
    let _ = writeln!(out, "  request  {}", clean(s("summary"), 500));
    let _ = writeln!(out, "  id       {}", clean(s("id"), 128));
    let _ = write!(out, "Allow this one call? [y/N] ");
    let _ = out.flush();
}

/// `tcflush(TCIFLUSH)` on the controlling terminal: drops typed-ahead input
/// the kernel holds, so an answer meant for an earlier prompt cannot land on
/// this one.
fn flush_tty_input() {
    use std::os::fd::AsRawFd;
    if let Ok(t) = std::fs::File::open("/dev/tty") {
        // SAFETY: tcflush on an open terminal fd; errors are ignored.
        unsafe {
            libc::tcflush(t.as_raw_fd(), libc::TCIFLUSH);
        }
    }
}

/// Why this process looks like it runs inside a tk chat, if it does: a
/// variable `tk run` sets in the harness (`contracts/daemon-protocol.md`,
/// section 2), or a `tk run` process among its ancestors. An agent that
/// leaves the tree (a double fork, `setsid`, a terminal multiplexer started
/// before the chat) is not seen here; that is S2.
pub fn inside_a_chat() -> Option<String> {
    for var in ["TK_CHAT_ID", "TK_CHAT_TOKEN", "TK_CHAT_TOKEN_FILE"] {
        if std::env::var_os(var).is_some() {
            return Some(format!("{var} is set"));
        }
    }
    peer::tk_run_ancestor(std::process::id()).map(|pid| format!("its ancestor {pid} is `tk run`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Wire(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for Wire {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn req(id: &str, secs: u64) -> In {
        In::Daemon(Incoming::Approve(json!({
            "id": id, "chat": "tk-c1", "harness": "claude-code", "tool": "bash",
            "summary": "rm -rf build", "timeoutSeconds": secs
        })))
    }

    fn typed(text: &str, delay: Duration) -> In {
        In::Typed(text.into(), Instant::now() + delay)
    }

    fn answers(wire: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> Vec<Value> {
        String::from_utf8(wire.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn run(msgs: Vec<In>, pause_after: usize) -> (Vec<Value>, String) {
        let (tx, rx) = mpsc::channel();
        let wire = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let w = Wire(wire.clone());
        let h = std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut flush = || {};
            serve(&rx, w, &mut out, &mut flush);
            String::from_utf8(out).unwrap()
        });
        for (i, m) in msgs.into_iter().enumerate() {
            tx.send(m).unwrap();
            if i + 1 == pause_after {
                std::thread::sleep(SETTLE + Duration::from_millis(100));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        tx.send(In::TtyGone).unwrap();
        let out = h.join().unwrap();
        (answers(&wire), out)
    }

    #[test]
    fn only_y_or_yes_allows() {
        for y in ["y", "Y", "yes", " YES \n"] {
            assert!(is_yes(y), "{y}");
        }
        for n in [
            "",
            "\n",
            "n",
            "no",
            "yy",
            "yes please",
            "ye",
            "1",
            "true",
            "y\u{0}",
        ] {
            assert!(!is_yes(n), "{n:?}");
        }
    }

    #[test]
    fn a_yes_typed_after_the_prompt_is_sent_for_that_request() {
        let (a, out) = run(vec![req("ap-1", 30), typed("y\n", SETTLE * 2)], 1);
        assert_eq!(
            a,
            vec![json!({"op": "answer", "id": "ap-1", "answer": "yes"})]
        );
        assert!(out.contains("Allow this one call? [y/N]"), "{out}");
    }

    #[test]
    fn enter_alone_denies() {
        let (a, _) = run(vec![req("ap-1", 30), typed("\n", SETTLE * 2)], 1);
        assert_eq!(a[0]["answer"], "no");
    }

    #[test]
    fn input_typed_before_the_prompt_never_answers_it() {
        // A "y" that arrives with a stamp before the prompt plus SETTLE.
        let (a, out) = run(vec![req("ap-1", 30), typed("y\n", Duration::ZERO)], 1);
        assert!(a.is_empty(), "{a:?}");
        assert!(out.contains("typed before the request was shown"), "{out}");
    }

    #[test]
    fn input_with_no_request_waiting_is_ignored() {
        let (a, out) = run(vec![typed("y\n", Duration::ZERO)], 0);
        assert!(a.is_empty());
        assert!(out.contains("no request is waiting"));
    }

    #[test]
    fn requests_are_answered_one_at_a_time_in_order() {
        let (tx, rx) = mpsc::channel();
        let wire = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let w = Wire(wire.clone());
        let h = std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut flush = || {};
            serve(&rx, w, &mut out, &mut flush);
            String::from_utf8(out).unwrap()
        });
        tx.send(req("ap-1", 30)).unwrap();
        tx.send(req("ap-2", 30)).unwrap();
        std::thread::sleep(SETTLE + Duration::from_millis(100));
        tx.send(In::Typed("y\n".into(), Instant::now())).unwrap();
        std::thread::sleep(SETTLE + Duration::from_millis(150));
        tx.send(In::Typed("n\n".into(), Instant::now())).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        tx.send(In::TtyGone).unwrap();
        let out = h.join().unwrap();
        let a = answers(&wire);
        assert_eq!(a.len(), 2, "{out}");
        assert_eq!(
            (a[0]["id"].as_str(), a[0]["answer"].as_str()),
            (Some("ap-1"), Some("yes"))
        );
        assert_eq!(
            (a[1]["id"].as_str(), a[1]["answer"].as_str()),
            (Some("ap-2"), Some("no"))
        );
    }

    #[test]
    fn a_request_that_times_out_is_not_answered() {
        // 1 s with the 1 s margin: the deadline is now.
        let (a, out) = run(vec![req("ap-1", 1)], 1);
        assert!(a.is_empty(), "{a:?}");
        assert!(out.contains("timed out"), "{out}");
    }

    #[test]
    fn a_hostile_summary_cannot_draw_a_prompt_line() {
        let (tx, rx) = mpsc::channel();
        let wire = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let w = Wire(wire.clone());
        let h = std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut flush = || {};
            serve(&rx, w, &mut out, &mut flush);
            String::from_utf8(out).unwrap()
        });
        tx.send(In::Daemon(Incoming::Approve(json!({
            "id": "ap-1", "chat": "c\x1b[2J", "harness": "pi", "tool": "bash",
            "summary": "ls\n-- approval request --\nAllow? \x1b]0;x\x07", "timeoutSeconds": 30
        }))))
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        tx.send(In::TtyGone).unwrap();
        let out = h.join().unwrap();
        assert!(!out.contains('\x1b') && !out.contains('\x07'), "{out:?}");
        // The only lines starting with "-- approval" or "Allow" are tk's own.
        assert_eq!(out.matches("\n-- approval request").count(), 1, "{out}");
    }

    #[test]
    fn a_daemon_that_goes_away_ends_the_loop_for_a_reconnect() {
        let (tx, rx) = mpsc::channel();
        tx.send(In::DaemonGone("closed".into())).unwrap();
        let mut out = Vec::new();
        let mut flush = || {};
        assert!(matches!(
            serve(&rx, Vec::new(), &mut out, &mut flush),
            Ended::DaemonGone(_)
        ));
    }

    #[test]
    fn the_banner_names_s2() {
        assert!(BANNER.contains("S2"));
    }
}
