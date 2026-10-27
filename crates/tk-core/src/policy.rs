//! The policy file (`~/.tk/policy.json`) and the matcher. Contract:
//! `contracts/policy.md`; schema: `contracts/policy.schema.json`; fixture:
//! `contracts/fixtures/policy/`.
//!
//! What is here: the types, `normalize_tool_name`, `fold_text` (the reject
//! list's text folding, confusables included), the reject token tables,
//! `evaluate` (the matcher, contract section 4) and `load` (the file, read
//! only when called; a bad file is the default policy with a reason).

use serde::{Deserialize, Serialize};

/// What a rule or the matcher decides. `Ask` times out to `Deny`
/// (`PolicyFile::ask_timeout_seconds`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Ask,
    Deny,
}

/// One rule. `tool` is a normalized name (`server/tool`, a bare built-in
/// name, `server/*` or `*`). `harness` and `chat` narrow it; absent means
/// any. `description_sha256` pins the approved description (case 57): a
/// mismatch turns an `allow` into `ask`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rule {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_sha256: Option<String>,
}

/// The file. `servers` lists the MCP server names the user has connected,
/// so a `{server}_{tool}` name with no `mcp` prefix can be split (contract,
/// normalization step 6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyFile {
    pub version: u32,
    #[serde(default = "default_ask_timeout")]
    pub ask_timeout_seconds: u32,
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

fn default_ask_timeout() -> u32 {
    60
}

impl Default for PolicyFile {
    fn default() -> Self {
        Self {
            version: 1,
            ask_timeout_seconds: default_ask_timeout(),
            servers: Vec::new(),
            rules: Vec::new(),
        }
    }
}

/// What the matcher is asked about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Call {
    /// The name as the harness gave it (`mcp__exa__search`, `Bash`, ...).
    pub tool: String,
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// The tool's description as the harness shows it now, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The text the reject list is run over: a command line, a file path
    /// plus content, or the serialized input.
    #[serde(default)]
    pub input_text: String,
}

/// The matcher's answer. `reason` is shown in the card or the refusal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub action: Action,
    pub normalized_tool: String,
    pub reason: String,
}

/// Normalizes a tool name the way the contract says (`policy.md`, section
/// 2). Returns `server/tool` for an MCP tool and the bare lowercased name for
/// a built-in. `servers` are the connected server names, lowercased by the
/// caller or not: they are lowercased here.
pub fn normalize_tool_name(raw: &str, servers: &[String]) -> String {
    let lowered: String = raw
        .trim()
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '_' || c == '/' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if let Some((server, tool)) = lowered.split_once('/') {
        return format!("{}/{}", strip_plugin(server), tool);
    }
    if let Some(rest) = lowered.strip_prefix("mcp__") {
        return join(split_at_double_then_single(rest));
    }
    if let Some(rest) = lowered.strip_prefix("mcp_") {
        return join(split_at_double_then_single(rest));
    }
    if let Some((server, tool)) = lowered.split_once("__") {
        return join((server, tool));
    }
    let known: Vec<String> = servers.iter().map(|s| s.to_ascii_lowercase()).collect();
    // `{server}_{tool}` (OpenCode, ToolHive vMCP): the longest known server
    // that is a prefix followed by `_` wins.
    let mut best: Option<&str> = None;
    for s in &known {
        if lowered.len() > s.len() + 1
            && lowered.starts_with(s.as_str())
            && lowered.as_bytes()[s.len()] == b'_'
            && best.is_none_or(|b| s.len() > b.len())
        {
            best = Some(s.as_str());
        }
    }
    if let Some(s) = best {
        return join((s, &lowered[s.len() + 1..]));
    }
    lowered
}

fn split_at_double_then_single(rest: &str) -> (&str, &str) {
    if let Some(p) = rest.split_once("__") {
        p
    } else if let Some(p) = rest.split_once('_') {
        p
    } else {
        ("", rest)
    }
}

fn join((server, tool): (&str, &str)) -> String {
    if server.is_empty() {
        tool.to_string()
    } else {
        format!("{}/{}", strip_plugin(server), tool)
    }
}

