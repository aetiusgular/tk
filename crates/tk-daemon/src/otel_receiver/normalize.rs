//! One OTLP log record from a harness into one activity event.
//!
//! The chat and the harness come from the token the request carried, never
//! from the record: a harness (or a command an agent ran with the harness's
//! environment) can write any attribute it likes, but only into its own
//! chat. Every event is marked `trust: "harness"` in its detail for that
//! reason. MonoCode's own adapter events are the trusted record.

use serde_json::{Map, Value};

use super::decode::OtelLog;
use crate::activity::types::{ActivityEvent, ActivitySource};

/// Longest string kept in an event's detail, in characters.
pub const MAX_STRING_CHARS: usize = 2000;
/// Rough cap on one event's detail, in bytes of JSON.
pub const MAX_DETAIL_BYTES: usize = 16 * 1024;
/// Longest summary line, in characters.
pub const MAX_SUMMARY_CHARS: usize = 200;

/// Events dropped as noise: one per streamed chunk or socket frame, and
/// start-up and latency timings. None of them is something the agent did.
fn is_noise(name: &str) -> bool {
    matches!(name, "sse_event" | "startup_phase" | "turn_ttft") || name.starts_with("websocket")
}

/// Attributes that hold who you are rather than what the agent did. They
/// never reach the log.
fn is_identity(key: &str) -> bool {
    key.starts_with("user.") || key.starts_with("organization.") || key == "account_id"
}

/// Attributes that hold prompt or reply text. Kept only when the switch is
/// on, even when a harness sends them anyway (a user's own environment may
/// ask it to). Off, both harnesses send a redaction mark in their place
/// (Claude Code 2.1.302 `<REDACTED>`, Codex 0.161.0 `[REDACTED]`, seen by the
/// contract check); it is dropped too.
fn is_prompt_text(key: &str) -> bool {
    matches!(key, "prompt" | "prompt_text" | "user_prompt" | "response") || key.ends_with(".prompt")
}

/// The event name without the harness prefix.
pub fn short_name(name: &str) -> &str {
    name.strip_prefix("claude_code.")
        .or_else(|| name.strip_prefix("codex."))
        .unwrap_or(name)
}

/// Text cut to `max` characters, with control characters removed.
pub fn clip(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    for ch in text.chars() {
        if ch.is_control() && ch != '\n' && ch != '\t' {
            continue;
        }
        if count == max {
            out.push('…');
            return out;
        }
        out.push(ch);
        count += 1;
    }
    out
}

fn one_line(text: &str) -> String {
    clip(&text.replace(['\n', '\t', '\r'], " "), MAX_SUMMARY_CHARS)
}

/// A value with every string clipped, for the detail.
fn clipped(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(clip(s, MAX_STRING_CHARS)),
        Value::Array(items) => Value::Array(items.iter().map(clipped).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), clipped(v))).collect())
        }
        other => other.clone(),
    }
}

fn str_attr<'a>(attrs: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| attrs.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

fn num_attr(attrs: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| match attrs.get(*k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

fn bool_attr(attrs: &Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|k| match attrs.get(*k)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s == "true" => Some(true),
        Value::String(s) if s == "false" => Some(false),
        _ => None,
    })
}

/// The kind and summary line for an event, from its short name.
fn describe(name: &str, attrs: &Map<String, Value>) -> (String, String) {
    let tool = str_attr(attrs, &["tool_name", "tool.name", "name"]).unwrap_or("tool");
    let model = str_attr(attrs, &["model", "slug"]).unwrap_or("model");
    let ms = num_attr(attrs, &["duration_ms"])
        .map(|d| format!(", {} ms", d.round() as i64))
        .unwrap_or_default();
    match name {
        "tool_result" => {
            let outcome = match bool_attr(attrs, &["success"]) {
                Some(true) => "ok",
                Some(false) => "failed",
                None => "done",
            };
            ("tool.call".into(), format!("{tool} {outcome}{ms}"))
        }
        "tool_decision" => {
            let decision = str_attr(attrs, &["decision"]).unwrap_or("decided");
            let source = str_attr(attrs, &["source"])
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            ("approval".into(), format!("{tool}: {decision}{source}"))
        }
        "api_request" => {
            let tokens = match (
                num_attr(attrs, &["input_tokens", "input_token_count"]),
                num_attr(attrs, &["output_tokens", "output_token_count"]),
            ) {
                (Some(i), Some(o)) => format!(", {} in, {} out", i as i64, o as i64),
                _ => String::new(),
            };
            let cost = num_attr(attrs, &["cost_usd"])
                .map(|c| format!(", ${c:.4}"))
                .unwrap_or_default();
            ("model.request".into(), format!("{model}{tokens}{cost}{ms}"))
        }
        "api_error" => {
            let error = str_attr(attrs, &["error", "error.message"]).unwrap_or("error");
            ("model.error".into(), format!("{model}: {error}"))
        }
        "user_prompt" => {
            let length = num_attr(attrs, &["prompt_length"])
                .map(|l| format!(" ({} characters)", l as i64))
                .unwrap_or_default();
            ("prompt".into(), format!("Prompt sent{length}"))
        }
        "conversation_starts" => (
            "session.start".into(),
            format!("Conversation started on {model}"),
        ),
        "assistant_response" => {
            let length = num_attr(attrs, &["response_length"])
                .map(|l| format!(" ({} characters)", l as i64))
                .unwrap_or_default();
            ("message".into(), format!("Reply{length}"))
        }
        other => (format!("otel.{other}"), other.to_string()),
    }
}

