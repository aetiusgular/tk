//! Redaction for the lines `tk connect` prints from a config file it edits
//! (integration w1 fix pass, Mac test finding M3). The dry run's diff shows
//! the lines around a change, and a removal shows the removed lines; both
//! come from the user's own file, which can hold keys in plain text
//! (`OPENAI_API_KEY = "sk-..."` in `~/.codex/config.toml` on the Mac).
//! Every printed line goes through `line`, added lines too.
//!
//! What it hides, in this order:
//! 1. the value of any `key = value` or `"key": value` pair, whole line or
//!    inline, whose key is secret-like (`tk_audit_linux::redact::secret_like`,
//!    the `SECRET_LIKE` list of `contracts/docker-args.md` plus three);
//! 2. the credential after `Bearer ` or `Basic `;
//! 3. the password in a URL's `user:password@`;
//! 4. a word in a well-known token shape (`secret_word`: `ghp_`, `sk-`,
//!    `AKIA...`, 64 hex);
//! 5. any run of 32 or more of `[A-Za-z0-9_+=-]` holding both a letter and a
//!    digit, which is the shape of most opaque keys.
//!
//! Best effort, as every redactor in tk: a short secret under a key that is
//! not secret-like (`FOO = "abc123"`) still prints. Rule 5 also hides long
//! ids (a UUID, a hash) in context lines; those lines are only there to
//! orient, and the file's own hash is printed outside the diff.

use tk_audit_linux::redact::{secret_like, secret_word};

pub const REDACTED: &str = "[redacted]";

/// One printed line, redacted.
pub fn line(s: &str) -> String {
    let s = pairs(s);
    let s = after_scheme(&s, "bearer ");
    let s = after_scheme(&s, "basic ");
    let s = userinfo(&s);
    runs(&s)
}

fn key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Rule 1: finds `<key><ws>(:|=)<ws><value>` anywhere in the line, the key
/// bare or quoted, and replaces a secret-like key's value.
fn pairs(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        // A key: a quoted string or a run of key characters.
        let (key, key_end) = if c[i] == '"' || c[i] == '\'' {
            match quoted_end(&c, i) {
                Some(e) => (c[i + 1..e].iter().collect::<String>(), e + 1),
                None => {
                    out.extend(&c[i..]);
                    break;
                }
            }
        } else if key_char(c[i]) && (i == 0 || !key_char(c[i - 1])) {
            let mut e = i;
            while e < c.len() && key_char(c[e]) {
                e += 1;
            }
            (c[i..e].iter().collect::<String>(), e)
        } else {
            out.push(c[i]);
            i += 1;
            continue;
        };
        let mut j = key_end;
        while j < c.len() && c[j] == ' ' {
            j += 1;
        }
        let is_pair = j < c.len() && (c[j] == ':' || c[j] == '=');
        if !is_pair || !secret_like(&key) || names_a_variable(&key) {
            out.extend(&c[i..key_end]);
            i = key_end;
            continue;
        }
        let mut v = j + 1;
        // `::` or `==` is not a separator (a Rust path, a comparison).
        if v < c.len() && c[v] == c[j] {
            out.extend(&c[i..key_end]);
            i = key_end;
            continue;
        }
        while v < c.len() && c[v] == ' ' {
            v += 1;
        }
        out.extend(&c[i..v]);
        if v >= c.len() {
            break;
        }
        match c[v] {
            // An object or array opens here; its members are pairs of their
            // own and are checked as the scan reaches them.
            '{' | '[' => {
                i = v;
            }
            '"' | '\'' => match quoted_end(&c, v) {
                Some(e) if is_reference(&c[v + 1..e].iter().collect::<String>()) => {
                    out.extend(&c[v..=e]);
                    i = e + 1;
                }
                Some(e) => {
                    out.push(c[v]);
                    out.push_str(REDACTED);
                    out.push(c[e]);
                    i = e + 1;
                }
                None => {
                    out.push(c[v]);
                    out.push_str(REDACTED);
                    break;
                }
            },
            _ => {
                let mut e = v;
                while e < c.len() && !matches!(c[e], ',' | '}' | ']' | ' ' | '#') {
                    e += 1;
                }
                if e > v {
                    out.push_str(REDACTED);
                }
                i = e;
            }
        }
    }
    out
}

