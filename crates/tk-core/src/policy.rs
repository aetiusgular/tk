//! The policy file (`~/.tk/policy.json`) and the matcher. Contract:
//! `contracts/policy.md`; schema: `contracts/policy.schema.json`; fixture:
//! `contracts/fixtures/policy/`.
//!
//! What is implemented here: the types, `normalize_tool_name` and
//! `fold_text` (the reject list's text folding), and the reject token table.
//! `evaluate` is a stub: lane B fills it (status doc `tk_lane_B.md`).

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
/// fullwidth ASCII mapped to ASCII. Confusable letters from other scripts
/// (Cyrillic `а` for `a`) are not folded here: lane B adds that with a
/// pinned crate or a table, and the fixture has a case for it marked
/// `confusable`.
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
            out.push(l);
        }
    }
    out
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

/// Evaluates a call against the policy. STUB: answers `Ask` with a reason
/// naming the stub. Lane B replaces the body; the fixture test in
/// `tests/policy_fixture.rs` is `#[ignore]` until then.
pub fn evaluate(_policy: &PolicyFile, call: &Call) -> Decision {
    Decision {
        action: Action::Ask,
        normalized_tool: normalize_tool_name(&call.tool, &_policy.servers),
        reason: "policy matcher not implemented (session 0 stub; lane B)".to_string(),
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

    #[test]
    fn evaluate_stub_answers_ask() {
        let call = Call {
            tool: "mcp__exa__search".into(),
            harness: "pi".into(),
            chat: None,
            description: None,
            input_text: String::new(),
        };
        let d = evaluate(&PolicyFile::default(), &call);
        assert_eq!(d.action, Action::Ask);
        assert_eq!(d.normalized_tool, "exa/search");
    }
}
