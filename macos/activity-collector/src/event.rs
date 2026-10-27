//! The `ActivityEvent` record (a copy of `src-tauri/src/activity/types.rs`,
//! since this crate is its own workspace) and the parser from one eslogger
//! JSON line to a [`RawEvent`].
//!
//! eslogger's JSON follows Apple's `es_message_t` layout: a top-level
//! `process`, `time`, an integer `event_type`, and an `event` object with
//! one key named after the event (`exec`, `fork`, ...). This parser keys on
//! that name, never on the integer, which differs between macOS releases.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::timefmt;

/// Where an activity event came from. Same names as the app's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivitySource {
    Otel,
    Adapter,
    Eslogger,
    Cua,
    Approval,
}

/// One entry in the activity log. Field names match the app's type exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub id: String,
    /// Milliseconds since the Unix epoch.
    pub at: i64,
    /// The MonoCode chat, or empty when not attributed.
    pub chat_id: String,
    /// The harness id, or empty when not attributed.
    pub harness: String,
    pub source: ActivitySource,
    pub kind: String,
    pub summary: String,
    /// The source's fields, kept as data.
    pub detail: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
}

/// The eslogger events this parser understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawKind {
    Exec,
    Fork,
    Exit,
    Create,
    Rename,
    Unlink,
    Close,
    Open,
    TccModify,
    Truncate,
    Link,
    Copyfile,
    Clone,
    Exchangedata,
}

impl RawKind {
    /// The eslogger event name, as passed on its command line.
    pub fn es_name(self) -> &'static str {
        match self {
            RawKind::Exec => "exec",
            RawKind::Fork => "fork",
            RawKind::Exit => "exit",
            RawKind::Create => "create",
            RawKind::Rename => "rename",
            RawKind::Unlink => "unlink",
            RawKind::Close => "close",
            RawKind::Open => "open",
            RawKind::TccModify => "tcc_modify",
            RawKind::Truncate => "truncate",
            RawKind::Link => "link",
            RawKind::Copyfile => "copyfile",
            RawKind::Clone => "clone",
            RawKind::Exchangedata => "exchangedata",
        }
    }

    /// All kinds, in the protocol's fixed event-list order. `write` stays
    /// out: too many events, with no way to select processes (note AA,
    /// appendix A).
    pub const ALL: [RawKind; 14] = [
        RawKind::Exec,
        RawKind::Fork,
        RawKind::Exit,
        RawKind::Create,
        RawKind::Rename,
        RawKind::Unlink,
        RawKind::Close,
        RawKind::Open,
        RawKind::TccModify,
        RawKind::Truncate,
        RawKind::Link,
        RawKind::Copyfile,
        RawKind::Clone,
        RawKind::Exchangedata,
    ];

    fn from_es_name(name: &str) -> Option<RawKind> {
        RawKind::ALL.into_iter().find(|k| k.es_name() == name)
    }
}

/// One eslogger event, reduced to the fields the collector uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawEvent {
    pub kind: Option<RawKind>,
    /// The acting process (`process.audit_token.pid`).
    pub pid: u32,
    /// `process.ppid`, when present.
    pub ppid: Option<u32>,
    /// `process.original_ppid`: the parent at fork, kept after a reparent
    /// to launchd (`es_process_t`).
    pub original_ppid: Option<u32>,
    /// `process.group_id`: the process group.
    pub group_id: Option<u32>,
    /// `process.session_id`: the Unix session of that group.
    pub session_id: Option<u32>,
    /// The acting process's executable. For `exec` this is the new image
    /// (`event.exec.target.executable.path`), since that is what now runs.
    pub exe: Option<String>,
    /// `exec` arguments (falls back to `[exe]` when eslogger gives none).
    pub argv: Vec<String>,
    /// The file the event is about (for `link`, `copyfile` and `clone`
    /// the source; for `exchangedata` the first file).
    pub path: Option<String>,
    /// The second file: `rename`, `link`, `copyfile` and `clone`
    /// destination, `exchangedata`'s other file.
    pub dest: Option<String>,
    /// `close`: whether the file was modified.
    pub modified: bool,
    /// `fork`: the child's pid.
    pub child_pid: Option<u32>,
    /// `tcc_modify`: the TCC service, when present.
    pub tcc_service: Option<String>,
    /// `tcc_modify`: the identity whose access changed, when present.
    pub tcc_identity: Option<String>,
    /// Milliseconds since the epoch, from `time`.
    pub time_ms: i64,
}

impl RawEvent {
    /// The kind, which a parsed event always has.
    pub fn raw_kind(&self) -> RawKind {
        self.kind.unwrap_or(RawKind::Exec)
    }
}

fn get_u32(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

fn get_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_owned)
}

/// The path of an `es_file_t` object (`{"path": "..."}`).
fn file_path(file: Option<&Value>) -> Option<String> {
    get_str(file?.get("path"))
}