/// `plugin_<plugin>_<server>` (Claude Code plugin servers) becomes `<server>`.
fn strip_plugin(server: &str) -> &str {
    if let Some(rest) = server.strip_prefix("plugin_") {
        if let Some((_, s)) = rest.split_once('_') {
            if !s.is_empty() {
                return s;
            }
        }
    }
    server
}

/// Folds text before the reject list runs over it: lowercase; invisible and
/// format characters dropped; every Unicode space mapped to one ASCII space;
/// fullwidth ASCII mapped to ASCII; any other non-ASCII character whose
/// Unicode TR39 confusable prototype is one printable ASCII character mapped
/// to it, lowercased (Cyrillic `а` to `a`, `‐` to `-`). The prototypes come
/// from `unicode-security` 0.1.2, whose tables are Unicode 16.0.0's
/// `confusables.txt`. A character whose prototype is longer, or not ASCII,
/// stays as it is (contract, section 3: Latin targets only).
pub fn fold_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let c = match c {
            '\u{00AD}'
            | '\u{034F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2060}'..='\u{2064}'
            | '\u{FEFF}'
            | '\u{180E}' => continue,
            '\u{0085}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFF01 + 0x21).unwrap_or(c),
            _ => c,
        };
        for l in c.to_lowercase() {
            match ascii_prototype(l) {
                Some(a) => out.push(a.to_ascii_lowercase()),
                None => out.push(l),
            }
        }
    }
    out
}

/// The one printable ASCII character a non-ASCII character is confusable
/// with, by its TR39 skeleton, or `None`.
fn ascii_prototype(c: char) -> Option<char> {
    if c.is_ascii() {
        return None;
    }
    let mut buf = [0u8; 4];
    let mut skeleton = unicode_security::confusable_detection::skeleton(c.encode_utf8(&mut buf));
    let first = skeleton.next()?;
    if skeleton.next().is_some() || !first.is_ascii_graphic() {
        return None;
    }
    Some(first)
}

/// The fixed reject list, part 1 (contract, section 3.1): endpoint, sandbox
/// and approval keys from the AP section 5 incidents. Matched against
/// `fold_text(input_text)` for every tool before any rule. A hit is `Deny`.
pub const ENDPOINT_KEYS: &[&str] = &[
    "anthropic_base_url",
    "chatgpt_base_url",
    "dangerouslydisablesandbox",
    "approval_policy",
    "--dangerously-",
    "settings.local.json",
];

/// Part 2 (contract, section 3.2): git config that names a program or a
/// transport (rules, cases 27, 31, 32). Matched only when `git_context` is
/// true for the folded text, so a note that mentions `protocol.version` is
/// not refused. A hit is `Deny`.
pub const GIT_CONFIG_KEYS: &[&str] = &[
    "core.fsmonitor",
    "core.hookspath",
    "core.sshcommand",
    "core.pager",
    "core.editor",
    "core.askpass",
    "core.gitproxy",
    "core.attributesfile",
    "sequence.editor",
    "pager.",
    "alias.",
    "gpg.program",
    "gpg.ssh.program",
    "credential.helper",
    "include.path",
    "includeif.",
    "filter.",
    ".driver",
    ".textconv",
    "diff.external",
    "uploadpack",
    "receivepack",
    ".insteadof",
    ".pushinsteadof",
    "protocol.",
    "ext::",
    "fd::",
    "git_allow_protocol",
    "git_ssh",
    "git_proxy_command",
    "git_askpass",
    "git_external_diff",
    "git_config_parameters",
    "git_config_count",
];

/// Markers that make text a git context (contract, section 3.2): a git
/// config path, or the word `git` followed later on the same line by
/// `-c`, `config` or `--config-env`. Lane B implements the second test;
/// these are the path markers.
pub const GIT_CONTEXT_PATHS: &[&str] = &[
    ".git/",
    ".gitmodules",
    ".gitattributes",
    ".gitconfig",
    "git_config",
];

/// Shell tools by normalized name (contract, section 4). The Codex names
/// are inferred from its docs, not run; lane F verifies them.
pub const SHELL_TOOLS: &[&str] = &[
    "bash",
    "shell",
    "exec_command",
    "local_shell",
    "run_shell_command",
    "user_bash",
    "terminal",
];

