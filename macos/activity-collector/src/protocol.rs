//! Request parsing and reply encoding for the two sockets (PROTOCOL.md,
//! "Registration socket" and "Read socket"). Pure functions.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::event::ActivityEvent;
use crate::tree::ProcessKind;

/// At most this many bytes in one request line.
pub const MAX_REQUEST_BYTES: usize = 4096;
/// At most this many requests on one registration connection.
pub const MAX_REQUESTS_PER_CONNECTION: usize = 64;
/// The largest `limit` an `events` request may ask for.
pub const MAX_EVENTS_LIMIT: usize = 10_000;

/// A registration-socket request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterRequest {
    Register {
        pid: u32,
        chat_id: String,
        kind: ProcessKind,
        at: i64,
    },
    Grant {
        chat_id: String,
        bundle_id: String,
        app_path: Option<String>,
        at: i64,
    },
    GrantEnded {
        chat_id: String,
        bundle_id: String,
        at: i64,
    },
}

/// A read-socket request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadRequest {
    Events {
        since: i64,
        until: i64,
        /// Empty when the request named no chat: every chat.
        chat_id: String,
        limit: usize,
    },
    Status,
}

/// Why a request was refused. The text is the reply's `error`.
pub type ProtoError = &'static str;

/// `chatId`: 1 to 128 characters from `[A-Za-z0-9_.:-]`.
pub fn valid_chat_id(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// `bundleId`: 1 to 255 characters from `[A-Za-z0-9.-]`.
pub fn valid_bundle_id(s: &str) -> bool {
    (1..=255).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

/// `appPath`: absolute, ends in `.app`, no `..`, no NUL, at most 1,024 bytes.
pub fn valid_app_path(s: &str) -> bool {
    s.len() <= 1024
        && s.starts_with('/')
        && s.ends_with(".app")
        && !s.contains('\0')
        && !s.split('/').any(|part| part == "..")
}

fn object(line: &[u8]) -> Result<Map<String, Value>, ProtoError> {
    if line.len() > MAX_REQUEST_BYTES {
        return Err("too long");
    }
    let value: Value = serde_json::from_slice(line).map_err(|_| "bad json")?;
    let Value::Object(map) = value else {
        return Err("bad json");
    };
    match map.get("v") {
        Some(v) if v.as_u64() == Some(1) && v.is_u64() => {}
        _ => return Err("bad version"),
    }
    Ok(map)
}

fn op(map: &Map<String, Value>) -> Result<&str, ProtoError> {
    map.get("op").and_then(Value::as_str).ok_or("bad op")
}

fn int(map: &Map<String, Value>, key: &str, err: ProtoError) -> Result<i64, ProtoError> {
    map.get(key).and_then(Value::as_i64).ok_or(err)
}

fn chat(map: &Map<String, Value>) -> Result<String, ProtoError> {
    match map.get("chatId").and_then(Value::as_str) {
        Some(s) if valid_chat_id(s) => Ok(s.to_owned()),
        _ => Err("bad chatId"),
    }
}

fn bundle(map: &Map<String, Value>) -> Result<String, ProtoError> {
    match map.get("bundleId").and_then(Value::as_str) {
        Some(s) if valid_bundle_id(s) => Ok(s.to_owned()),
        _ => Err("bad bundleId"),
    }
}

/// Parses one registration-socket line.
pub fn parse_register(line: &[u8]) -> Result<RegisterRequest, ProtoError> {
    let map = object(line)?;
    match op(&map)? {
        "register" => {
            let pid = map
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|p| u32::try_from(p).ok())
                .filter(|p| *p > 0)
                .ok_or("bad pid")?;
            let kind = match map.get("kind").and_then(Value::as_str) {
                Some("harness") => ProcessKind::Harness,
                Some("cuaRuntime") => ProcessKind::CuaRuntime,
                _ => return Err("bad kind"),
            };
            Ok(RegisterRequest::Register {
                pid,
                chat_id: chat(&map)?,
                kind,
                at: int(&map, "at", "bad at")?,
            })
        }
        "grant" => {
            let app_path = match map.get("appPath") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if valid_app_path(s) => Some(s.clone()),
                Some(_) => return Err("bad appPath"),
            };
            Ok(RegisterRequest::Grant {
                chat_id: chat(&map)?,
                bundle_id: bundle(&map)?,
                app_path,
                at: int(&map, "at", "bad at")?,
            })
        }
        "grantEnded" => Ok(RegisterRequest::GrantEnded {
            chat_id: chat(&map)?,
            bundle_id: bundle(&map)?,
            at: int(&map, "at", "bad at")?,
        }),
        _ => Err("unknown op"),
    }
}

