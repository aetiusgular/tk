//! `tk watch tail`: the daemon's feed, one line per event, as it arrives
//! (`contracts/daemon-protocol.md`, `tail`). With `--since` the daemon sends
//! its backlog first (the log on disk and the last 4,096 events, the last
//! copy per chat and id); without it, live events only.

use std::io::Write;

use serde_json::{json, Value};

use super::conn::{check_ok, classify, Conn, Incoming};
use super::text::{clean, clock, now_ms, parse_when, short};
use super::Args;

pub const FLAGS: &[super::Flag] = &[
    ("--since", true),
    ("--chat", true),
    ("--json", false),
    ("--socket", true),
];

pub const USAGE: &str = "usage: tk watch tail [--since <30s|10m|2h|1d|ms>] [--chat <id>] [--json] [--socket <path>]";

pub fn main(args: &Args) -> u8 {
    let since = match args.value("--since") {
        Some(t) => match parse_when(t, now_ms()) {
            Some(ms) => Some(ms),
            None => {
                eprintln!("tk watch tail: --since takes 30s, 10m, 2h, 1d, now or epoch ms, got {t:?}");
                return 2;
            }
        },
        None => None,
    };
    let socket = match args.socket() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tk watch tail: {e}");
            return 2;
        }
    };
    let conn = match Conn::open(&socket, "tk watch tail") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tk watch tail: {e}");
            return 1;
        }
    };
    let out = std::io::stdout();
    let mut out = out.lock();
    match run(conn, since, args.value("--chat"), args.flag("--json"), &mut out) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("tk watch tail: {e}");
            1
        }
    }
}

/// Streams until the daemon closes or stdout goes away.
pub fn run(
    mut conn: Conn,
    since: Option<i64>,
    chat: Option<&str>,
    json_out: bool,
    out: &mut impl Write,
) -> Result<(), String> {
    let mut req = json!({"op": "tail"});
    if let Some(s) = since {
        req["since"] = json!(s);
    }
    if let Some(c) = chat {
        req["chat"] = json!(c);
    }
    conn.send(&req)?;
    // The reply comes before any push (contract section 2).
    check_ok(conn.line()?)?;
    conn.stream_mode()?;
    if !json_out {
        let _ = writeln!(
            out,
            "{:<8}  {:<16}  {:<11}  {:<8}  {:<12}  summary",
            "time", "chat", "harness", "source", "kind"
        );
    }
    loop {
        let line = match conn.line() {
            Ok(l) => l,
            Err(e) if e.contains("closed") => return Ok(()),
            Err(e) => return Err(e),
        };
        if let Incoming::Event(ev) = classify(line) {
            let text = if json_out {
                serde_json::to_string(&ev).unwrap_or_default()
            } else {
                render(&ev)
            };
            if writeln!(out, "{text}").is_err() || out.flush().is_err() {
                return Ok(());
            }
        }
    }
}

/// One event as one terminal line. Every field is cleaned (`text::clean`):
/// a summary is written by a harness or a chat token, so by the agent too.
pub fn render(ev: &Value) -> String {
    let s = |k: &str| ev.get(k).and_then(Value::as_str).unwrap_or("");
    let at = ev.get("at").and_then(Value::as_i64).unwrap_or(0);
    let chat = if s("chatId").is_empty() {
        "-".to_string()
    } else {
        short(s("chatId"), 16)
    };
    let mut summary = clean(s("summary"), 160);
    if let Some(flags) = preflags(ev) {
        summary.push_str(&format!("  [{flags}]"));
    }
    format!(
        "{:<8}  {:<16}  {:<11}  {:<8}  {:<12}  {}",
        clock(at),
        chat,
        short(s("harness"), 11),
        short(s("source"), 8),
        short(s("kind"), 12),
        summary
    )
}

/// The Linux feed's own marks (`detail.preflags`, contract section 3), shown
/// so a write outside the folder stands out in the stream. The daemon's
/// check, in `tk watch report`, is what decides.
fn preflags(ev: &Value) -> Option<String> {
    let p = ev.get("detail")?.get("preflags")?.as_object()?;
    let on: Vec<String> = p
        .iter()
        .filter(|(_, v)| v.as_bool() == Some(true))
        .map(|(k, _)| clean(k, 24))
        .collect();
    (!on.is_empty()).then(|| on.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_has_the_columns_and_cleans_the_summary() {
        let ev = json!({
            "id": "pi-1", "at": 1792204985350i64, "chatId": "tk-3f2a1c9b0d11",
            "harness": "pi", "source": "adapter", "kind": "tool.call",
            "summary": "bash ok\x1b]0;owned\x07", "detail": {}
        });
        let line = render(&ev);
        assert!(line.contains("tk-3f2a1c9b0d11"), "{line}");
        assert!(line.contains("tool.call"));
        assert!(!line.contains('\x1b') && !line.contains('\x07'), "{line}");
    }

    #[test]
    fn an_unattributed_event_shows_a_dash() {
        let ev = json!({"id": "x", "at": 0, "chatId": "", "harness": "", "source": "auditd", "kind": "write", "summary": "w"});
        assert!(render(&ev).contains(" -  "));
    }

    #[test]
    fn the_linux_feeds_marks_are_shown() {
        let ev = json!({"id": "x", "at": 0, "chatId": "c", "harness": "pi", "source": "auditd",
            "kind": "write", "summary": "/etc/x", "detail": {"preflags": {"writeOutside": true, "sensitive": false}}});
        assert!(render(&ev).ends_with("[writeOutside]"), "{}", render(&ev));
    }

    #[test]
    fn a_hostile_chat_id_and_harness_are_cleaned_too() {
        let ev = json!({"id": "x", "at": 0, "chatId": "\x1b[31mc", "harness": "p\u{202e}i",
            "source": "adapter", "kind": "k\n", "summary": ""});
        let line = render(&ev);
        assert!(!line.chars().any(super::super::text::hidden), "{line:?}");
    }
}