/// Text the shell reads in a way a matcher cannot (case 30). An `allow`
/// never applies to a shell tool whose input contains one; the call is
/// `Ask` instead (contract, section 4).
pub const UNREADABLE_SHELL_TOKENS: &[&str] =
    &["$", "`", "!", "{", "}", "~", "(e:", "(+", "(-", "\\"];

/// True when folded text is a git context (contract, section 3.2): it names
/// a git config path, or the word `git` (not inside a longer word) is
/// followed later on the same line by `-c`, `config` or `--config-env`. A
/// line that ends in `\` continues on the next, as the shell reads it.
pub fn git_context(folded: &str) -> bool {
    if GIT_CONTEXT_PATHS.iter().any(|p| folded.contains(p)) {
        return true;
    }
    let joined = folded.replace("\\\r\n", " ").replace("\\\n", " ");
    joined.split(['\n', '\r']).any(|line| {
        let bytes = line.as_bytes();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut from = 0;
        while let Some(found) = line[from..].find("git") {
            let at = from + found;
            let end = at + 3;
            let before_ok = at == 0 || !word(bytes[at - 1]);
            let after_ok = end >= bytes.len() || !word(bytes[end]);
            if before_ok && after_ok {
                let rest = &line[end..];
                if rest.contains("-c") || rest.contains("config") {
                    return true;
                }
            }
            from = end;
        }
        false
    })
}

/// The first reject-list key the folded text holds (contract, section 3),
/// or `None`. Endpoint keys apply everywhere; git config keys only in a git
/// context.
pub fn reject_hit(folded: &str) -> Option<&'static str> {
    if let Some(key) = ENDPOINT_KEYS.iter().find(|k| folded.contains(*k)) {
        return Some(key);
    }
    if git_context(folded) {
        return GIT_CONFIG_KEYS
            .iter()
            .find(|k| folded.contains(*k))
            .copied();
    }
    None
}

/// How specific a rule's `tool` is for a normalized name: `Some(2)` exact,
/// `Some(1)` `server/*`, `Some(0)` `*`, `None` when it does not match.
fn tool_rank(pattern: &str, normalized: &str) -> Option<u8> {
    let pattern = pattern.trim().to_ascii_lowercase();
    if pattern == "*" {
        return Some(0);
    }
    if pattern == normalized {
        return Some(2);
    }
    if let Some(server) = pattern.strip_suffix("/*") {
        if let Some((s, _)) = normalized.split_once('/') {
            if s == server {
                return Some(1);
            }
        }
    }
    None
}

fn restrictiveness(action: Action) -> u8 {
    match action {
        Action::Allow => 0,
        Action::Ask => 1,
        Action::Deny => 2,
    }
}

