//! C7: Cua's recording of one runtime, read back as action records.
//!
//! Cua writes one folder per action under the output directory,
//! `turn-00001/` and on, each with `action.json` and screenshots (recording
//! guide, read 16 Oct 2026). The helper reads each new `action.json` once and
//! sends MonoCode one record. Cua 0.37.0 writes `tool`, `arguments`,
//! `result_summary`, `result_error`, `timestamp` and the session offsets, and
//! no top-level `pid` (`cua-driver-core/src/recording.rs:1436-1447`). Other
//! names are still read as a fallback.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// The largest `action.json` read.
const MAX_ACTION_BYTES: u64 = 256 * 1024;
/// The longest string kept from an action's arguments.
const MAX_ARG_CHARS: usize = 200;
/// The most turn folders read per scan.
const MAX_PER_SCAN: usize = 64;

/// The record sent to MonoCode for one action.
pub fn record_from_action(turn: &str, action: &Value, dir: &Path) -> Value {
    let object = action.as_object();
    let field = |names: &[&str]| -> Option<&Value> {
        let object = object?;
        names.iter().find_map(|name| object.get(*name))
    };
    let tool = field(&["tool", "tool_name", "name"])
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let error = field(&["result_error", "error", "is_error", "isError"])
        .map(|value| match value {
            Value::Bool(flag) => *flag,
            Value::Null => false,
            Value::String(text) => !text.is_empty(),
            _ => true,
        })
        .unwrap_or(false);
    // The target app's pid is an argument of the call, not a field of its own.
    let pid = field(&["arguments"])
        .and_then(|arguments| arguments.get("pid"))
        .and_then(Value::as_u64)
        .filter(|pid| *pid <= u32::MAX as u64);
    let at = field(&["timestamp", "at", "time"])
        .and_then(Value::as_str)
        .map(|text| clip(text, 40));
    let arguments = field(&["arguments", "args", "params"])
        .map(clip_value)
        .unwrap_or(Value::Null);
    let summary = field(&["result_summary", "summary", "result"])
        .and_then(Value::as_str)
        .map(|text| clip(text, MAX_ARG_CHARS));
    let screenshot = ["after.png", "screenshot.png"]
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned());
    let mut record = Map::new();
    record.insert("turn".into(), json!(clip(turn, 32)));
    record.insert("tool".into(), json!(clip(tool, 64)));
    record.insert("error".into(), json!(error));
    record.insert("arguments".into(), arguments);
    if let Some(pid) = pid {
        record.insert("pid".into(), json!(pid));
    }
    if let Some(at) = at {
        record.insert("at".into(), json!(at));
    }
    if let Some(summary) = summary {
        record.insert("summary".into(), json!(summary));
    }
    if let Some(screenshot) = screenshot {
        record.insert("screenshot".into(), json!(screenshot));
    }
    Value::Object(record)
}

/// The helper's own record of a call Cua does not record: a read-only tool,
/// or any call while recording is off. Same fields as Cua's, plus
/// `"source": "helper"`; `turn` is `call-NNNNNN`.
pub fn helper_record(
    turn: &str,
    tool: &str,
    arguments: &Value,
    error: bool,
    at: &str,
    summary: Option<&str>,
    screenshot: Option<&str>,
) -> Value {
    let mut record = Map::new();
    record.insert("turn".into(), json!(clip(turn, 32)));
    record.insert("tool".into(), json!(clip(tool, 64)));
    record.insert("arguments".into(), clip_value(arguments));
    record.insert("error".into(), json!(error));
    record.insert("at".into(), json!(clip(at, 40)));
    if let Some(summary) = summary {
        record.insert("summary".into(), json!(clip(summary, MAX_ARG_CHARS)));
    }
    if let Some(screenshot) = screenshot {
        record.insert("screenshot".into(), json!(screenshot));
    }
    record.insert("source".into(), json!("helper"));
    Value::Object(record)
}