/// Parses the one read-socket line.
pub fn parse_read(line: &[u8]) -> Result<ReadRequest, ProtoError> {
    let map = object(line)?;
    match op(&map)? {
        "events" => {
            let since = int(&map, "since", "bad since")?;
            let until = int(&map, "until", "bad until")?;
            if since > until {
                return Err("bad range");
            }
            let chat_id = match map.get("chatId") {
                None | Some(Value::Null) => String::new(),
                Some(_) => chat(&map)?,
            };
            let limit = match map.get("limit") {
                None | Some(Value::Null) => MAX_EVENTS_LIMIT,
                Some(v) => v
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| (1..=MAX_EVENTS_LIMIT).contains(n))
                    .ok_or("bad limit")?,
            };
            Ok(ReadRequest::Events {
                since,
                until,
                chat_id,
                limit,
            })
        }
        "status" => Ok(ReadRequest::Status),
        _ => Err("unknown op"),
    }
}

/// `{"ok":true}` plus a newline.
pub fn reply_ok() -> String {
    "{\"ok\":true}\n".to_owned()
}

/// `{"ok":false,"error":"..."}` plus a newline.
pub fn reply_error(error: &str) -> String {
    // Built by hand to keep the protocol's key order (`ok` first).
    let text = serde_json::to_string(error).unwrap_or_else(|_| "\"error\"".to_owned());
    format!("{{\"ok\":false,\"error\":{text}}}\n")
}

/// One event line of an `events` answer.
pub fn reply_event(event: &ActivityEvent) -> String {
    let mut s = serde_json::to_string(event).unwrap_or_default();
    s.push('\n');
    s
}

/// The closing line of an `events` answer.
pub fn reply_end(truncated: bool, chain_ok: bool) -> String {
    format!("{{\"end\":true,\"truncated\":{truncated},\"chainOk\":{chain_ok}}}\n")
}

/// What eslogger is doing, for `status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EsloggerState {
    Running,
    Stopped,
    Missing,
}

/// The `status` answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub version: String,
    pub eslogger: EsloggerState,
    /// eslogger's last stderr line while it is not running, capped and
    /// printable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eslogger_error: Option<String>,
    pub events: Vec<String>,
    pub missing_events: Vec<String>,
    pub tracked: u64,
    pub grants: u64,
    pub dropped: u64,
    pub log_bytes: u64,
}