/// SHA-256 hex of a tool description, the form `descriptionSha256` pins.
pub fn description_sha256(description: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(description.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn rule_reason(rule: &Rule) -> String {
    let mut reason = format!("rule {} {}", rule.tool, action_name(rule.action));
    if let Some(h) = &rule.harness {
        reason.push_str(&format!(" for harness {h}"));
    }
    if let Some(c) = &rule.chat {
        reason.push_str(&format!(" for chat {}", safe_label(c)));
    }
    reason
}

fn action_name(action: Action) -> &'static str {
    match action {
        Action::Allow => "allow",
        Action::Ask => "ask",
        Action::Deny => "deny",
    }
}

/// A name as it may appear in a reason: one line, bounded.
fn safe_label(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(64).collect()
}

/// Evaluates a call against the policy (contract, section 4). Steps:
/// normalize; the reject list over `fold_text(input_text)`; the matching
/// rules ranked chat, then harness, then tool specificity, ties to the most
/// restrictive action; no rule is `Ask`; an `allow` whose pinned description
/// changed is `Ask`; an `allow` for a shell tool whose input holds text the
/// shell reads in a way a matcher cannot is `Ask`. The reason never holds
/// the input text. `Ask` times out to `Deny` in the caller.
pub fn evaluate(policy: &PolicyFile, call: &Call) -> Decision {
    let normalized = normalize_tool_name(&call.tool, &policy.servers);
    let decide = |action: Action, reason: String| Decision {
        action,
        normalized_tool: normalized.clone(),
        reason,
    };
    let folded = fold_text(&call.input_text);
    if let Some(key) = reject_hit(&folded) {
        return decide(Action::Deny, format!("reject list: {key}"));
    }
    let harness = call.harness.trim().to_ascii_lowercase();
    // (chat set, harness set, tool rank) for every matching rule.
    let mut matches: Vec<((u8, u8, u8), &Rule)> = Vec::new();
    for rule in &policy.rules {
        let Some(tool) = tool_rank(&rule.tool, &normalized) else {
            continue;
        };
        if let Some(h) = &rule.harness {
            if h.trim().to_ascii_lowercase() != harness {
                continue;
            }
        }
        if let Some(c) = &rule.chat {
            if call.chat.as_deref() != Some(c.as_str()) {
                continue;
            }
        }
        let rank = (
            u8::from(rule.chat.is_some()),
            u8::from(rule.harness.is_some()),
            tool,
        );
        matches.push((rank, rule));
    }
    let Some(top) = matches.iter().map(|(rank, _)| *rank).max() else {
        return decide(Action::Ask, "no rule".to_string());
    };
    let winners: Vec<&Rule> = matches
        .iter()
        .filter(|(rank, _)| *rank == top)
        .map(|(_, rule)| *rule)
        .collect();
    let Some(winner) = winners
        .iter()
        .copied()
        .max_by_key(|rule| restrictiveness(rule.action))
    else {
        return decide(Action::Ask, "no rule".to_string());
    };
    if winner.action != Action::Allow {
        return decide(winner.action, rule_reason(winner));
    }
    // Every tied `allow` must hold its pin: one changed description is enough
    // to ask (case 57).
    let pins: Vec<&str> = winners
        .iter()
        .filter_map(|rule| rule.description_sha256.as_deref())
        .collect();
    if !pins.is_empty() {
        let current = call.description.as_deref().map(description_sha256);
        let all_hold = current
            .as_deref()
            .is_some_and(|hash| pins.iter().all(|pin| pin.eq_ignore_ascii_case(hash)));
        if !all_hold {
            return decide(Action::Ask, "description changed".to_string());
        }
    }
    if SHELL_TOOLS.contains(&normalized.as_str())
        && UNREADABLE_SHELL_TOKENS.iter().any(|t| folded.contains(t))
    {
        return decide(
            Action::Ask,
            "shell text the matcher cannot read".to_string(),
        );
    }
    decide(Action::Allow, rule_reason(winner))
}

/// Why a policy file was not used. The default policy (no rules, every call
/// `Ask`) is used instead: nothing fails open (contract, section 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadProblem {
    Missing,
    Unreadable(String),
    Invalid(String),
}

impl std::fmt::Display for LoadProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadProblem::Missing => write!(f, "no policy file"),
            LoadProblem::Unreadable(e) => write!(f, "policy file unreadable: {e}"),
            LoadProblem::Invalid(e) => write!(f, "policy file invalid: {e}"),
        }
    }
}

/// Largest policy file read, in bytes. A bigger one is treated as invalid.
pub const MAX_POLICY_BYTES: u64 = 1024 * 1024;

/// `$TK_POLICY` if set and not empty, else `~/.tk/policy.json`. User-level
/// only: no project-level file is ever read (contract, section 1).
pub fn default_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("TK_POLICY").filter(|p| !p.is_empty()) {
        return Some(std::path::PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".tk/policy.json"))
}

/// Parses and checks a policy file's text against the schema's limits.
pub fn parse(text: &str) -> Result<PolicyFile, LoadProblem> {
    let policy: PolicyFile =
        serde_json::from_str(text).map_err(|e| LoadProblem::Invalid(e.to_string()))?;
    policy.validate().map_err(LoadProblem::Invalid)?;
    Ok(policy)
}

/// Reads the policy at `path`. Any problem returns the default policy with
/// the problem, so the caller can say why every call asks.
pub fn load(path: &std::path::Path) -> (PolicyFile, Option<LoadProblem>) {
    let fallback = |problem| (PolicyFile::default(), Some(problem));
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return fallback(LoadProblem::Missing)
        }
        Err(e) => return fallback(LoadProblem::Unreadable(e.kind().to_string())),
    };
    if !meta.is_file() || meta.len() > MAX_POLICY_BYTES {
        return fallback(LoadProblem::Invalid("not a file under 1 MiB".into()));
    }
    match std::fs::read_to_string(path) {
        Err(e) => fallback(LoadProblem::Unreadable(e.kind().to_string())),
        Ok(text) => match parse(&text) {
            Ok(policy) => (policy, None),
            Err(problem) => fallback(problem),
        },
    }
}