/// The activity event for one record, or `None` for noise. `received_at` is
/// used when the record has no time.
pub fn to_activity(
    log: &OtelLog,
    chat_id: &str,
    harness: &str,
    log_prompts: bool,
    received_at: i64,
) -> Option<ActivityEvent> {
    let name = short_name(&log.event_name);
    if name.is_empty() || is_noise(name) {
        return None;
    }
    let mut attributes = Map::new();
    let mut size = 0usize;
    let mut dropped = 0usize;
    for (key, value) in &log.attributes {
        if is_identity(key) || (!log_prompts && is_prompt_text(key)) {
            continue;
        }
        let value = clipped(value);
        let cost = key.len() + value.to_string().len();
        if size.saturating_add(cost) > MAX_DETAIL_BYTES {
            dropped += 1;
            continue;
        }
        size += cost;
        attributes.insert(key.clone(), value);
    }
    let (kind, summary) = describe(name, &attributes);
    let mut detail = Map::new();
    detail.insert("event".into(), Value::String(clip(&log.event_name, 200)));
    detail.insert("trust".into(), Value::String("harness".into()));
    detail.insert("attributes".into(), Value::Object(attributes));
    if dropped > 0 {
        detail.insert("droppedAttributes".into(), Value::from(dropped));
    }
    if log.time_ms > 0 {
        detail.insert("harnessAt".into(), Value::from(log.time_ms));
    }
    if let Some(service) = log.resource.get("service.name").and_then(Value::as_str) {
        detail.insert("service".into(), Value::String(clip(service, 200)));
    }
    Some(ActivityEvent {
        id: format!("otel-{}", uuid::Uuid::new_v4().simple()),
        // The receive time, never the harness's own: a back-dated record
        // could otherwise sit before a real action and change what the
        // check sees (security review S4). The harness's time is kept.
        at: received_at,
        chat_id: chat_id.to_string(),
        harness: harness.to_string(),
        source: ActivitySource::Otel,
        kind,
        summary: one_line(&summary),
        detail: Value::Object(detail),
        pid: None,
        approval_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn log(name: &str, attrs: Value) -> OtelLog {
        OtelLog {
            time_ms: 10,
            event_name: name.into(),
            body: None,
            attributes: attrs.as_object().unwrap().clone(),
            resource: Map::new(),
        }
    }

    #[test]
    fn tool_result_from_claude_code() {
        let e = to_activity(
            &log("tool_result", json!({ "tool_name": "Bash", "success": "true", "duration_ms": 12.4, "tool_parameters": "{\"bash_command\":\"ls\"}" })),
            "chat-1",
            "claude",
            false,
            99,
        )
        .unwrap();
        assert_eq!(e.kind, "tool.call");
        assert_eq!(e.summary, "Bash ok, 12 ms");
        assert_eq!(e.chat_id, "chat-1");
        assert_eq!(e.harness, "claude");
        assert_eq!(e.source, ActivitySource::Otel);
        // The receive time; the record's own time is kept apart (S4).
        assert_eq!(e.at, 99);
        assert_eq!(e.detail["harnessAt"], 10);
        assert_eq!(e.detail["trust"], "harness");
        assert_eq!(
            e.detail["attributes"]["tool_parameters"],
            "{\"bash_command\":\"ls\"}"
        );
    }

    #[test]
    fn codex_names_lose_their_prefix() {
        let e = to_activity(
            &log(
                "codex.tool_decision",
                json!({ "tool_name": "shell", "decision": "approved", "source": "user" }),
            ),
            "c",
            "codex",
            false,
            0,
        )
        .unwrap();
        assert_eq!(e.kind, "approval");
        assert_eq!(e.summary, "shell: approved (user)");
        assert_eq!(e.detail["event"], "codex.tool_decision");
    }

    #[test]
    fn prompt_text_dropped_unless_switched_on() {
        let l = log(
            "user_prompt",
            json!({ "prompt": "my secret plan", "prompt_length": 14 }),
        );
        let off = to_activity(&l, "c", "claude", false, 0).unwrap();
        assert!(off.detail["attributes"].get("prompt").is_none());
        assert_eq!(off.summary, "Prompt sent (14 characters)");
        let on = to_activity(&l, "c", "claude", true, 0).unwrap();
        assert_eq!(on.detail["attributes"]["prompt"], "my secret plan");
    }

    #[test]
    fn identity_attributes_never_kept() {
        let e = to_activity(&log("api_request", json!({ "user.email": "a@b", "organization.id": "o", "model": "m", "input_tokens": 3, "output_tokens": 4, "cost_usd": 0.01 })), "c", "claude", true, 0).unwrap();
        let attrs = e.detail["attributes"].as_object().unwrap();
        assert!(!attrs.contains_key("user.email"));
        assert!(!attrs.contains_key("organization.id"));
        assert_eq!(e.summary, "m, 3 in, 4 out, $0.0100");
    }

    #[test]
    fn noise_dropped_and_unknown_kept() {
        assert!(to_activity(&log("codex.sse_event", json!({})), "c", "codex", false, 0).is_none());
        assert!(to_activity(&log("", json!({})), "c", "codex", false, 0).is_none());
        let e = to_activity(
            &log("codex.something_new", json!({})),
            "c",
            "codex",
            false,
            5,
        )
        .unwrap();
        assert_eq!(e.kind, "otel.something_new");
    }

    #[test]
    fn long_strings_clipped_and_detail_capped() {
        let big = "x".repeat(10_000);
        let mut attrs = Map::new();
        for i in 0..40 {
            attrs.insert(format!("k{i}"), Value::String(big.clone()));
        }
        let e = to_activity(
            &log("tool_result", Value::Object(attrs)),
            "c",
            "codex",
            false,
            0,
        )
        .unwrap();
        let kept = e.detail["attributes"].as_object().unwrap();
        assert!(kept
            .values()
            .all(|v| v.as_str().unwrap().chars().count() <= MAX_STRING_CHARS + 1));
        assert!(e.detail.to_string().len() < MAX_DETAIL_BYTES + 4096);
        assert!(e.detail["droppedAttributes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn summary_is_one_plain_line() {
        let e = to_activity(
            &log(
                "api_error",
                json!({ "model": "m", "error": "bad\nthing\u{1b}[31m" }),
            ),
            "c",
            "claude",
            false,
            0,
        )
        .unwrap();
        assert!(!e.summary.contains('\n'));
        assert!(!e.summary.contains('\u{1b}'));
    }

    fn fixture_events(body: &[u8], harness: &str) -> Vec<ActivityEvent> {
        let decoded = crate::otel_receiver::decode::decode_json(body).unwrap();
        decoded
            .logs
            .iter()
            .filter_map(|l| to_activity(l, "chat", harness, false, 1))
            .collect()
    }

    #[test]
    fn claude_code_2_1_302_payload() {
        // Saved by tests/cli-contract/checks/otel.mjs from the real CLI.
        let events = fixture_events(
            include_bytes!("fixtures/claude-2.1.302-logs.json"),
            "claude",
        );
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
        assert!(kinds.contains(&"tool.call"));
        assert!(kinds.contains(&"approval"));
        assert!(kinds.contains(&"model.request"));
        assert!(kinds.contains(&"prompt"));
        assert!(kinds.contains(&"message"));
        let tool = events.iter().find(|e| e.kind == "tool.call").unwrap();
        assert_eq!(tool.summary, "Bash ok, 90 ms");
        assert_eq!(
            tool.detail["attributes"]["tool_use_id"],
            "toolu_1b45099a6ab049728f96"
        );
        assert!(tool.detail["harnessAt"].as_i64().unwrap() > 1_700_000_000_000);
        for e in &events {
            let attrs = e.detail["attributes"].as_object().unwrap();
            assert!(!attrs.contains_key("user.id"), "{}", e.kind);
            assert!(
                !attrs.contains_key("prompt")
                    && !attrs.contains_key("prompt_text")
                    && !attrs.contains_key("response")
            );
        }
    }

    #[test]
    fn codex_0_161_0_payload() {
        let events = fixture_events(include_bytes!("fixtures/codex-0.161.0-logs.json"), "codex");
        assert!(events
            .iter()
            .all(|e| !e.detail["event"].as_str().unwrap().contains("sse_event")));
        assert!(events
            .iter()
            .all(|e| e.kind != "otel.startup_phase" && e.kind != "otel.turn_ttft"));
        let tool = events.iter().find(|e| e.kind == "tool.call").unwrap();
        assert_eq!(tool.summary, "exec_command ok, 63 ms");
        assert_eq!(tool.detail["attributes"]["call_id"], "otel1");
        // Codex writes timeUnixNano 0; the observed time stands in.
        assert!(tool.detail["harnessAt"].as_i64().unwrap() > 1_700_000_000_000);
        let decision = events.iter().find(|e| e.kind == "approval").unwrap();
        assert_eq!(decision.summary, "exec_command: approved (Config)");
        assert!(events.iter().any(|e| e.kind == "session.start"));
        assert!(events
            .iter()
            .all(|e| e.detail["attributes"].get("prompt").is_none()));
    }

    #[test]
    fn time_falls_back_to_receive_time() {
        let mut l = log("tool_result", json!({}));
        l.time_ms = 0;
        assert_eq!(to_activity(&l, "c", "claude", false, 77).unwrap().at, 77);
    }
}
