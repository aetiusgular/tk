//! Best-effort redaction of secrets in command lines before they leave the
//! feed. The reading of `contracts/docker-args.md` section 2: the value of a
//! `key=value` (or `--key value`) whose key matches `SECRET_LIKE`, plus
//! well-known token shapes, plus 64-character lower-case hex runs (the shape
//! of tk's own tokens, `tk_core::token`). Best effort: a secret in any other
//! shape passes. The token shapes are well-known prefixes, not read in each
//! vendor's docs this session (I).

pub const REDACTED: &str = "[redacted]";

/// `SECRET_LIKE` from `contracts/docker-args.md` (the fork's
/// `dockerArgs.ts:38`), plus `api-key`, `authorization` and `credential`.
/// Not a bare `auth`: it would take `--author`.
fn secret_like(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "passwd",
        "apikey",
        "api_key",
        "api-key",
        "private_key",
        "authorization",
        "credential",
    ]
    .iter()
    .any(|w| k.contains(w))
}

/// Prefixes after which the rest of the word is a secret.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "sk-ant-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
];

/// Redacts one argv list. A flag whose name is secret-like takes its next
/// argument with it.
pub fn argv(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut next_is_secret = false;
    for a in args {
        if next_is_secret {
            out.push(REDACTED.to_string());
            next_is_secret = false;
            continue;
        }
        if let Some(name) = a.strip_prefix('-') {
            let name = name.trim_start_matches('-');
            if !name.contains('=') && !name.is_empty() && secret_like(name) {
                out.push(a.clone());
                next_is_secret = true;
                continue;
            }
        }
        out.push(text(a));
    }
    out
}

/// Redacts secrets inside one string.
pub fn text(s: &str) -> String {
    // `key=value` or `key: value` with a secret-like key: the whole value.
    if let Some((key, _)) = s.split_once('=') {
        let bare = key.trim_start_matches('-');
        if !bare.is_empty() && !bare.contains(' ') && secret_like(bare) {
            return format!("{key}={REDACTED}");
        }
    }
    if let Some((key, _)) = s.split_once(':') {
        let bare = key.trim();
        if secret_like(bare) && !bare.contains(' ') || bare.eq_ignore_ascii_case("authorization") {
            return format!("{key}: {REDACTED}");
        }
    }
    let words: Vec<String> = s
        .split(' ')
        .map(|w| {
            if secret_word(w) {
                REDACTED.to_string()
            } else {
                w.to_string()
            }
        })
        .collect();
    let joined = words.join(" ");
    redact_after_bearer(&joined)
}

fn secret_word(w: &str) -> bool {
    let w = w.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';'));
    if TOKEN_PREFIXES
        .iter()
        .any(|p| w.starts_with(p) && w.len() > p.len() + 8)
    {
        return true;
    }
    // AWS access key id: AKIA + 16 upper-case letters or digits.
    if w.len() == 20
        && w.starts_with("AKIA")
        && w[4..]
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return true;
    }
    // OpenAI-style `sk-` keys: `sk-` then at least 20 word characters.
    if let Some(rest) = w.strip_prefix("sk-") {
        if rest.len() >= 20
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return true;
        }
    }
    // tk's own token shape, anywhere in the word.
    hex64_inside(w)
}

fn hex64_inside(w: &str) -> bool {
    let mut run = 0;
    for b in w.bytes() {
        if b.is_ascii_digit() || (b'a'..=b'f').contains(&b) {
            run += 1;
            if run >= 64 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

fn redact_after_bearer(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    match lower.find("bearer ") {
        Some(i) => format!("{}{}", &s[..i + 7], REDACTED),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_fixture_token_and_flag_forms() {
        // From tests/fixtures/scenario1: `/bin/true --token ghp_aaaa...`.
        let out = argv(&v(&[
            "/bin/true",
            "--token",
            "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ]));
        assert_eq!(out, v(&["/bin/true", "--token", REDACTED]));
        let out = argv(&v(&[
            "x",
            "--api-key=sk-test-0123456789abcdef",
            "API_KEY=abc",
        ]));
        assert_eq!(out, v(&["x", "--api-key=[redacted]", "API_KEY=[redacted]"]));
    }

    #[test]
    fn headers_and_shapes_inside_text() {
        assert_eq!(
            text("Authorization: Bearer abc.def"),
            "Authorization: [redacted]"
        );
        assert_eq!(
            text("curl -H x: Bearer abc"),
            "curl -H x: Bearer [redacted]"
        );
        assert_eq!(
            text("echo AKIAABCDEFGHIJKLMNOP done"),
            "echo [redacted] done"
        );
        let tok = "a".repeat(64);
        // `TK` is not secret-like; the 64-hex run is, so the word goes.
        assert_eq!(text(&format!("echo TK={tok}")), "echo [redacted]");
    }

    #[test]
    fn ordinary_arguments_pass() {
        let a = v(&["git", "commit", "-m", "fix the token parser", "--author=me"]);
        assert_eq!(argv(&a), a);
        assert_eq!(
            text("sha=0123456789abcdef0123456789abcdef01234567"),
            "sha=0123456789abcdef0123456789abcdef01234567"
        );
    }
}
