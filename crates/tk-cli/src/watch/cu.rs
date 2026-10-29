//! `tk cu`: computer use's controls on the daemon's `cu` op (toolkit plan
//! T1.5; lane K's request 3, added by integration w2). The fork had them in
//! its menu bar, which went with Tauri.
//!
//! - `tk cu status`: the helper's state, the chats that run it, open cards.
//! - `tk cu stop --chat <id>`: ends one chat's grants, cards and runtime.
//! - `tk cu stop --all`: every chat.
//! - `tk cu revoke --chat <id> --app <bundle id>`: takes one app back.
//!
//! Daemon scope only, as `watch` connects (`conn.rs`): a chat token, which
//! the agent holds too (case 66), cannot reach these.

use serde_json::{json, Value};

use super::conn::Conn;
use super::text::clean;
use super::Args;

pub const FLAGS: &[super::Flag] = &[
    ("--chat", true),
    ("--app", true),
    ("--all", false),
    ("--json", false),
    ("--socket", true),
];

pub const USAGE: &str = "usage: tk cu status [--json] [--socket <path>]
       tk cu stop (--chat <id> | --all) [--socket <path>]
       tk cu revoke --chat <id> --app <bundle id> [--socket <path>]";

/// The request line for a command, or a usage error.
pub fn request(command: &str, args: &Args) -> Result<Value, String> {
    let chat = args.value("--chat");
    let app = args.value("--app");
    match command {
        "status" if chat.is_none() && app.is_none() && !args.flag("--all") => {
            Ok(json!({"op": "cu", "action": "status"}))
        }
        "stop" if args.flag("--all") && chat.is_none() && app.is_none() => {
            Ok(json!({"op": "cu", "action": "stop-all"}))
        }
        "stop" if !args.flag("--all") && app.is_none() => match chat {
            Some(c) => Ok(json!({"op": "cu", "action": "stop", "chat": c})),
            None => Err("tk cu stop needs --chat <id> or --all".into()),
        },
        "revoke" if !args.flag("--all") => match (chat, app) {
            (Some(c), Some(a)) => Ok(json!({"op": "cu", "action": "revoke", "chat": c, "app": a})),
            _ => Err("tk cu revoke needs --chat <id> and --app <bundle id>".into()),
        },
        "status" | "stop" | "revoke" => {
            Err(format!("tk cu {command}: those flags do not go together"))
        }
        other => Err(format!("unknown tk cu command {other:?}")),
    }
}

pub fn main(args: &[String]) -> u8 {
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return 2;
    };
    if command == "-h" || command == "--help" || command == "help" {
        println!("{USAGE}");
        return 0;
    }
    let parsed = match Args::parse(&args[1..], FLAGS) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return 2;
        }
    };
    let line = match request(command, &parsed) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return 2;
        }
    };
    let socket = match parsed.socket() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tk cu: {e}");
            return 2;
        }
    };
    let mut conn = match Conn::open(&socket, "tk cu") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tk cu: {e}");
            return 1;
        }
    };
    let reply = match conn.request(&line) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tk cu: {e}");
            return 1;
        }
    };
    if reply["ok"] != true {
        eprintln!(
            "tk cu: {}",
            clean(reply["error"].as_str().unwrap_or("the daemon refused"), 300)
        );
        return 1;
    }
    let report = &reply["report"];
    if parsed.flag("--json") {
        println!("{report}");
    } else {
        print!("{}", render(report));
    }
    0
}

/// The report for a terminal: one `path: value` line per field, nested
/// fields by their path (`runningChats[0].chat`), an empty list as
/// `(none)`. Every value is the daemon's text, which can hold a chat's
/// words, so each is cleaned on its own line (text::clean); cleaning the
/// rendered block escaped its newlines too (Mac test w2, MW4).
pub fn render(report: &Value) -> String {
    let mut out = String::new();
    fn walk(path: &str, v: &Value, out: &mut String, budget: &mut usize) {
        if *budget == 0 {
            return;
        }
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, child) in m {
                    let key = clean(k, 64);
                    let p = if path.is_empty() {
                        key
                    } else {
                        format!("{path}.{key}")
                    };
                    walk(&p, child, out, budget);
                }
            }
            Value::Array(a) if !a.is_empty() => {
                for (i, child) in a.iter().enumerate() {
                    walk(&format!("{path}[{i}]"), child, out, budget);
                }
            }
            Value::Object(_) | Value::Array(_) => {
                *budget -= 1;
                out.push_str(&format!("{path}: (none)\n"));
            }
            Value::String(s) => {
                *budget -= 1;
                out.push_str(&format!("{path}: {}\n", clean(s, 300)));
            }
            other => {
                *budget -= 1;
                out.push_str(&format!("{path}: {other}\n"));
            }
        }
    }
    let mut budget = 500usize;
    if report.is_null() {
        return "(no report)\n".into();
    }
    walk("", report, &mut out, &mut budget);
    if budget == 0 {
        out.push_str("… (more fields left out; --json prints all)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Result<Value, String> {
        let s: Vec<String> = v.iter().map(|x| x.to_string()).collect();
        let a = Args::parse(&s[1..], FLAGS)?;
        request(&s[0], &a)
    }

    #[test]
    fn each_command_builds_its_line_and_nothing_else_passes() {
        assert_eq!(parse(&["status"]).unwrap()["action"], "status");
        assert_eq!(parse(&["stop", "--all"]).unwrap()["action"], "stop-all");
        let stop = parse(&["stop", "--chat", "tk-c1"]).unwrap();
        assert_eq!(
            (stop["action"].as_str(), stop["chat"].as_str()),
            (Some("stop"), Some("tk-c1"))
        );
        let rev = parse(&["revoke", "--chat", "tk-c1", "--app", "com.apple.calculator"]).unwrap();
        assert_eq!(rev["app"], "com.apple.calculator");
        for bad in [
            &["stop"][..],
            &["stop", "--all", "--chat", "x"],
            &["revoke", "--chat", "x"],
            &["status", "--all"],
            &["launch"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_human_output_is_one_field_per_line_with_each_value_cleaned() {
        // Mac test w2, MW4: the whole pretty-printed JSON went through
        // `clean`, which escaped its newlines onto one line.
        let report = json!({
            "helper": "missing",
            "helperPath": "/Applications/MonoCode Computer Use.app",
            "pending": [],
            "runningChats": [{"chat": "tk-c1", "apps": ["com.apple.calculator"]}],
            "note": "two\nlines \u{1b}[2J"
        });
        let text = render(&report);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 6, "{text}");
        assert!(
            lines.iter().filter(|l| l.contains("\\u{A}")).count() == 1,
            "only the note's own newline is escaped: {text}"
        );
        assert!(lines.contains(&"helper: missing"), "{text}");
        assert!(
            lines.contains(&"helperPath: /Applications/MonoCode Computer Use.app"),
            "{text}"
        );
        assert!(lines.contains(&"pending: (none)"), "{text}");
        assert!(lines.contains(&"runningChats[0].chat: tk-c1"), "{text}");
        assert!(
            lines.contains(&"runningChats[0].apps[0]: com.apple.calculator"),
            "{text}"
        );
        let note = lines.iter().find(|l| l.starts_with("note: ")).unwrap();
        assert!(!note.contains('\x1b') && note.contains("\\u{A}"), "{note}");
    }

    #[test]
    fn no_command_is_a_usage_error_and_help_is_not() {
        assert_eq!(main(&[]), 2);
        assert_eq!(main(&["--help".to_string()]), 0);
    }
}
