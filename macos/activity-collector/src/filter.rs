//! What to keep, and how one kept event becomes an `ActivityEvent`.
//!
//! `open` is kept only for sensitive paths, `close` only when the file was
//! modified (it becomes kind `write`). The `detail` is bounded and the
//! summary is one line of at most 300 characters built only from the
//! detail's fields.

use serde_json::{Map, Value};

use crate::event::{ActivityEvent, ActivitySource, RawEvent, RawKind};
use crate::tree::Attribution;

/// At most this many `argv` items in `detail`.
pub const MAX_ARGV_ITEMS: usize = 32;
/// At most this many bytes in one `argv` item.
pub const MAX_ARGV_ITEM_BYTES: usize = 512;
/// At most this many bytes in a path or other string field.
pub const MAX_FIELD_BYTES: usize = 4096;
/// At most this many characters in a summary.
pub const MAX_SUMMARY_CHARS: usize = 300;

/// Truncates to at most `max` bytes, on a char boundary.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Paths under a user's home (`/Users/<name>/`) that are sensitive.
const HOME_PREFIXES: [&str; 9] = [
    ".ssh/",
    ".aws/",
    ".gnupg/",
    ".config/gh/",
    ".kube/",
    "Library/Keychains/",
    "Library/Cookies/",
    "Library/Safari/",
    "Library/Application Support/Google/Chrome/",
];

/// Exact files under a user's home that are sensitive.
const HOME_FILES: [&str; 2] = [".docker/config.json", ".netrc"];

/// Chrome files that hold secrets, in any profile folder.
const CHROME_FILES: [&str; 2] = ["Cookies", "Login Data"];

/// Whether an `open` of this path is worth logging.
pub fn is_sensitive_path(path: &str) -> bool {
    if path.contains('\0') {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or("");
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    if path.starts_with("/Library/Keychains/") || path == "/etc/sudoers" {
        return true;
    }
    // `/private/etc/sudoers` is the real path behind `/etc` on macOS.
    if path == "/private/etc/sudoers" {
        return true;
    }
    let Some(rest) = path.strip_prefix("/Users/") else {
        return false;
    };
    let Some((user, in_home)) = rest.split_once('/') else {
        return false;
    };
    if user.is_empty() {
        return false;
    }
    if HOME_FILES.contains(&in_home) {
        return true;
    }
    for prefix in HOME_PREFIXES {
        if in_home.starts_with(prefix) {
            if prefix.ends_with("Chrome/") {
                return CHROME_FILES.contains(&name);
            }
            return true;
        }
    }
    false
}

/// The protocol's kind name for a raw event, or `None` when it is not kept.
pub fn kept_kind(raw: &RawEvent) -> Option<&'static str> {
    Some(match raw.kind? {
        RawKind::Exec => "exec",
        RawKind::Fork => "fork",
        RawKind::Exit => "exit",
        RawKind::Create => "create",
        RawKind::Rename => "rename",
        RawKind::Unlink => "unlink",
        RawKind::Close if raw.modified => "write",
        RawKind::Close => return None,
        RawKind::Open if raw.path.as_deref().is_some_and(is_sensitive_path) => "open",
        RawKind::Open => return None,
        RawKind::TccModify => "tcc",
        RawKind::Truncate => "truncate",
        RawKind::Link => "link",
        RawKind::Copyfile => "copyfile",
        RawKind::Clone => "clone",
        RawKind::Exchangedata => "exchangedata",
    })
}

fn put_str(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        map.insert(
            key.to_owned(),
            Value::String(truncate_bytes(v, MAX_FIELD_BYTES).to_owned()),
        );
    }
}

