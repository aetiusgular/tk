//! sbx's network log into the feed. `sbx policy log [SANDBOX] --json` has no
//! follow flag (https://docs.docker.com/reference/cli/sbx/policy/log/), so
//! `tk run` polls it and posts what is new: a host seen for the first time,
//! or a count that went up.
//!
//! The JSON shape is not documented (NF). The older Docker Desktop page lists
//! the fields Sandbox, Host, Rule, Last Seen, Count, in "Blocked requests"
//! and "Allowed requests" sections
//! (https://docs.docker.com/ai/sandboxes/network-policies). So the parser
//! reads keys without regard to case or separators, takes an array or an
//! object of arrays, and reads "blocked" from a boolean, a decision string,
//! or the section's name. The box test records one real output and pins it
//! as a fixture.

use std::collections::HashMap;

use serde_json::{json, Value};
use tk_core::event::{Event, Source};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub host: String,
    pub blocked: bool,
    pub rule: String,
    pub count: u64,
    pub sandbox: String,
}

fn norm(k: &str) -> String {
    k.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn field<'a>(obj: &'a serde_json::Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    obj.iter()
        .find(|(k, _)| names.contains(&norm(k).as_str()))
        .map(|(_, v)| v)
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn entry(v: &Value, section_blocked: Option<bool>) -> Option<Entry> {
    let o = v.as_object()?;
    let host = text(field(
        o,
        &["host", "destination", "domain", "target", "hostport"],
    ));
    if host.is_empty() {
        return None;
    }
    let blocked = match field(o, &["blocked", "denied"]) {
        Some(Value::Bool(b)) => *b,
        _ => {
            let d = text(field(
                o,
                &["action", "decision", "status", "result", "verdict", "type"],
            ))
            .to_ascii_lowercase();
            if d.contains("block") || d.contains("deny") || d.contains("denied") {
                true
            } else if d.contains("allow") {
                false
            } else {
                // Unknown: count it as blocked, so it is shown, not hidden.
                section_blocked.unwrap_or(true)
            }
        }
    };
    let count = match field(o, &["count", "requests", "hits", "requestcount"]) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(1),
        Some(Value::String(s)) => s.parse().unwrap_or(1),
        _ => 1,
    };
    Some(Entry {
        host: host.chars().take(300).collect(),
        blocked,
        rule: text(field(o, &["rule", "matchedrule", "policy", "matchingrule"]))
            .chars()
            .take(300)
            .collect(),
        count,
        sandbox: text(field(o, &["sandbox", "sandboxname", "name"]))
            .chars()
            .take(100)
            .collect(),
    })
}

/// Reads one `--json` output. Unknown shapes give no entries, never a panic.
pub fn parse(output: &str) -> Vec<Entry> {
    let Ok(v) = serde_json::from_str::<Value>(output) else {
        // One JSON object per line is the other likely shape.
        return output
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| entry(&v, None))
            .collect();
    };
    let mut out = Vec::new();
    match &v {
        Value::Array(a) => out.extend(a.iter().filter_map(|e| entry(e, None))),
        Value::Object(o) => {
            if let Some(e) = entry(&v, None) {
                out.push(e);
            }
            for (k, val) in o {
                let n = norm(k);
                let section = if n.contains("block") || n.contains("deny") {
                    Some(true)
                } else if n.contains("allow") {
                    Some(false)
                } else {
                    None
                };
                if let Value::Array(a) = val {
                    out.extend(a.iter().filter_map(|e| entry(e, section)));
                }
            }
        }
        _ => {}
    }
    out
}

/// Remembers counts and returns what is new since the last call.
#[derive(Default)]
pub struct Differ {
    seen: HashMap<(String, bool, String), u64>,
}

impl Differ {
    /// Takes the state before the harness starts: what is there already is
    /// not this run's (a re-attached sandbox keeps its log).
    pub fn baseline(&mut self, entries: &[Entry]) {
        for e in entries {
            self.seen
                .insert((e.host.clone(), e.blocked, e.rule.clone()), e.count);
        }
    }

    pub fn new_entries(&mut self, entries: &[Entry]) -> Vec<Entry> {
        let mut out = Vec::new();
        for e in entries {
            let key = (e.host.clone(), e.blocked, e.rule.clone());
            let before = self.seen.get(&key).copied();
            if before.is_none_or(|b| e.count > b) {
                out.push(e.clone());
            }
            self.seen.insert(key, e.count.max(before.unwrap_or(0)));
        }
        out
    }
}