fn valid_rule_tool(tool: &str) -> bool {
    let name = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    if tool == "*" {
        return true;
    }
    match tool.split_once('/') {
        Some((server, rest)) => name(server) && (rest == "*" || name(rest)),
        None => name(tool),
    }
}

fn valid_harness(h: &str) -> bool {
    let b = h.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn valid_server(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 100
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn lower_hex_64(d: &str) -> bool {
    d.len() == 64 && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl PolicyFile {
    /// The checks `contracts/policy.schema.json` makes that serde does not.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!("version {} (expected 1)", self.version));
        }
        if !(1..=3600).contains(&self.ask_timeout_seconds) {
            return Err("askTimeoutSeconds outside 1 to 3600".into());
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.servers {
            if !valid_server(s) {
                return Err(format!("server name {:?}", safe_label(s)));
            }
            if !seen.insert(s.as_str()) {
                return Err(format!("server {:?} listed twice", safe_label(s)));
            }
        }
        if self.rules.len() > 1000 {
            return Err("more than 1000 rules".into());
        }
        for (i, rule) in self.rules.iter().enumerate() {
            if !valid_rule_tool(&rule.tool) {
                return Err(format!("rule {i}: tool {:?}", safe_label(&rule.tool)));
            }
            if rule.harness.as_deref().is_some_and(|h| !valid_harness(h)) {
                return Err(format!("rule {i}: harness"));
            }
            if rule
                .chat
                .as_deref()
                .is_some_and(|c| c.is_empty() || c.chars().count() > 200)
            {
                return Err(format!("rule {i}: chat"));
            }
            if rule
                .description_sha256
                .as_deref()
                .is_some_and(|d| !lower_hex_64(d))
            {
                return Err(format!("rule {i}: descriptionSha256"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(raw: &str) -> String {
        normalize_tool_name(raw, &["exa".to_string(), "github".to_string()])
    }

    #[test]
    fn every_spelling_from_ap_section_4_normalizes_to_one_name() {
        for raw in [
            "mcp__exa__search",
            "mcp_exa_search",
            "exa_search",
            "exa__search",
            "MCP__Exa__Search",
            "exa/search",
        ] {
            assert_eq!(n(raw), "exa/search", "{raw}");
        }
    }

    #[test]
    fn claude_plugin_servers_drop_the_plugin_name() {
        assert_eq!(n("mcp__plugin_tk_exa__search"), "exa/search");
    }

    #[test]
    fn built_ins_stay_bare() {
        assert_eq!(n("Bash"), "bash");
        assert_eq!(n("read_file"), "read_file");
        assert_eq!(n("WebFetch"), "webfetch");
    }

    #[test]
    fn unknown_server_single_underscore_is_a_bare_name() {
        // `foo_bar` with no `foo` server connected is a built-in, not `foo/bar`.
        assert_eq!(n("foo_bar"), "foo_bar");
    }

    #[test]
    fn other_characters_become_underscores() {
        assert_eq!(n("mcp__my-server__do.thing"), "my_server/do_thing");
    }

    #[test]
    fn fold_text_drops_invisibles_and_maps_spaces_and_fullwidth() {
        assert_eq!(
            fold_text("ANTHROPIC\u{200B}_BASE\u{00A0}URL"),
            "anthropic_base url"
        );
        assert_eq!(fold_text("\u{FF21}\u{FF30}\u{FF29}"), "api");
    }

    #[test]
    fn policy_file_round_trips_and_rejects_unknown_fields() {
        let text =
            r#"{"version":1,"servers":["exa"],"rules":[{"tool":"exa/search","action":"allow"}]}"#;
        let p: PolicyFile = serde_json::from_str(text).unwrap();
        assert_eq!(p.ask_timeout_seconds, 60);
        assert_eq!(p.rules[0].action, Action::Allow);
        assert!(serde_json::from_str::<PolicyFile>(r#"{"version":1,"extra":1}"#).is_err());
    }

    fn call(tool: &str, harness: &str, input: &str) -> Call {
        Call {
            tool: tool.into(),
            harness: harness.into(),
            chat: None,
            description: None,
            input_text: input.into(),
        }
    }

    fn policy(rules: &str) -> PolicyFile {
        parse(&format!(
            r#"{{"version":1,"servers":["exa","github"],"rules":{rules}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn the_default_policy_asks_with_no_rule() {
        let d = evaluate(&PolicyFile::default(), &call("mcp__exa__search", "pi", ""));
        assert_eq!(d.action, Action::Ask);
        assert_eq!(d.reason, "no rule");
        assert_eq!(d.normalized_tool, "exa/search");
    }

    #[test]
    fn confusables_fold_to_latin_and_ascii_is_untouched() {
        // Cyrillic capital and small a, Greek omicron, a Unicode hyphen.
        assert_eq!(fold_text("\u{0410}\u{0430}\u{03BF}\u{2010}"), "aao-");
        // ASCII never changes, even letters TR39 maps elsewhere (`m` to `rn`).
        assert_eq!(fold_text("rm -rf ~/1Il0O"), "rm -rf ~/1il0o");
        // A character whose prototype is longer than one character stays.
        assert_eq!(fold_text("\u{2474}"), "\u{2474}");
    }

    #[test]
    fn git_context_needs_the_word_git_or_a_config_path() {
        assert!(git_context("git -c core.pager=x log"));
        assert!(git_context("/usr/bin/git config --global x"));
        assert!(git_context("cat .git/config"));
        assert!(git_context("export git_config_parameters=x"));
        assert!(git_context("git \\\n -c core.pager=x log"));
        assert!(!git_context("legit -c core.pager"));
        assert!(!git_context("digit config"));
        assert!(!git_context("read notes/protocol.version.md"));
        assert!(!git_context("git\n-c"), "a new line ends the test");
    }

    #[test]
    fn git_keys_deny_only_in_a_git_context() {
        let p = policy(r#"[{"tool":"bash","action":"allow"}]"#);
        let d = evaluate(
            &p,
            &call("Bash", "pi", "git -c protocol.ext.allow=always fetch"),
        );
        assert_eq!(d.action, Action::Deny);
        assert_eq!(d.reason, "reject list: protocol.");
        let d = evaluate(&p, &call("Bash", "pi", "cat docs/protocol.md"));
        assert_eq!(d.action, Action::Allow);
        let d = evaluate(
            &p,
            &call("Bash", "pi", "git \u{200B}-c core.hooksPath=/tmp/h commit"),
        );
        assert_eq!(d.reason, "reject list: core.hookspath");
    }

    #[test]
    fn endpoint_keys_deny_every_tool_in_any_spelling() {
        let p = policy(r#"[{"tool":"*","action":"allow"}]"#);
        for text in [
            "ANTHROPIC_BASE_URL=x",
            "\u{FF21}NTHROPIC_BASE_URL=x",
            "anthropic\u{00AD}_base_url",
            "\u{0430}nthropic_base_url",
            "codex \u{2010}\u{2010}dangerously-bypass",
            "{\"dangerouslyDisableSandbox\":true}",
        ] {
            let d = evaluate(&p, &call("mcp__exa__search", "pi", text));
            assert_eq!(d.action, Action::Deny, "{text:?}");
            assert!(d.reason.starts_with("reject list: "), "{text:?}");
            assert!(!d.reason.contains('='), "the reason never holds the input");
        }
    }

    #[test]
    fn ranking_chat_then_harness_then_tool_and_ties_go_restrictive() {
        let p = policy(
            r#"[
            {"tool":"exa/search","action":"allow"},
            {"tool":"*","harness":"codex","action":"ask"},
            {"tool":"exa/*","chat":"c9","action":"deny"},
            {"tool":"github/*","action":"allow"},
            {"tool":"github/*","action":"ask"}
        ]"#,
        );
        // Harness set beats a more specific tool.
        let d = evaluate(&p, &call("mcp__exa__search", "codex", ""));
        assert_eq!(d.action, Action::Ask);
        // Chat set beats everything.
        let mut c = call("mcp__exa__search", "pi", "");
        c.chat = Some("c9".into());
        assert_eq!(evaluate(&p, &c).action, Action::Deny);
        // Equal rank: the most restrictive.
        let d = evaluate(&p, &call("mcp__github__get_issue", "pi", ""));
        assert_eq!(d.action, Action::Ask);
        assert!(d.reason.starts_with("rule github/* ask"));
        // A server pattern never matches a built-in.
        let d = evaluate(&p, &call("Bash", "pi", "ls"));
        assert_eq!(d.reason, "no rule");
    }

    #[test]
    fn a_pinned_allow_asks_when_the_description_is_missing_or_changed() {
        let pin = description_sha256("Write a file.");
        let p = policy(&format!(
            r#"[{{"tool":"github/create_issue","action":"allow","descriptionSha256":"{pin}"}}]"#
        ));
        let mut c = call("mcp__github__create_issue", "pi", "");
        assert_eq!(evaluate(&p, &c).reason, "description changed");
        c.description = Some("Write a file. And more.".into());
        assert_eq!(evaluate(&p, &c).reason, "description changed");
        c.description = Some("Write a file.".into());
        assert_eq!(evaluate(&p, &c).action, Action::Allow);
    }

    #[test]
    fn shell_text_the_matcher_cannot_read_turns_allow_into_ask() {
        let p = policy(r#"[{"tool":"bash","action":"allow"},{"tool":"read","action":"allow"}]"#);
        for text in [
            "echo $HOME",
            "echo `id`",
            "ls *(e:x:)",
            "cat ~/.ssh/id_ed25519",
            "echo \u{FF04}HOME",
            "a \\\n b",
            "!!",
            "echo {a,b}",
        ] {
            let d = evaluate(&p, &call("bash", "claude-code", text));
            assert_eq!(d.action, Action::Ask, "{text:?}");
            assert_eq!(d.reason, "shell text the matcher cannot read");
        }
        // Not a shell tool: the same text is allowed.
        assert_eq!(
            evaluate(&p, &call("Read", "claude-code", "$HOME/x")).action,
            Action::Allow
        );
        assert_eq!(
            evaluate(&p, &call("Bash", "pi", "ls -la")).action,
            Action::Allow
        );
    }

    #[test]
    fn bad_policy_files_fall_back_to_the_default_with_a_reason() {
        let dir = std::env::temp_dir().join(format!(
            "tk-policy-test-{}-{}",
            std::process::id(),
            crate::token::mint().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (p, problem) = load(&dir.join("missing.json"));
        assert_eq!(p, PolicyFile::default());
        assert_eq!(problem, Some(LoadProblem::Missing));
        for (name, text) in [
            ("version.json", r#"{"version":2}"#),
            ("timeout.json", r#"{"version":1,"askTimeoutSeconds":0}"#),
            (
                "tool.json",
                r#"{"version":1,"rules":[{"tool":"Exa/*","action":"allow"}]}"#,
            ),
            (
                "pin.json",
                r#"{"version":1,"rules":[{"tool":"*","action":"allow","descriptionSha256":"ABC"}]}"#,
            ),
            ("servers.json", r#"{"version":1,"servers":["exa","exa"]}"#),
            (
                "field.json",
                r#"{"version":1,"rules":[{"tool":"*","action":"allow","when":"always"}]}"#,
            ),
            ("broken.json", "{"),
        ] {
            std::fs::write(dir.join(name), text).unwrap();
            let (p, problem) = load(&dir.join(name));
            assert_eq!(p, PolicyFile::default(), "{name}");
            assert!(matches!(problem, Some(LoadProblem::Invalid(_))), "{name}");
        }
        let (_, problem) = load(&dir);
        assert!(matches!(problem, Some(LoadProblem::Invalid(_))), "a folder");
        std::fs::write(
            dir.join("good.json"),
            r#"{"version":1,"rules":[{"tool":"exa/*","action":"allow"}]}"#,
        )
        .unwrap();
        let (p, problem) = load(&dir.join("good.json"));
        assert_eq!(problem, None);
        assert_eq!(p.rules.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
