//! An action, placed in a chat, to the daemon's event
//! (`contracts/daemon-protocol.md` section 3, `tk_core::event::Event`).
//!
//! Kinds and detail keys are the macOS collector's (`macos/activity-
//! collector/src/filter.rs:92-170`), because the daemon's checks read those
//! (`crates/tk-daemon/src/activity/check.rs`): `exec`, `fork`, `write`,
//! `create`, `open`, `truncate`, `rename`, `unlink`, `link`, with `path`,
//! `dest`, `argv`, `ppid`, `child`, `tier`, `exe`. Connections are
//! `net.connect` with `host` (`check.rs` `event_host` reads `host`). The
//! bounds are the collector's: 32 argv items of 512 bytes, 4,096 bytes per
//! field; the summary is at most 200 characters (the contract).

use serde_json::{json, Map, Value};
use tk_core::event::{Event, Source};

use crate::action::{Action, What};
use crate::checks::{host_key, Reason};
use crate::redact;
use crate::tree::Placement;

pub const MAX_ARGV_ITEMS: usize = 32;
pub const MAX_ARGV_ITEM_BYTES: usize = 512;
pub const MAX_FIELD_BYTES: usize = 4096;
pub const MAX_SUMMARY_CHARS: usize = 200;

pub fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The event id: the kernel's timestamp and serial, so reading the same
/// record twice (a feed restart over the same log) replaces the first copy
/// rather than adding one (case 35).
pub fn event_id(action: &Action) -> String {
    format!("auditd-{}-{}", action.ts_ms, action.serial)
}

pub fn to_event(action: &Action, placed: &Placement, reasons: &[Reason]) -> Event {
    let mut d = Map::new();
    let put = |d: &mut Map<String, Value>, k: &str, v: &str| {
        d.insert(
            k.to_string(),
            Value::String(truncate_bytes(v, MAX_FIELD_BYTES)),
        );
    };
    let kind: &str;
    let summary: String;
    match &action.what {
        What::Exec { argv } => {
            kind = "exec";
            let args: Vec<String> = redact::argv(argv)
                .into_iter()
                .take(MAX_ARGV_ITEMS)
                .map(|a| truncate_bytes(&a, MAX_ARGV_ITEM_BYTES))
                .collect();
            summary = format!("exec {}", args.join(" "));
            d.insert("argv".into(), json!(args));
            if argv.len() > MAX_ARGV_ITEMS {
                d.insert("argvTruncated".into(), json!(argv.len()));
            }
        }
        What::Fork { child } => {
            kind = "fork";
            summary = format!("fork {} child {child}", action.comm);
            d.insert("child".into(), json!(child));
        }
        What::Write {
            path,
            created,
            flags_known,
        } => {
            kind = if *created { "create" } else { "write" };
            summary = format!("{kind} {path}");
            put(&mut d, "path", path);
            if !flags_known {
                d.insert("flagsKnown".into(), json!(false));
            }
        }
        What::Open { path } => {
            kind = "open";
            summary = format!("open {path}");
            put(&mut d, "path", path);
        }
        What::Truncate { path } => {
            kind = "truncate";
            summary = format!("truncate {path}");
            put(&mut d, "path", path);
        }
        What::Rename { from, to } => {
            kind = "rename";
            summary = format!("rename {from} -> {to}");
            put(&mut d, "path", from);
            put(&mut d, "dest", to);
        }
        What::Unlink { path } => {
            kind = "unlink";
            summary = format!("unlink {path}");
            put(&mut d, "path", path);
        }
        What::Link { from, to } => {
            kind = "link";
            summary = format!("link {from} -> {to}");
            put(&mut d, "path", from);
            put(&mut d, "dest", to);
        }
        What::Connect { addr, port, .. } => {
            kind = "net.connect";
            let host = host_key(addr, *port);
            summary = format!("connect {host}");
            put(&mut d, "host", &host);
            d.insert("addr".into(), json!(addr));
            d.insert("port".into(), json!(port));
        }
    }
    d.insert("ppid".into(), json!(action.ppid));
    d.insert(
        "tier".into(),
        json!(if placed.is_root { "root" } else { "child" }),
    );
    put(&mut d, "exe", &action.exe);
    put(&mut d, "comm", &action.comm);
    d.insert("syscall".into(), json!(action.syscall));
    if let Some(cwd) = &action.cwd {
        put(&mut d, "cwd", cwd);
    }
    if action.denied {
        d.insert("denied".into(), json!(true));
    }
    d.insert("auditAt".into(), json!(action.ts_ms));
    d.insert("auditSerial".into(), json!(action.serial));
    if !reasons.is_empty() {
        d.insert(
            "preflags".into(),
            json!(reasons.iter().map(|r| r.key()).collect::<Vec<_>>()),
        );
    }
    let summary: String = summary
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_SUMMARY_CHARS)
        .collect();
    Event {
        id: event_id(action),
        at: action.ts_ms,
        chat_id: placed.chat.clone(),
        harness: placed.harness.clone(),
        source: Source::Auditd,
        kind: kind.to_string(),
        summary,
        detail: Value::Object(d),
        pid: Some(action.pid),
        approval_id: None,
    }
}
