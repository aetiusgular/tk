//! The event record the daemon stores and streams. The shape is the fork's
//! `ActivityEvent` (`activity/types.rs` at `159ef3e`, serde camelCase) with
//! two sources added: `auditd` (lane D's Linux feed) and `run` (lane D's
//! `tk run` records). Contract: `contracts/daemon-protocol.md`, section 3.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Otel,
    Adapter,
    Eslogger,
    Auditd,
    Cua,
    Approval,
    Run,
}

/// One record. `at` is milliseconds since the epoch, stamped by the daemon
/// when received; a harness's own time stays inside `detail` (case 35).
/// `chat_id` is `""` when the event is not attributed to a chat.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: String,
    pub at: i64,
    pub chat_id: String,
    pub harness: String,
    pub source: Source,
    pub kind: String,
    pub summary: String,
    #[serde(default)]
    pub detail: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_camel_case_names_and_absent_optionals() {
        let text = r#"{"id":"e1","at":1700000000000,"chatId":"","harness":"pi","source":"otel","kind":"tool_call","summary":"exa/search","detail":{"x":1}}"#;
        let e: Event = serde_json::from_str(text).unwrap();
        assert_eq!(e.source, Source::Otel);
        assert!(e.pid.is_none());
        let back = serde_json::to_string(&e).unwrap();
        assert!(back.contains("\"chatId\":\"\""));
        assert!(!back.contains("pid"));
    }

    #[test]
    fn unknown_source_is_refused() {
        let text = r#"{"id":"e1","at":1,"chatId":"","harness":"","source":"webhook","kind":"","summary":""}"#;
        assert!(serde_json::from_str::<Event>(text).is_err());
    }
}
