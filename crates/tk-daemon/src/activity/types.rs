//! The activity log's records. Copied from the fork (`activity/types.rs` at
//! `159ef3e`); the event type now lives in `tk-core`.

use serde::{Deserialize, Serialize};

/// The event and its source are `tk_core::event` (the daemon protocol's
/// shape, `contracts/daemon-protocol.md` section 3), which is the fork's
/// `ActivityEvent` with two sources added: `auditd` (lane D's Linux feed) and
/// `run` (`tk run`'s records). One type, so the socket, the receiver and the
/// log never convert.
pub use tk_core::event::{Event as ActivityEvent, Source as ActivitySource};

/// What an approval covered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalSubject {
    Tool,
    Command,
    App,
    Folder,
    Host,
}

/// How long an approval held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalScope {
    Once,
    Chat,
    Always,
}

/// What one approval card, rule or grant allowed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRecord {
    pub id: String,
    pub chat_id: String,
    pub harness: String,
    pub subject: ApprovalSubject,
    pub target: String,
    pub scope: ApprovalScope,
    pub granted: bool,
    pub granted_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
}

/// What kind of process MonoCode registered with the collector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessKind {
    /// A harness MonoCode started (`harness_spawn`).
    Harness,
    /// A Cua runtime the computer-use helper started for a chat.
    CuaRuntime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_with_the_typescript_field_names() {
        let json = serde_json::json!({
            "id": "e1",
            "at": 1_760_000_000_000_i64,
            "chatId": "c1",
            "harness": "codex",
            "source": "eslogger",
            "kind": "exec",
            "summary": "ran ls",
            "detail": { "argv": ["ls"] },
            "pid": 42,
            "approvalId": "a1"
        });
        let event: ActivityEvent = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(event.source, ActivitySource::Eslogger);
        assert_eq!(serde_json::to_value(&event).unwrap(), json);
    }

    #[test]
    fn optional_fields_may_be_absent() {
        let event: ActivityEvent = serde_json::from_value(serde_json::json!({
            "id": "e2", "at": 0, "chatId": "", "harness": "", "source": "otel",
            "kind": "tool.call", "summary": "", "detail": {}
        }))
        .unwrap();
        assert_eq!(event.pid, None);
        assert_eq!(event.approval_id, None);
        let back = serde_json::to_value(&event).unwrap();
        assert!(back.get("pid").is_none());
    }

    #[test]
    fn approval_record_uses_camel_case() {
        let record = ApprovalRecord {
            id: "a1".into(),
            chat_id: "c1".into(),
            harness: "claude".into(),
            subject: ApprovalSubject::App,
            target: "com.apple.calculator".into(),
            scope: ApprovalScope::Chat,
            granted: true,
            granted_at: 5,
            ended_at: None,
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["chatId"], "c1");
        assert_eq!(value["grantedAt"], 5);
        assert_eq!(value["subject"], "app");
        assert!(value.get("endedAt").is_none());
    }

    #[test]
    fn unknown_source_is_refused() {
        let result: Result<ActivityEvent, _> = serde_json::from_value(serde_json::json!({
            "id": "e3", "at": 0, "chatId": "", "harness": "", "source": "shell",
            "kind": "", "summary": "", "detail": {}
        }));
        assert!(result.is_err());
    }
}