/// The bounded `detail` object.
pub fn build_detail(raw: &RawEvent, attribution: &Attribution) -> Value {
    let mut map = Map::new();
    put_str(&mut map, "path", raw.path.as_deref());
    put_str(&mut map, "dest", raw.dest.as_deref());
    if raw.kind == Some(RawKind::Exec) && !raw.argv.is_empty() {
        let argv = raw
            .argv
            .iter()
            .take(MAX_ARGV_ITEMS)
            .map(|a| Value::String(truncate_bytes(a, MAX_ARGV_ITEM_BYTES).to_owned()))
            .collect();
        map.insert("argv".to_owned(), Value::Array(argv));
    }
    if let Some(ppid) = raw.ppid {
        map.insert("ppid".to_owned(), Value::from(ppid));
    }
    // The process's lineage beyond `ppid`, for the check to attribute a
    // process whose fork was dropped (`es_process_t`).
    for (key, value) in [
        ("originalPpid", raw.original_ppid),
        ("groupId", raw.group_id),
        ("sessionId", raw.session_id),
    ] {
        if let Some(v) = value {
            map.insert(key.to_owned(), Value::from(v));
        }
    }
    // A fork is logged under the parent (`pid`); the new process is here.
    if raw.kind == Some(RawKind::Fork) {
        if let Some(child) = raw.child_pid {
            map.insert("child".to_owned(), Value::from(child));
        }
    }
    let tier = if attribution.root { "root" } else { "child" };
    map.insert("tier".to_owned(), Value::from(tier));
    put_str(&mut map, "exe", raw.exe.as_deref());
    put_str(&mut map, "grant", attribution.grant.as_deref());
    // Beyond the protocol's list: the TCC fields the task asks to keep.
    if raw.kind == Some(RawKind::TccModify) {
        put_str(&mut map, "service", raw.tcc_service.as_deref());
        put_str(&mut map, "identity", raw.tcc_identity.as_deref());
    }
    Value::Object(map)
}

fn field<'a>(detail: &'a Value, key: &str) -> Option<&'a str> {
    detail.get(key).and_then(Value::as_str)
}

/// One line, at most 300 characters, from `kind` and the detail's fields.
pub fn build_summary(kind: &str, detail: &Value) -> String {
    let mut text = String::from(kind);
    let mut push = |s: &str| {
        text.push(' ');
        text.push_str(s);
    };
    match kind {
        "exec" => {
            if let Some(argv) = detail.get("argv").and_then(Value::as_array) {
                for a in argv.iter().filter_map(Value::as_str) {
                    push(a);
                }
            } else if let Some(exe) = field(detail, "exe") {
                push(exe);
            }
        }
        "fork" | "exit" => {
            if let Some(exe) = field(detail, "exe") {
                push(exe);
            }
            if let Some(child) = detail.get("child").and_then(Value::as_u64) {
                push(&format!("child {child}"));
            }
        }
        "rename" | "link" | "copyfile" | "clone" | "exchangedata" => {
            push(field(detail, "path").unwrap_or("?"));
            push(if kind == "exchangedata" { "<->" } else { "->" });
            push(field(detail, "dest").unwrap_or("?"));
        }
        "tcc" => {
            for key in ["service", "identity"] {
                if let Some(v) = field(detail, key) {
                    push(v);
                }
            }
        }
        _ => {
            if let Some(p) = field(detail, "path") {
                push(p);
            }
        }
    }
    if let Some(grant) = field(detail, "grant") {
        text.push_str(" (grant ");
        text.push_str(grant);
        text.push(')');
    }
    let one_line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_SUMMARY_CHARS)
        .collect();
    one_line
}