fn clip(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// The arguments with every string cut to `MAX_ARG_CHARS` and depth capped.
fn clip_value(value: &Value) -> Value {
    fn walk(value: &Value, depth: usize) -> Value {
        if depth > 4 {
            return Value::String("…".into());
        }
        match value {
            Value::String(text) => Value::String(text.chars().take(MAX_ARG_CHARS).collect()),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .take(32)
                    .map(|item| walk(item, depth + 1))
                    .collect(),
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .take(32)
                    .map(|(key, item)| (clip(key, 64), walk(item, depth + 1)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    walk(value, 0)
}

/// Reads turn folders not seen before, in order. Folders that are not
/// `turn-<digits>`, symlinks, and oversized files are skipped.
pub struct Scanner {
    root: PathBuf,
    seen: BTreeSet<String>,
}

impl Scanner {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            seen: BTreeSet::new(),
        }
    }

    pub fn scan(&mut self) -> Vec<Value> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut turns: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_turn(name) && !self.seen.contains(name))
            .collect();
        turns.sort();
        let mut out = Vec::new();
        for turn in turns.into_iter().take(MAX_PER_SCAN) {
            let dir = self.root.join(&turn);
            let path = dir.join("action.json");
            let Some(action) = read_small_json(&path) else {
                // Not written yet: try again on the next scan.
                continue;
            };
            out.push(record_from_action(&turn, &action, &dir));
            self.seen.insert(turn);
        }
        out
    }
}

fn is_turn(name: &str) -> bool {
    name.strip_prefix("turn-").is_some_and(|digits| {
        !digits.is_empty() && digits.len() <= 9 && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x0100;
#[cfg(all(unix, not(target_os = "macos")))]
const O_NOFOLLOW: i32 = 0o400000;

fn read_small_json(path: &Path) -> Option<Value> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ACTION_BYTES {
        return None;
    }
    // Open without following a link swapped in after the check (review, 15).
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(O_NOFOLLOW);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_ACTION_BYTES).read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `action.json` as Cua 0.37.0 writes it.
    fn action(tool: &str) -> Value {
        json!({
            "tool": tool,
            "arguments": {"pid": 501},
            "result_summary": "done",
            "result_error": false,
            "timestamp": "2026-10-16T20:00:00.000Z",
            "t_ms_from_session_start": 10,
            "t_start_ms_from_session_start": 2
        })
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cu-helper-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_cua_0370_action_json() {
        // The shape Cua 0.37.0 writes (cua-driver-core/src/recording.rs:1436-1447).
        let record = record_from_action(
            "turn-00001",
            &json!({
                "tool": "click",
                "arguments": {"pid": 4242, "x": 10, "y": 20},
                "result_summary": "Refused",
                "result_error": true,
                "timestamp": "2026-10-16T20:00:00.123Z",
                "t_ms_from_session_start": 1200,
                "t_start_ms_from_session_start": 1100,
                "click_point": {"x": 10, "y": 20}
            }),
            Path::new("/nonexistent"),
        );
        assert_eq!(record["error"], true, "result_error is the error flag");
        assert_eq!(record["pid"], 4242, "pid comes from arguments.pid");
        assert_eq!(record["at"], "2026-10-16T20:00:00.123Z");
        assert_eq!(record["summary"], "Refused");
        let no_pid = record_from_action(
            "turn-2",
            &json!({"tool":"press_key","arguments":{"pid":"x"},"result_error":false,"pid":7}),
            Path::new("/x"),
        );
        assert_eq!(no_pid["error"], false);
        assert!(no_pid.get("pid").is_none(), "only a numeric arguments.pid");
    }

    #[test]
    fn reads_an_action_loosely() {
        let record = record_from_action(
            "turn-00001",
            &json!({"tool":"click","arguments":{"pid":42,"x":10,"text":"a".repeat(500)},"result_summary":"ok","result_error":false,"timestamp":"2026-10-16T20:00:00.000Z","t_ms_from_session_start":5,"t_start_ms_from_session_start":1}),
            Path::new("/nonexistent"),
        );
        assert_eq!(record["tool"], "click");
        assert_eq!(record["error"], false);
        assert_eq!(record["pid"], 42);
        assert_eq!(
            record["arguments"]["text"].as_str().unwrap().len(),
            MAX_ARG_CHARS
        );
        let odd = record_from_action(
            "turn-2",
            &json!({"name":"type_text","is_error":"boom"}),
            Path::new("/x"),
        );
        assert_eq!(odd["tool"], "type_text");
        assert_eq!(odd["error"], true);
        let junk = record_from_action("turn-3", &json!([1, 2]), Path::new("/x"));
        assert_eq!(junk["tool"], "unknown");
    }

    #[test]
    fn scans_each_new_turn_once_in_order() {
        let root = temp_dir("scan");
        for (turn, tool) in [("turn-00002", "type_text"), ("turn-00001", "click")] {
            fs::create_dir_all(root.join(turn)).unwrap();
            fs::write(
                root.join(turn).join("action.json"),
                action(tool).to_string(),
            )
            .unwrap();
        }
        fs::write(root.join("turn-00001").join("after.png"), b"png").unwrap();
        fs::create_dir_all(root.join("turn-00003")).unwrap(); // not written yet
        fs::create_dir_all(root.join("notes")).unwrap();
        let mut scanner = Scanner::new(root.clone());
        let first = scanner.scan();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0]["tool"], "click");
        assert!(first[0]["screenshot"]
            .as_str()
            .unwrap()
            .ends_with("after.png"));
        assert!(scanner.scan().is_empty(), "nothing twice");
        fs::write(
            root.join("turn-00003").join("action.json"),
            action("scroll").to_string(),
        )
        .unwrap();
        assert_eq!(scanner.scan()[0]["tool"], "scroll");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_oversized_and_linked_files() {
        let root = temp_dir("big");
        fs::create_dir_all(root.join("turn-1")).unwrap();
        fs::write(
            root.join("turn-1").join("action.json"),
            vec![b' '; 300 * 1024],
        )
        .unwrap();
        #[cfg(unix)]
        {
            fs::create_dir_all(root.join("turn-2")).unwrap();
            std::os::unix::fs::symlink("/etc/hostname", root.join("turn-2").join("action.json"))
                .unwrap();
        }
        let mut scanner = Scanner::new(root.clone());
        assert!(scanner.scan().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn turn_names() {
        assert!(is_turn("turn-00001"));
        assert!(!is_turn("turn-"));
        assert!(!is_turn("turn-1a"));
        assert!(!is_turn("x-1"));
    }
}