/// A key whose value is the name of a variable, not its value: Codex's
/// `bearer_token_env_var`, `env_vars`. The name is kept.
fn names_a_variable(key: &str) -> bool {
    let k = key.to_ascii_lowercase().replace(['_', '-'], "");
    k.ends_with("envvar") || k.ends_with("envvars")
}

/// A value that names a variable the harness reads at start, not a secret:
/// `${VAR}` (Claude Code, Codex, Pi), `{env:VAR}` (OpenCode), optionally
/// after an auth scheme (`Bearer ${VAR}`). tk writes these itself; hiding
/// them would hide what the entry does.
fn is_reference(v: &str) -> bool {
    let v = match v.split_once(' ') {
        Some((scheme, rest)) if scheme.chars().all(|c| c.is_ascii_alphabetic()) => rest,
        _ => v,
    };
    let name = v
        .strip_prefix("${")
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| v.strip_prefix("{env:").and_then(|r| r.strip_suffix('}')));
    name.is_some_and(|n| {
        n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The index of the quote that closes the one at `start`, backslash escapes
/// skipped in double quotes (TOML literal strings have none).
fn quoted_end(c: &[char], start: usize) -> Option<usize> {
    let q = c[start];
    let mut k = start + 1;
    while k < c.len() {
        if q == '"' && c[k] == '\\' {
            k += 2;
            continue;
        }
        if c[k] == q {
            return Some(k);
        }
        k += 1;
    }
    None
}

/// Rule 2: the credential after an HTTP auth scheme, to the next quote,
/// space or comma.
fn after_scheme(s: &str, scheme: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut from = 0;
    while let Some(k) = lower[from..].find(scheme) {
        let start = from + k + scheme.len();
        out.push_str(&s[from..start]);
        let rest = &s[start..];
        let mut end = rest
            .find(|ch: char| matches!(ch, '"' | '\'' | ' ' | ',' | '}' | ']'))
            .unwrap_or(rest.len());
        // `${VAR}` and `{env:VAR}` end at their own brace.
        if rest.starts_with("${") || rest.starts_with("{env:") {
            if let Some(close) = rest.find('}') {
                end = close + 1;
            }
        }
        if end > 0 && !rest[..end].starts_with(REDACTED) && !is_reference(&rest[..end]) {
            out.push_str(REDACTED);
        } else {
            out.push_str(&rest[..end]);
        }
        from = start + end;
    }
    out.push_str(&s[from..]);
    out
}

/// Rule 3: `scheme://user:password@host` keeps the user.
fn userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut from = 0;
    while let Some(k) = s[from..].find("://") {
        let auth_start = from + k + 3;
        out.push_str(&s[from..auth_start]);
        let rest = &s[auth_start..];
        let auth_end = rest
            .find(|ch: char| matches!(ch, '/' | '"' | '\'' | ' ' | '?' | '#'))
            .unwrap_or(rest.len());
        let auth = &rest[..auth_end];
        match (auth.rfind('@'), auth.find(':')) {
            (Some(at), Some(colon)) if colon < at => {
                out.push_str(&auth[..=colon]);
                out.push_str(REDACTED);
                out.push_str(&auth[at..]);
            }
            _ => out.push_str(auth),
        }
        from = auth_start + auth_end;
    }
    out.push_str(&s[from..]);
    out
}

fn run_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=')
}