/// The event to log, or `None` when it is filtered out. `id` is left empty;
/// the log writer assigns `es-<day>-<seq>`.
pub fn to_activity(raw: &RawEvent, attribution: &Attribution) -> Option<ActivityEvent> {
    let kind = kept_kind(raw)?;
    let detail = build_detail(raw, attribution);
    let summary = build_summary(kind, &detail);
    Some(ActivityEvent {
        id: String::new(),
        at: raw.time_ms,
        chat_id: attribution.chat_id.clone(),
        harness: String::new(),
        source: ActivitySource::Eslogger,
        kind: kind.to_owned(),
        summary,
        detail,
        pid: Some(raw.pid),
        approval_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(grant: Option<&str>) -> Attribution {
        Attribution {
            chat_id: "c1".into(),
            grant: grant.map(str::to_owned),
            root: false,
        }
    }

    #[test]
    fn sensitive_paths() {
        for p in [
            "/Users/a/.ssh/id_ed25519",
            "/Users/a/.aws/credentials",
            "/Users/a/.gnupg/x",
            "/Users/a/.config/gh/hosts.yml",
            "/Users/a/.kube/config",
            "/Users/a/.docker/config.json",
            "/Users/a/.netrc",
            "/Users/a/Library/Keychains/login.keychain-db",
            "/Users/a/Library/Cookies/Cookies.binarycookies",
            "/Users/a/Library/Application Support/Google/Chrome/Default/Cookies",
            "/Users/a/Library/Application Support/Google/Chrome/Profile 1/Login Data",
            "/Users/a/Library/Safari/History.db",
            "/Library/Keychains/System.keychain",
            "/etc/sudoers",
            "/Users/a/src/app/.env",
            "/tmp/x/.env.local",
        ] {
            assert!(is_sensitive_path(p), "{p}");
        }
        for p in [
            "/Users/a/notes.txt",
            "/Users/a/.sshx/id",
            "/Users//.ssh/id",
            "/Users/.ssh/id",
            "/Users/a/.docker/other.json",
            "/Users/a/Library/Application Support/Google/Chrome/Default/History",
            "/Users/a/.envrc",
            "/Users/a/x.env",
            "/etc/sudoers.d/x",
            "/home/a/.ssh/id",
        ] {
            assert!(!is_sensitive_path(p), "{p}");
        }
    }

    #[test]
    fn kinds_and_filtering() {
        let mut raw = RawEvent {
            kind: Some(RawKind::Close),
            pid: 1,
            path: Some("/tmp/a".into()),
            ..RawEvent::default()
        };
        assert_eq!(kept_kind(&raw), None);
        raw.modified = true;
        assert_eq!(kept_kind(&raw), Some("write"));
        raw.kind = Some(RawKind::Open);
        assert_eq!(kept_kind(&raw), None);
        raw.path = Some("/Users/a/.ssh/id_rsa".into());
        assert_eq!(kept_kind(&raw), Some("open"));
        raw.kind = Some(RawKind::TccModify);
        assert_eq!(kept_kind(&raw), Some("tcc"));
        raw.kind = None;
        assert_eq!(kept_kind(&raw), None);
    }

    #[test]
    fn detail_is_bounded() {
        let long = "é".repeat(400); // 800 bytes
        let raw = RawEvent {
            kind: Some(RawKind::Exec),
            pid: 9,
            ppid: Some(1),
            exe: Some("/usr/bin/git".into()),
            argv: (0..40).map(|_| long.clone()).collect(),
            time_ms: 5,
            ..RawEvent::default()
        };
        let ev = to_activity(&raw, &attr(None)).unwrap();
        let argv = ev.detail["argv"].as_array().unwrap();
        assert_eq!(argv.len(), MAX_ARGV_ITEMS);
        assert_eq!(argv[0].as_str().unwrap().len(), MAX_ARGV_ITEM_BYTES);
        assert_eq!(ev.detail["ppid"], 1);
        assert!(ev.summary.chars().count() <= MAX_SUMMARY_CHARS);
        assert_eq!(ev.source, ActivitySource::Eslogger);
        assert_eq!(ev.harness, "");
        assert_eq!(ev.pid, Some(9));
        assert_eq!(ev.at, 5);
    }

    #[test]
    fn summaries() {
        let raw = RawEvent {
            kind: Some(RawKind::Exec),
            pid: 9,
            argv: vec!["git".into(), "commit\n-m".into()],
            ..RawEvent::default()
        };
        assert_eq!(
            to_activity(&raw, &attr(None)).unwrap().summary,
            "exec git commit -m"
        );
        let raw = RawEvent {
            kind: Some(RawKind::Rename),
            pid: 9,
            path: Some("/a".into()),
            dest: Some("/b".into()),
            ..RawEvent::default()
        };
        let ev = to_activity(&raw, &attr(Some("com.apple.calculator"))).unwrap();
        assert_eq!(ev.summary, "rename /a -> /b (grant com.apple.calculator)");
        assert_eq!(ev.detail["grant"], "com.apple.calculator");
        let raw = RawEvent {
            kind: Some(RawKind::TccModify),
            pid: 9,
            tcc_service: Some("kTCCServiceCamera".into()),
            ..RawEvent::default()
        };
        assert_eq!(
            to_activity(&raw, &attr(None)).unwrap().summary,
            "tcc kTCCServiceCamera"
        );
    }

    #[test]
    fn truncation_on_char_boundary() {
        assert_eq!(truncate_bytes("héllo", 2), "h");
        assert_eq!(truncate_bytes("héllo", 3), "hé");
        assert_eq!(truncate_bytes("abc", 10), "abc");
        assert_eq!(truncate_bytes("é", 0), "");
    }
}