/// The `status` line.
pub fn reply_status(status: &Status) -> String {
    let mut s = serde_json::to_string(status).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(s: &str) -> Result<RegisterRequest, ProtoError> {
        parse_register(s.as_bytes())
    }

    fn read(s: &str) -> Result<ReadRequest, ProtoError> {
        parse_read(s.as_bytes())
    }

    #[test]
    fn protocol_examples_parse() {
        assert_eq!(
            reg(
                r#"{"v":1,"op":"register","pid":4242,"chatId":"chat-1","kind":"harness","at":1760000000000}"#
            ),
            Ok(RegisterRequest::Register {
                pid: 4242,
                chat_id: "chat-1".into(),
                kind: ProcessKind::Harness,
                at: 1_760_000_000_000
            })
        );
        assert_eq!(
            reg(
                r#"{"v":1,"op":"grant","chatId":"chat-1","bundleId":"com.apple.calculator","appPath":"/System/Applications/Calculator.app","at":1760000000000}"#
            ),
            Ok(RegisterRequest::Grant {
                chat_id: "chat-1".into(),
                bundle_id: "com.apple.calculator".into(),
                app_path: Some("/System/Applications/Calculator.app".into()),
                at: 1_760_000_000_000
            })
        );
        assert!(matches!(
            reg(
                r#"{"v":1,"op":"grantEnded","chatId":"chat-1","bundleId":"com.apple.calculator","at":1760000005000}"#
            ),
            Ok(RegisterRequest::GrantEnded { .. })
        ));
        assert_eq!(
            read(
                r#"{"v":1,"op":"events","since":1760000000000,"until":1760086400000,"chatId":"chat-1","limit":2000}"#
            ),
            Ok(ReadRequest::Events {
                since: 1_760_000_000_000,
                until: 1_760_086_400_000,
                chat_id: "chat-1".into(),
                limit: 2000
            })
        );
        assert_eq!(read(r#"{"v":1,"op":"status"}"#), Ok(ReadRequest::Status));
    }

    #[test]
    fn refusals() {
        let cases: &[(&str, ProtoError)] = &[
            ("nope", "bad json"),
            ("[1]", "bad json"),
            (r#"{"op":"register"}"#, "bad version"),
            (r#"{"v":2,"op":"register"}"#, "bad version"),
            (r#"{"v":1.0,"op":"register"}"#, "bad version"),
            (r#"{"v":"1","op":"register"}"#, "bad version"),
            (r#"{"v":1}"#, "bad op"),
            (r#"{"v":1,"op":"unregister","pid":1}"#, "unknown op"),
            (r#"{"v":1,"op":"status"}"#, "unknown op"),
            (
                r#"{"v":1,"op":"register","pid":0,"chatId":"c","kind":"harness","at":1}"#,
                "bad pid",
            ),
            (
                r#"{"v":1,"op":"register","pid":-4,"chatId":"c","kind":"harness","at":1}"#,
                "bad pid",
            ),
            (
                r#"{"v":1,"op":"register","pid":4294967296,"chatId":"c","kind":"harness","at":1}"#,
                "bad pid",
            ),
            (
                r#"{"v":1,"op":"register","pid":1,"chatId":"c d","kind":"harness","at":1}"#,
                "bad chatId",
            ),
            (
                r#"{"v":1,"op":"register","pid":1,"chatId":"","kind":"harness","at":1}"#,
                "bad chatId",
            ),
            (
                r#"{"v":1,"op":"register","pid":1,"chatId":"c","kind":"shell","at":1}"#,
                "bad kind",
            ),
            (
                r#"{"v":1,"op":"register","pid":1,"chatId":"c","kind":"harness"}"#,
                "bad at",
            ),
            (
                r#"{"v":1,"op":"grant","chatId":"c","bundleId":"com_x","at":1}"#,
                "bad bundleId",
            ),
            (
                r#"{"v":1,"op":"grant","chatId":"c","bundleId":"x","appPath":"Calc.app","at":1}"#,
                "bad appPath",
            ),
            (
                r#"{"v":1,"op":"grant","chatId":"c","bundleId":"x","appPath":"/a/../b.app","at":1}"#,
                "bad appPath",
            ),
            (
                r#"{"v":1,"op":"grant","chatId":"c","bundleId":"x","appPath":"/a/b","at":1}"#,
                "bad appPath",
            ),
            (
                r#"{"v":1,"op":"grant","chatId":"c","bundleId":"x","appPath":5,"at":1}"#,
                "bad appPath",
            ),
        ];
        for (line, err) in cases {
            assert_eq!(reg(line).err(), Some(*err), "{line}");
        }
        assert_eq!(
            reg(&format!(
                r#"{{"v":1,"op":"x","pad":"{}"}}"#,
                "a".repeat(5000)
            ))
            .err(),
            Some("too long")
        );
        let grant_no_path = reg(r#"{"v":1,"op":"grant","chatId":"c","bundleId":"x","at":1}"#);
        assert!(matches!(
            grant_no_path,
            Ok(RegisterRequest::Grant { app_path: None, .. })
        ));
        for (line, err) in [
            (r#"{"v":1,"op":"register"}"#, "unknown op"),
            (r#"{"v":1,"op":"events","until":1}"#, "bad since"),
            (r#"{"v":1,"op":"events","since":2,"until":1}"#, "bad range"),
            (
                r#"{"v":1,"op":"events","since":1,"until":2,"limit":10001}"#,
                "bad limit",
            ),
            (
                r#"{"v":1,"op":"events","since":1,"until":2,"limit":0}"#,
                "bad limit",
            ),
            (
                r#"{"v":1,"op":"events","since":1,"until":2,"chatId":"a b"}"#,
                "bad chatId",
            ),
        ] {
            assert_eq!(read(line).err(), Some(err), "{line}");
        }
        assert_eq!(
            read(r#"{"v":1,"op":"events","since":1,"until":2}"#),
            Ok(ReadRequest::Events {
                since: 1,
                until: 2,
                chat_id: String::new(),
                limit: MAX_EVENTS_LIMIT
            })
        );
    }

    #[test]
    fn validators() {
        assert!(valid_chat_id(&"a".repeat(128)));
        assert!(!valid_chat_id(&"a".repeat(129)));
        assert!(valid_chat_id("A-z_0.9:x"));
        assert!(!valid_chat_id("é"));
        assert!(valid_bundle_id(&"a".repeat(255)));
        assert!(!valid_bundle_id(&"a".repeat(256)));
        assert!(!valid_app_path(&format!("/{}.app", "a".repeat(1020))));
        assert!(valid_app_path(&format!("/{}.app", "a".repeat(1019))));
        assert!(!valid_app_path("/a\0.app"));
        assert!(valid_app_path("/a/..b.app"));
    }

    #[test]
    fn replies() {
        assert_eq!(reply_ok(), "{\"ok\":true}\n");
        assert_eq!(reply_error("full"), "{\"ok\":false,\"error\":\"full\"}\n");
        let v: Value = serde_json::from_str(&reply_error("a\"b")).unwrap();
        assert_eq!(v["error"], "a\"b");
        assert_eq!(
            reply_end(true, false),
            "{\"end\":true,\"truncated\":true,\"chainOk\":false}\n"
        );
        let status = Status {
            version: "0.0.0".into(),
            eslogger: EsloggerState::Running,
            eslogger_error: None,
            events: vec!["exec".into()],
            missing_events: vec!["tcc_modify".into()],
            tracked: 1,
            grants: 0,
            dropped: 3,
            log_bytes: 10,
        };
        let v: Value = serde_json::from_str(&reply_status(&status)).unwrap();
        assert_eq!(v["eslogger"], "running");
        assert_eq!(v["missingEvents"][0], "tcc_modify");
        assert_eq!(v["logBytes"], 10);
    }
}
