//! Wave 3 contract: the activity log's records, mirrored from
//! `src/features/activity/model/activityTypes.ts`. Lane B owns this file.

use serde::{Deserialize, Serialize};

/// Where an activity event came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivitySource {
    Otel,
    Adapter,
    Eslogger,
    Cua,
    Approval,
}

/// One entry in the activity log. Field names match the TypeScript type.
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
