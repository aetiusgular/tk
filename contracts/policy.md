# Contract: the policy file and the matcher

Written by tk session 0, 27 Oct 2026. Implemented twice: in Rust by lane B (`crates/tk-core/src/policy.rs`, used by the daemon and `tk connect`) and in TypeScript by lane A (`packages/tk-pi`), with lane F's hook script as a third reader. All three are tested against `contracts/fixtures/policy/`. A change to this file is a request for integration.

Schema: `contracts/policy.schema.json`. Fixture: `fixtures/policy/policy.json` (10 rules) and `fixtures/policy/cases.json` (21 cases with the expected action and reason prefix).

## 1. The file

- Path: `$TK_POLICY` if set, else `~/.tk/policy.json`. User-level only. `tk` never reads a project-level policy file, because an agent can write one (rules section 5, item 4).
- Read lazily: no reader opens it at load or session start (corner case 46). Lane A reads it on the first `tool_call`.
- A missing file is the default policy: no rules, so every call is `ask` (section 5, step 4), and `ask` times out to `deny`. A file that does not parse, or fails the schema, is treated as missing and the reason says so. Nothing fails open.

```json
{
  "version": 1,
  "askTimeoutSeconds": 60,
  "servers": ["exa", "github"],
  "rules": [
    { "tool": "exa/*", "action": "allow" },
    { "tool": "fs/write_file", "chat": "chat-b", "action": "allow",
      "descriptionSha256": "475c1f1f..." }
  ]
}
```

| Field | Meaning |
|---|---|
| `version` | `1` |
| `askTimeoutSeconds` | How long an `ask` waits before it becomes `deny`. 1 to 3600, default 60 |
| `servers` | MCP server names the user connected. Used only to split `{server}_{tool}` names (section 2, step 6) |
| `rules[].tool` | A normalized name (`server/tool` or a bare built-in), `server/*`, or `*` |
| `rules[].harness` | Optional. A harness id from `contracts/harness-presets.json` |
| `rules[].chat` | Optional. A chat id as the harness or `tk run` reports it |
| `rules[].action` | `allow`, `ask` or `deny` |
| `rules[].descriptionSha256` | Optional. SHA-256 hex of the approved tool description (case 57) |

## 2. Normalizing a tool name

Every harness spells an MCP tool differently (note AP section 4; corner case 60). The matcher normalizes before it matches. In order:

1. Trim; ASCII-lowercase; replace every character outside `[a-z0-9_/]` with `_`.
2. If the name contains `/`, it is already `server/tool`: done (after step 7 on the server part).
3. `mcp__<rest>`: split `<rest>` at the first `__`, else at the first `_`. Claude Code, Pi, Copilot (`mcp__s__t`).
4. `mcp_<rest>`: the same split. Gemini, Qwen, Hermes, Crush (`mcp_s_t`).
5. `<a>__<b>`: server `a`, tool `b`. OpenClaw's bridge, MetaMCP (`s__t`).
6. `<a>_<b>` where `a` is the longest entry of `servers` that the name starts with, followed by `_`: server `a`. OpenCode, ToolHive vMCP (`s_t`, `{workload}_t`). A name with no such server is a bare built-in name.
7. A server named `plugin_<plugin>_<server>` (Claude Code's plugin servers, `mcp__plugin_<plugin>_<server>__<tool>`) becomes `<server>`.

Anything else is a built-in tool by its bare lowercased name: `bash`, `read`, `write`, `edit`, `webfetch`.

Known gap: a server name that itself contains `_` is ambiguous in steps 3 and 4 (Gemini's parser splits at the first underscore too: note AP section 4). Library entries use server names without `_` (lane E's validator).

Source: the Rust `normalize_tool_name` in `crates/tk-core/src/policy.rs` and its tests, written by session 0 from this section.

## 3. The reject list

Run before any rule, on `fold(inputText)`. A hit is `deny`, whatever the rules say, with the reason `reject list: <key>`.

**Folding** (case 30, case 33): lowercase; drop invisible and format characters (U+00AD, U+034F, U+200B to U+200F, U+2060 to U+2064, U+FEFF, U+180E); map every Unicode space (U+0085, U+00A0, U+1680, U+2000 to U+200A, U+2028, U+2029, U+202F, U+205F, U+3000) to one ASCII space; map fullwidth ASCII (U+FF01 to U+FF5E) to ASCII; map confusable letters from other scripts to their Latin look-alike (Unicode TR39 `confusables.txt`, Latin targets only). Session 0's Rust `fold_text` does all but the last; fixture case 21 needs it. Lanes A and B add it, from a pinned copy of `confusables.txt` or a pinned package, and record the version.

### 3.1 Endpoint, sandbox and approval keys, every tool

`anthropic_base_url`, `chatgpt_base_url`, `dangerouslydisablesandbox`, `approval_policy`, `--dangerously-`, `settings.local.json`.

These are the keys the 2025 and 2026 incidents wrote (note AP section 5: the Downloads-folder hijack, the marketplace plugin hijack, Codex's bypass flag). `settings.local.json` is refused for reads too; integration may narrow it to writes if that is too strict in use.

### 3.2 Git config that names a program or a transport, in a git context only

Keys: `core.fsmonitor`, `core.hookspath`, `core.sshcommand`, `core.pager`, `core.editor`, `core.askpass`, `core.gitproxy`, `core.attributesfile`, `sequence.editor`, `pager.`, `alias.`, `gpg.program`, `gpg.ssh.program`, `credential.helper`, `include.path`, `includeif.`, `filter.`, `.driver`, `.textconv`, `diff.external`, `uploadpack`, `receivepack`, `.insteadof`, `.pushinsteadof`, `protocol.`, `ext::`, `fd::`, `git_allow_protocol`, `git_ssh`, `git_proxy_command`, `git_askpass`, `git_external_diff`, `git_config_parameters`, `git_config_count`.

A **git context** is folded text that contains a git config path (`.git/`, `.gitmodules`, `.gitattributes`, `.gitconfig`, `git_config`), or the word `git` (not inside a longer word) followed later on the same line by `-c`, `config` or `--config-env`. Outside a git context these keys are ordinary text: a note about `protocol.version` is not refused.

Source: rules section 4, cases 27, 31 and 32, and the session 6 findings that named each key (`plans/parallel/session_6.md`).

## 4. Matching

1. Normalize the tool name (section 2).
2. Run the reject list (section 3). A hit is `deny`; stop.
3. Collect the rules that match: `tool` equals the normalized name, or is `<server>/*` with the same server, or is `*`; `harness` absent or equal; `chat` absent or equal.
4. None: the answer is `ask`, reason `no rule`.
5. Rank the matches by, in order: `chat` set beats unset; then `harness` set beats unset; then the tool pattern, exact beats `server/*` beats `*`. The top rank wins. Among rules of equal rank the most restrictive action wins: `deny`, then `ask`, then `allow`. So a rule scoped to a chat overrides everything else for that chat, which is how a chat is locked down.
6. If the answer is `allow` and the winning rule has `descriptionSha256`: when the call carries no description, or `sha256(description)` differs, the answer is `ask`, reason `description changed`.
7. If the answer is `allow` and the tool is a shell tool (`bash`, `shell`, `exec_command`, `local_shell`, `run_shell_command`, `user_bash`, `terminal`) and the input contains a character or form the shell reads in a way a matcher cannot (`$`, a backtick, `!`, `{`, `}`, `~`, `(e:`, `(+`, `(-`, `\`), the answer is `ask`, reason `shell text the matcher cannot read`. Case 30.
8. `ask` waits `askTimeoutSeconds` for an answer, then is `deny`, reason `ask timed out`. A client that cannot show the question answers `deny` at once (case 63).

The answer carries `action`, `normalizedTool` and `reason`. The reason is shown in the card or the refusal; it never contains the input text.

The Codex shell tool names (`shell`, `exec_command`, `local_shell`) are inferred, not read; lane F confirms them against Codex 0.163.0 in the contract check.

## 5. Per harness

| Harness | Where it runs | `ask` becomes |
|---|---|---|
| Pi | lane A's `tool_call` handler | `ctx.ui.confirm` with the timeout |
| Claude Code | lane F's `PreToolUse` hook | `permissionDecision: "ask"` |
| Codex | lane F's `PreToolUse` hook | `deny` with the reason (Codex hooks have no ask; note AP section 4) |
| OpenCode | lane F's `tool.execute.before` | the daemon's `approve` when it runs, else `deny` |
| Any, through the daemon | `approve` in `daemon-protocol.md` | the first approver's answer, else `deny` at the timeout |
