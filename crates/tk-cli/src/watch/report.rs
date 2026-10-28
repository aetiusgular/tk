//! `tk watch report`: the daemon's `report` over a window, rendered for a
//! terminal (`contracts/daemon-protocol.md`, the report object). The check
//! inside it is the fork's `FlagsReport`: writes outside a chat's folders,
//! sensitive paths, new hosts, apps left running.

use std::io::Write;

use serde_json::{json, Value};

use super::conn::Conn;
use super::text::{clean, clock, count, date_time, now_ms, parse_when, short};
use super::Args;

pub const FLAGS: &[super::Flag] = &[
    ("--since", true),
    ("--until", true),
    ("--chat", true),
    ("--json", false),
    ("--socket", true),
];

pub const USAGE: &str = "usage: tk watch report [--since <1d|2h|ms>] [--until <now|ms>] [--chat <id>] [--json] [--socket <path>]";

/// The window when `--since` is not given.
pub const DEFAULT_SINCE: &str = "1d";

pub fn main(args: &Args) -> u8 {
    let now = now_ms();
    let since_text = args.value("--since").unwrap_or(DEFAULT_SINCE);
    let until_text = args.value("--until").unwrap_or("now");
    let (Some(since), Some(until)) = (parse_when(since_text, now), parse_when(until_text, now))
    else {
        eprintln!("tk watch report: --since and --until take 30s, 10m, 2h, 1d, now or epoch ms");
        return 2;
    };
    if until < since {
        eprintln!("tk watch report: --until is before --since");
        return 2;
    }
    let socket = match args.socket() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tk watch report: {e}");
            return 2;
        }
    };
    let report = match fetch(&socket, args.value("--chat"), since, until) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tk watch report: {e}");
            return 1;
        }
    };
    let out = std::io::stdout();
    let mut out = out.lock();
    let text = if args.flag("--json") {
        serde_json::to_string_pretty(&report).unwrap_or_default()
    } else {
        render(&report)
    };
    let _ = writeln!(out, "{text}");
    // A broken chain is the one result a script must not miss.
    if report.get("chainOk").and_then(Value::as_bool) == Some(false) {
        3
    } else {
        0
    }
}

pub fn fetch(
    socket: &std::path::Path,
    chat: Option<&str>,
    since: i64,
    until: i64,
) -> Result<Value, String> {
    let mut conn = Conn::open(socket, "tk watch report")?;
    let mut req = json!({"op": "report", "since": since, "until": until});
    if let Some(c) = chat {
        req["chat"] = json!(c);
    }
    let reply = conn.request(&req)?;
    reply
        .get("report")
        .cloned()
        .ok_or_else(|| "daemon protocol: the report reply has no report".into())
}

pub fn render(r: &Value) -> String {
    let n = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0);
    let mut out = String::new();
    let chat = match r.get("chat").and_then(Value::as_str) {
        Some(c) => format!("chat {}", clean(c, 64)),
        None => "every chat".into(),
    };
    out.push_str(&format!(
        "tk watch report: {} to {}, {chat}\n",
        date_time(r.get("since").and_then(Value::as_i64).unwrap_or(0)),
        date_time(r.get("until").and_then(Value::as_i64).unwrap_or(0)),
    ));
    let chain = match r.get("chainOk").and_then(Value::as_bool) {
        Some(true) => "chain ok",
        Some(false) => "CHAIN BROKEN: the log was changed or a record is missing",
        None => "chain not checked",
    };
    out.push_str(&format!(
        "events {}   flags {}   {chain}\n",
        count(n("events")),
        count(n("flags"))
    ));
    if r.get("truncated").and_then(Value::as_bool) == Some(true) {
        out.push_str("truncated: the window holds more events than one report reads\n");
    }
    if n("dropped") > 0 {
        out.push_str(&format!(
            "dropped: {} records the daemon's queue had no room for\n",
            count(n("dropped"))
        ));
    }
    for (title, key) in [
        ("by source", "bySource"),
        ("by kind", "byKind"),
        ("approvals", "approvals"),
    ] {
        if let Some(line) = tally(r.get(key)) {
            out.push_str(&format!("{title}: {line}\n"));
        }
    }
    let check = r.get("check").cloned().unwrap_or(Value::Null);
    if let Some(reach) = check.get("collectorReachable").and_then(Value::as_str) {
        let why = check
            .get("collectorReason")
            .and_then(Value::as_str)
            .map(|w| format!(" ({})", clean(w, 80)))
            .unwrap_or_default();
        out.push_str(&format!("os collector: {}{why}\n", clean(reach, 16)));
    }
    let flags = check
        .get("flags")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if flags.is_empty() {
        out.push_str("no flags\n");
    } else {
        out.push_str("flags:\n");
        for f in &flags {
            out.push_str(&flag_line(f));
            out.push('\n');
        }
    }
    if let Some(sup) = check.get("suppressed").and_then(Value::as_object) {
        let hidden: u64 = sup.values().filter_map(Value::as_u64).sum();
        if hidden > 0 {
            out.push_str(&format!(
                "expected and hidden: {} (tk watch report --json lists them)\n",
                count(hidden)
            ));
        }
    }
    out.trim_end().to_string()
}