/// One entry as a daemon event: `net.connect` from `run`, with the host the
/// daemon's checks read (`detail.host`).
pub fn to_event(e: &Entry, chat: &str, harness: &str, now_ms: i64) -> Event {
    let verdict = if e.blocked { "blocked" } else { "allowed" };
    let id_key = format!("{}|{}|{}", e.host, e.blocked, e.rule);
    let digest = tk_core::token::hash(&id_key);
    Event {
        id: format!("sbx-{chat}-{}-{}", &digest[..16], e.count),
        at: now_ms,
        chat_id: chat.to_string(),
        harness: harness.to_string(),
        source: Source::Run,
        kind: "net.connect".into(),
        summary: format!("sbx {verdict} {}", e.host)
            .chars()
            .take(200)
            .collect(),
        detail: json!({
            "via": "sbx policy log",
            "host": e.host,
            "blocked": e.blocked,
            "rule": e.rule,
            "count": e.count,
            "sandbox": e.sandbox,
            "trust": "boundary",
        }),
        pid: None,
        approval_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_of_sections_like_the_desktop_page() {
        let out = r#"{"blocked":[{"Sandbox":"tk-c","Host":"evil.example:443","Rule":"<default policy>","Last Seen":"now","Count":2}],
                     "allowed":[{"Sandbox":"tk-c","Host":"api.anthropic.com:443","Rule":"api.anthropic.com","Count":5}]}"#;
        let e = parse(out);
        assert_eq!(e.len(), 2);
        let by = |h: &str| e.iter().find(|x| x.host == h).unwrap();
        let evil = by("evil.example:443");
        assert!(evil.blocked && evil.count == 2 && evil.sandbox == "tk-c");
        let api = by("api.anthropic.com:443");
        assert!(!api.blocked && api.rule == "api.anthropic.com" && api.count == 5);
    }

    #[test]
    fn an_array_with_decisions_and_json_lines() {
        let out = r#"[{"host":"a:443","action":"deny"},{"host":"b:443","decision":"allowed","count":"3"}]"#;
        let e = parse(out);
        assert_eq!((e[0].blocked, e[1].blocked, e[1].count), (true, false, 3));
        let lines = "{\"host\":\"c:80\",\"blocked\":true}\n{\"host\":\"d:80\",\"blocked\":false}\n";
        assert_eq!(parse(lines).len(), 2);
    }

    #[test]
    fn junk_gives_nothing_and_an_unknown_verdict_is_shown_as_blocked() {
        assert!(parse("").is_empty());
        assert!(parse("Error: no such sandbox").is_empty());
        assert!(parse("[1, \"x\", {\"nohost\":1}]").is_empty());
        assert!(parse(r#"[{"host":"z:1","action":"???"}]"#)[0].blocked);
    }

    #[test]
    fn only_new_hosts_and_higher_counts_are_posted() {
        let mut d = Differ::default();
        let e = |h: &str, c| Entry {
            host: h.into(),
            blocked: true,
            rule: "r".into(),
            count: c,
            sandbox: "s".into(),
        };
        d.baseline(&[e("old:443", 4)]);
        assert!(d.new_entries(&[e("old:443", 4)]).is_empty());
        assert_eq!(d.new_entries(&[e("old:443", 5), e("new:443", 1)]).len(), 2);
        assert!(d.new_entries(&[e("new:443", 1)]).is_empty());
    }

    #[test]
    fn the_event_is_net_connect_from_run_and_ids_follow_the_count() {
        let e = Entry {
            host: "evil:443".into(),
            blocked: true,
            rule: "".into(),
            count: 2,
            sandbox: "tk-c".into(),
        };
        let ev = to_event(&e, "c", "pi", 9);
        assert_eq!(ev.kind, "net.connect");
        assert_eq!(ev.source, Source::Run);
        assert_eq!(ev.detail["host"], "evil:443");
        assert_eq!(ev.summary, "sbx blocked evil:443");
        let mut e3 = e.clone();
        e3.count = 3;
        assert_ne!(to_event(&e3, "c", "pi", 9).id, ev.id);
    }
}