/// Rules 4 and 5, over runs of token characters.
fn runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            let opaque = word.len() >= 32
                && word.bytes().any(|b| b.is_ascii_alphabetic())
                && word.bytes().any(|b| b.is_ascii_digit());
            if opaque || secret_word(word) {
                out.push_str(REDACTED);
            } else {
                out.push_str(word);
            }
            word.clear();
        }
    };
    for ch in s.chars() {
        if run_char(ch) {
            word.push(ch);
        } else {
            flush(&mut word, &mut out);
            out.push(ch);
        }
    }
    flush(&mut word, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::line;

    #[test]
    fn toml_and_json_pairs_with_a_secret_like_key() {
        assert_eq!(
            line("OPENAI_API_KEY = \"abc\""),
            "OPENAI_API_KEY = \"[redacted]\""
        );
        assert_eq!(line("password='p w'"), "password='[redacted]'");
        assert_eq!(
            line("    \"GITHUB_TOKEN\": \"x\","),
            "    \"GITHUB_TOKEN\": \"[redacted]\","
        );
        assert_eq!(line("api_key: plain"), "api_key: [redacted]");
        assert_eq!(
            line("env = { API_TOKEN = \"x\", MODE = \"fast\" }"),
            "env = { API_TOKEN = \"[redacted]\", MODE = \"fast\" }"
        );
        assert_eq!(
            line("\"secrets\": {"),
            "\"secrets\": {",
            "an opening brace has nothing to hide"
        );
        // An escaped quote inside the value does not end it early.
        assert_eq!(
            line(r#""token": "a\"b", "n": 1"#),
            r#""token": "[redacted]", "n": 1"#
        );
    }

    #[test]
    fn variable_references_are_not_secrets() {
        for l in [
            "        \"CONTEXT7_API_KEY\": \"${CONTEXT7_API_KEY}\"",
            "        \"Authorization\": \"Bearer ${TK_GATEWAY_TOKEN}\"",
            "      \"x-api-key\": \"{env:EXA_API_KEY}\",",
            "bearer_token_env_var = \"TK_GATEWAY_TOKEN\"",
        ] {
            assert_eq!(line(l), l);
        }
        // Text around a reference is not a reference.
        assert_eq!(
            line("\"token\": \"abc${X}\""),
            "\"token\": \"[redacted]\""
        );
    }

    #[test]
    fn ordinary_lines_are_kept() {
        for l in [
            "[mcp_servers.exa]",
            "command = \"docker\"",
            "args = [\"mcp\", \"gateway\", \"run\", \"--profile\", \"tk_exa\"]",
            "    \"url\": \"https://mcp.exa.ai/mcp\",",
            "# my settings",
            "model = \"claude-haiku-5-5-20261001\"",
            "\"tokenizer\": \"cl100k\"",
        ] {
            let want = if l.contains("tokenizer") {
                // `tokenizer` holds `token`: the list is a substring test,
                // as SECRET_LIKE is in the fork. Over-hiding is the safe side.
                "\"tokenizer\": \"[redacted]\"".to_string()
            } else {
                l.to_string()
            };
            assert_eq!(line(l), want);
        }
    }

    #[test]
    fn schemes_userinfo_and_shapes() {
        assert_eq!(
            line("\"headers\": {\"Authorization\": \"Bearer abc.def\"}"),
            "\"headers\": {\"Authorization\": \"[redacted]\"}"
        );
        assert_eq!(
            line("\"X-Auth\": \"Bearer abc.def\""),
            "\"X-Auth\": \"Bearer [redacted]\""
        );
        assert_eq!(
            line("url = \"postgresql://me:hunter2@db:5432/x\""),
            "url = \"postgresql://me:[redacted]@db:5432/x\""
        );
        assert_eq!(
            line("url = \"https://host.example/p:q@x\""),
            "url = \"https://host.example/p:q@x\"",
            "an @ after the path is not userinfo"
        );
        assert_eq!(
            line("\"args\": [\"--x\", \"ghp_abcdefghijklmnopqrstuvwxyz0123\"]"),
            "\"args\": [\"--x\", \"[redacted]\"]"
        );
        assert_eq!(
            line("X_CUSTOM = \"Zq3vX9mK2pL8rT4wY7nB1cF6hJ0sD5gA\""),
            "X_CUSTOM = \"[redacted]\""
        );
        let words: String = "abcdefghij".repeat(4);
        assert_eq!(line(&words), words, "letters only is not a key shape");
    }
}