fn flag_line(f: &Value) -> String {
    let s = |k: &str| f.get(k).and_then(Value::as_str).unwrap_or("");
    let times = f.get("count").and_then(Value::as_u64).unwrap_or(1);
    let more = if times > 1 {
        format!(" (x{times})")
    } else {
        String::new()
    };
    format!(
        "  {}  {:<16}  {:<20}  {}{more}",
        clock(f.get("at").and_then(Value::as_i64).unwrap_or(0)),
        short(s("chatId"), 16),
        reason(s("reason")),
        clean(s("summary"), 140)
    )
}

/// The check's reasons in words (`activity/check.rs`, `FlagReason`).
fn reason(r: &str) -> String {
    match r {
        "writeOutsideFolders" => "write outside folders".into(),
        "sensitivePath" => "sensitive path".into(),
        "newHost" => "new host".into(),
        "leftRunning" => "left running".into(),
        "appNotGranted" => "app not granted".into(),
        "recordingOff" => "recording off".into(),
        other => clean(other, 20),
    }
}

/// `a 3, b 1` in descending count, at most 8 keys and "N more".
fn tally(v: Option<&Value>) -> Option<String> {
    let m = v?.as_object()?;
    if m.is_empty() {
        return None;
    }
    let mut rows: Vec<(String, u64)> = m
        .iter()
        .map(|(k, v)| (clean(k, 40), v.as_u64().unwrap_or(0)))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let more = rows.len().saturating_sub(8);
    let mut parts: Vec<String> = rows
        .iter()
        .take(8)
        .map(|(k, n)| format!("{k} {}", count(*n)))
        .collect();
    if more > 0 {
        parts.push(format!("{more} more"));
    }
    Some(parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "since": 1792200000000i64, "until": 1792204985350i64, "chat": null,
            "events": 1204, "flags": 1,
            "bySource": {"adapter": 400, "otel": 700, "run": 4},
            "byKind": {"tool.call": 900},
            "approvals": {"yes:approver": 3, "no:timeout": 1},
            "chainOk": true, "truncated": false, "dropped": 0,
            "check": {
                "flags": [{"id": "w-1", "chatId": "tk-aaaa", "at": 1792204000000i64,
                    "reason": "writeOutsideFolders", "summary": "/etc/hosts", "eventId": "e1",
                    "context": [], "count": 2, "lastAt": 0}],
                "events": [], "chainOk": true, "collectorReachable": "no",
                "collectorReason": "not running", "truncated": false, "dropped": 0,
                "expected": [], "suppressed": {"harnessStartup": 5}
            }
        })
    }

    #[test]
    fn the_report_names_counts_flags_and_approvals() {
        let t = render(&sample());
        assert!(t.contains("events 1,204   flags 1   chain ok"), "{t}");
        assert!(t.contains("by source: otel 700, adapter 400, run 4"), "{t}");
        assert!(t.contains("approvals: yes:approver 3, no:timeout 1"), "{t}");
        assert!(t.contains("write outside folders"), "{t}");
        assert!(t.contains("/etc/hosts (x2)"), "{t}");
        assert!(t.contains("os collector: no (not running)"), "{t}");
        assert!(t.contains("expected and hidden: 5"), "{t}");
    }

    #[test]
    fn a_broken_chain_is_said_in_capitals() {
        let mut r = sample();
        r["chainOk"] = json!(false);
        assert!(render(&r).contains("CHAIN BROKEN"));
    }

    #[test]
    fn a_hostile_flag_summary_is_cleaned() {
        let mut r = sample();
        r["check"]["flags"][0]["summary"] = json!("\x1b[2Jfine");
        assert!(!render(&r).contains('\x1b'));
    }

    #[test]
    fn a_long_tally_is_cut() {
        let mut m = serde_json::Map::new();
        for i in 0..12 {
            m.insert(format!("k{i:02}"), json!(i));
        }
        let t = tally(Some(&Value::Object(m))).unwrap();
        assert!(t.ends_with("4 more"), "{t}");
    }
}