/// A create or rename destination: `existing_file.path`, or
/// `new_path.dir.path` joined with `new_path.filename`.
fn destination_path(dest: Option<&Value>) -> Option<String> {
    let dest = dest?;
    if let Some(p) = file_path(dest.get("existing_file")) {
        return Some(p);
    }
    let new_path = dest.get("new_path")?;
    join_name(new_path.get("dir"), new_path.get("filename"))
}

/// `dir.path` joined with one file name, refused when the name is empty or
/// holds a `/`.
fn join_name(dir: Option<&Value>, name: Option<&Value>) -> Option<String> {
    let dir = file_path(dir)?;
    let name = name?.as_str()?;
    if name.is_empty() || name.contains('/') {
        return None;
    }
    if dir.ends_with('/') {
        Some(format!("{dir}{name}"))
    } else {
        Some(format!("{dir}/{name}"))
    }
}

/// Parses one eslogger JSON line. Unknown events and malformed lines give
/// `None`; this never panics.
pub fn parse_eslogger_line(line: &str) -> Option<RawEvent> {
    let root: Value = serde_json::from_str(line.trim()).ok()?;
    let event_obj = root.get("event")?.as_object()?;
    // The event object carries exactly one key: the event's name.
    let (name, body) = event_obj
        .iter()
        .find_map(|(k, v)| RawKind::from_es_name(k).map(|kind| (kind, v)))?;
    let process = root.get("process")?;
    let pid = get_u32(process.get("audit_token")?.get("pid")?)?;
    let ppid = process.get("ppid").and_then(get_u32);
    let id_of = |key: &str| process.get(key).and_then(get_u32);
    let time_ms = timefmt::parse_rfc3339_ms(root.get("time")?.as_str()?)?;
    let mut raw = RawEvent {
        kind: Some(name),
        pid,
        ppid,
        original_ppid: id_of("original_ppid"),
        group_id: id_of("group_id"),
        session_id: id_of("session_id"),
        exe: file_path(process.get("executable")),
        time_ms,
        ..RawEvent::default()
    };
    match name {
        RawKind::Exec => {
            let target = body.get("target")?;
            let target_exe = file_path(target.get("executable"));
            if target_exe.is_some() {
                raw.exe = target_exe;
            }
            raw.argv = match body.get("args").and_then(Value::as_array) {
                Some(items) => items
                    .iter()
                    .filter_map(|a| a.as_str().map(str::to_owned))
                    .collect(),
                None => Vec::new(),
            };
            if raw.argv.is_empty() {
                raw.argv = raw.exe.clone().into_iter().collect();
            }
        }
        RawKind::Fork => {
            let child = body.get("child")?;
            raw.child_pid = Some(get_u32(child.get("audit_token")?.get("pid")?)?);
        }
        RawKind::Exit => {}
        RawKind::Create => {
            raw.path = Some(destination_path(body.get("destination"))?);
        }
        RawKind::Rename => {
            raw.path = Some(file_path(body.get("source"))?);
            raw.dest = Some(destination_path(body.get("destination"))?);
        }
        RawKind::Unlink => raw.path = Some(file_path(body.get("target"))?),
        RawKind::Close => {
            raw.path = Some(file_path(body.get("target"))?);
            raw.modified = body.get("modified").and_then(Value::as_bool)?;
        }
        RawKind::Open => raw.path = Some(file_path(body.get("file"))?),
        RawKind::TccModify => {
            raw.tcc_service = get_str(body.get("service"));
            raw.tcc_identity = get_str(body.get("identity"));
        }
        // Member names from Apple's pages for each `es_event_*_t`:
        // truncate `target`; link `source`, `target_dir`, `target_filename`;
        // copyfile `source`, `target_file` (optional), `target_dir`,
        // `target_name`; clone `source`, `target_dir`, `target_name`;
        // exchangedata `file1`, `file2`
        // (https://developer.apple.com/documentation/endpointsecurity/).
        RawKind::Truncate => raw.path = Some(file_path(body.get("target"))?),
        RawKind::Link => {
            raw.path = Some(file_path(body.get("source"))?);
            raw.dest = Some(join_name(
                body.get("target_dir"),
                body.get("target_filename"),
            )?);
        }
        RawKind::Copyfile => {
            raw.path = Some(file_path(body.get("source"))?);
            raw.dest = match file_path(body.get("target_file")) {
                Some(existing) => Some(existing),
                None => Some(join_name(body.get("target_dir"), body.get("target_name"))?),
            };
        }
        RawKind::Clone => {
            raw.path = Some(file_path(body.get("source"))?);
            raw.dest = Some(join_name(body.get("target_dir"), body.get("target_name"))?);
        }
        RawKind::Exchangedata => {
            raw.path = Some(file_path(body.get("file1"))?);
            raw.dest = Some(file_path(body.get("file2"))?);
        }
    }
    Some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(event: &str, pid: u32) -> String {
        format!(
            r#"{{"event_type":9,"time":"2025-10-09T12:00:00.123456789Z","process":{{"audit_token":{{"pid":{pid}}},"ppid":1,"executable":{{"path":"/bin/zsh"}}}},"event":{event}}}"#
        )
    }

    #[test]
    fn event_round_trips_with_the_typescript_field_names() {
        let json = serde_json::json!({
            "id": "e1", "at": 1_760_000_000_000_i64, "chatId": "c1", "harness": "codex",
            "source": "eslogger", "kind": "exec", "summary": "ran ls",
            "detail": { "argv": ["ls"] }, "pid": 42, "approvalId": "a1"
        });
        let event: ActivityEvent = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), json);
    }

    #[test]
    fn exec_with_args() {
        let raw = parse_eslogger_line(&line(
            r#"{"exec":{"target":{"executable":{"path":"/usr/bin/git"}},"args":["git","status"]}}"#,
            10,
        ))
        .unwrap();
        assert_eq!(raw.kind, Some(RawKind::Exec));
        assert_eq!(raw.pid, 10);
        assert_eq!(raw.ppid, Some(1));
        assert_eq!(raw.exe.as_deref(), Some("/usr/bin/git"));
        assert_eq!(raw.argv, vec!["git", "status"]);
        assert_eq!(raw.time_ms, 1_760_011_200_123);
    }

    #[test]
    fn exec_without_args_falls_back_to_target() {
        let raw = parse_eslogger_line(&line(
            r#"{"exec":{"target":{"executable":{"path":"/usr/bin/git"}}}}"#,
            10,
        ))
        .unwrap();
        assert_eq!(raw.argv, vec!["/usr/bin/git"]);
    }

    #[test]
    fn fork_child() {
        let raw = parse_eslogger_line(&line(
            r#"{"fork":{"child":{"audit_token":{"pid":77}}}}"#,
            10,
        ))
        .unwrap();
        assert_eq!(raw.child_pid, Some(77));
    }

    #[test]
    fn create_both_forms() {
        let a = parse_eslogger_line(&line(
            r#"{"create":{"destination_type":0,"destination":{"existing_file":{"path":"/tmp/a"}}}}"#,
            1,
        ))
        .unwrap();
        assert_eq!(a.path.as_deref(), Some("/tmp/a"));
        let b = parse_eslogger_line(&line(
            r#"{"create":{"destination_type":1,"destination":{"new_path":{"dir":{"path":"/tmp"},"filename":"b","mode":420}}}}"#,
            1,
        ))
        .unwrap();
        assert_eq!(b.path.as_deref(), Some("/tmp/b"));
        let bad = line(
            r#"{"create":{"destination":{"new_path":{"dir":{"path":"/tmp"},"filename":"x/y"}}}}"#,
            1,
        );
        assert!(parse_eslogger_line(&bad).is_none());
    }

    #[test]
    fn rename_unlink_close_open_tcc() {
        let r = parse_eslogger_line(&line(
            r#"{"rename":{"source":{"path":"/a"},"destination_type":1,"destination":{"new_path":{"dir":{"path":"/d/"},"filename":"b"}}}}"#,
            1,
        ))
        .unwrap();
        assert_eq!(
            (r.path.as_deref(), r.dest.as_deref()),
            (Some("/a"), Some("/d/b"))
        );
        let u = parse_eslogger_line(&line(r#"{"unlink":{"target":{"path":"/x"}}}"#, 1)).unwrap();
        assert_eq!(u.path.as_deref(), Some("/x"));
        let c = parse_eslogger_line(&line(
            r#"{"close":{"target":{"path":"/x"},"modified":true}}"#,
            1,
        ))
        .unwrap();
        assert!(c.modified);
        let o =
            parse_eslogger_line(&line(r#"{"open":{"file":{"path":"/y"},"fflag":1}}"#, 1)).unwrap();
        assert_eq!(o.path.as_deref(), Some("/y"));
        let t = parse_eslogger_line(&line(
            r#"{"tcc_modify":{"service":"kTCCServiceCamera","identity":"com.x"}}"#,
            1,
        ))
        .unwrap();
        assert_eq!(t.tcc_service.as_deref(), Some("kTCCServiceCamera"));
        assert_eq!(t.tcc_identity.as_deref(), Some("com.x"));
    }

    #[test]
    fn malformed_and_unknown_are_none() {
        for l in [
            "".to_owned(),
            "not json".to_owned(),
            "[]".to_owned(),
            "{\"event\":{}}".to_owned(),
            line(r#"{"mmap":{}}"#, 1),
            line(r#"{"close":{"target":{"path":"/x"}}}"#, 1),
            line(r#"{"fork":{"child":{"audit_token":{"pid":-1}}}}"#, 1),
            line(r#"{"unlink":{"target":{"path":5}}}"#, 1),
            line(r#"{"exec":{}}"#, 1),
            r#"{"time":"bad","process":{"audit_token":{"pid":1}},"event":{"exit":{}}}"#
                .to_owned(),
            r#"{"time":"2025-10-09T12:00:00Z","process":{"audit_token":{"pid":99999999999}},"event":{"exit":{}}}"#
                .to_owned(),
        ] {
            assert!(parse_eslogger_line(&l).is_none(), "{l}");
        }
        let deep = "[".repeat(10_000);
        assert!(parse_eslogger_line(&deep).is_none());
    }
}
