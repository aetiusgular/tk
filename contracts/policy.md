# Contract: the policy file and the matcher

Written by tk session 0, 27 Oct 2026; settled by integration w1 the same day. Implemented three times: in Rust by lane B (`crates/tk-core/src/policy.rs`, used by `tk connect` and, from wave 4, `tk acp`), in TypeScript by lane F (`packages/tk-claude-plugin/src/core/`, the canonical hook core, copied byte for byte into every hook plugin), and in TypeScript by lane A (`packages/tk-pi`). The hook core is the reference; integration w1 brought the other two to it. All three are tested against `contracts/fixtures/policy/`. A change to this file is a request for integration.

Schema: `contracts/policy.schema.json`. Fixtures: `fixtures/policy/policy.json` (12 rules), `fixtures/policy/cases.json` (45 cases with the expected action and reason prefix; 22 to 33 pin what integration w1 settled, 34 to 45 what its security pass changed), and `fixtures/policy/fold.json` (the fold of every non-ASCII code point whose fold holds ASCII, 3,391 cases, generated from the hook core by `crates/tk-core/gen/gen-fold-tables.mjs`).

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
| `servers` | MCP server names the user connected, normalized by section 2, step 1 before use (so `my-server` matches `my-server_fetch`). Used only to split `{server}_{tool}` names (section 2, step 6) |
| `rules[].tool` | A normalized name (`server/tool` or a bare built-in), `server/*`, or `*` |
| `rules[].harness` | Optional. A harness id from `contracts/harness-presets.json` |
| `rules[].chat` | Optional. A chat id as the harness or `tk run` reports it |
| `rules[].action` | `allow`, `ask` or `deny` |
| `rules[].descriptionSha256` | Optional. SHA-256 hex of the approved tool description (case 57) |

## 2. Normalizing a tool name

Every harness spells an MCP tool differently (note AP section 4, note AQ section 6; corner case 60). The matcher normalizes before it matches. It is a function of the name, the `servers` list and the agent's id (`harness`); later waves apply an agent's declared name form from its preset row before these steps (note AQ section 6), and no wave 1 agent declares one. In order:

1. Trim; ASCII-lowercase; replace every character outside `[a-z0-9_/]` with `_`. The same step normalizes each `servers` entry.
2. If the name contains `/`, it is already `server/tool`: done (after step 7 on the server part).
3. `mcp__<rest>`: split `<rest>` at the first run of two or more `_` (the whole run consumed), else at the first `_`. Claude Code, Codex, Pi, OpenClaude, Qwen Code 0.26.1, Hermes, Command Code, Kimi (`mcp__s__t`; note AQ section 9 corrects the earlier attribution of Qwen and Hermes to step 4).
4. `mcp_<rest>`: the same split. Gemini, Crush, Kiro's hooks (`mcp_s_t`).
5. `<a>__<b>`, split at the first run of two or more `_`: server `a`, tool `b`. Grok, Cline, Codebuff, goose, OpenClaw's bridge, MetaMCP (`s__t`), and Droid's binary (`s___t`).
6. `<a>_<b>` where `a` is the longest normalized entry of `servers` that the name starts with, followed by `_`: server `a`. OpenCode and Kilo (`s_t`), ToolHive vMCP (`{workload}_t`), and Copilot CLI's `<server>-<tool>`, which step 1 turns into `s_t` (note AQ section 9: Copilot is not `mcp__s__t`). A name with no such server is a bare built-in name.
7. A server named `plugin_<plugin>_<server>` (Claude Code's plugin servers, `mcp__plugin_<plugin>_<server>__<tool>`) becomes `<server>`.

Anything else is a built-in tool by its bare lowercased name: `bash`, `read`, `write`, `edit`, `webfetch`.

Known gaps: a server name that itself contains `_` is ambiguous in steps 3 and 4 (Gemini's parser splits at the first underscore too: note AP section 4). Library entries and `tk connect` use server names without `_`. The forms note AQ section 6 lists as unhandled (Auggie's `<tool>_<server>`, Cursor's `MCP:<tool>`, dispatcher and code tools, Kiro's `@s/t`, Vibe's `mcp_s.t`) wait for the per-agent forms of waves 2 and 4.

Source: `normalizeToolName` in the hook core (`packages/tk-claude-plugin/src/core/policy.ts`), `normalize_tool_name_for` in `crates/tk-core/src/policy.rs`, `normalizeToolName` in `packages/tk-pi/src/policy.ts`; fixture cases 1 to 12 and 22 to 25.

## 3. The reject list

Run before any rule, on `fold(inputText)` and again on the same text with every `"` and `'` removed, because the shell joins `ANTHROPIC_"BASE_URL"` and `g''it` into one word (integration w1, security finding S1; fixture cases 34, 35). A hit is `deny`, whatever the rules say, with the reason `reject list: <key>`.

**The input text.** For a shell tool: the command, then on their own lines the values of every other field of the call except a free-text `description` (a working folder such as OpenCode's bash `workdir`, a timeout), each value that holds a `/` also once more with a trailing `/` so a folder meets the folder markers of section 4 step 7a (integration w1, finding S7). For any other tool: every key and every string, number and boolean value, one per line, unescaped (the hook core also adds the serialized JSON).

**Escapes** first: `\uXXXX` (JSON and TOML) and `\UXXXXXXXX` (TOML) are decoded, so a key a tool writes into a config file escaped is seen as the key it becomes (fixture case 28).

**Folding** (case 30, case 33), settled by integration w1 on the hook core's algorithm: NFKD; drop combining marks (general category Mn and Me); then per character: drop invisible and format characters (U+00AD, U+034F, U+200B to U+200F, U+2060 to U+2064, U+FEFF, U+180E); map every Unicode space (U+0085, U+00A0, U+1680, U+2000 to U+200A, U+2028, U+2029, U+202F, U+205F, U+3000) to one ASCII space; map fullwidth ASCII (U+FF01 to U+FF5E) to ASCII; map a confusable to its ASCII look-alike, one or more characters (Unicode `confusables.txt` 17.0.0, sha256 `091c7f82…ef22a`, from unicode-org/unicodetools at `578f6c0`); then lowercase and map once more. The three matchers give the same result on every one of the 194,432 non-ASCII code points (run 27 Oct; `fixtures/policy/fold.json` holds the 3,391 whose fold contains ASCII). The Rust tables are generated from the hook core's sources and Node's ICU (Unicode 17.0); `gen-fold-tables.mjs --check` fails when they drift.

### 3.1 Endpoint, sandbox and approval keys, every tool

`anthropic_base_url`, `chatgpt_base_url`, `dangerouslydisablesandbox`, `approval_policy`, `--dangerously-`, `settings.local.json`.

These are the keys the 2025 and 2026 incidents wrote (note AP section 5: the Downloads-folder hijack, the marketplace plugin hijack, Codex's bypass flag). `settings.local.json` is refused for reads too; integration may narrow it to writes if that is too strict in use.

### 3.2 Git config that names a program or a transport, in a git context only

Keys: `core.fsmonitor`, `core.hookspath`, `core.sshcommand`, `core.pager`, `core.editor`, `core.askpass`, `core.gitproxy`, `core.attributesfile`, `sequence.editor`, `pager.`, `alias.`, `gpg.program`, `gpg.ssh.program`, `credential.helper`, `include.path`, `includeif.`, `filter.`, `.driver`, `.textconv`, `diff.external`, `uploadpack`, `receivepack`, `.insteadof`, `.pushinsteadof`, `protocol.`, `ext::`, `fd::`, `git_allow_protocol`, `git_ssh`, `git_proxy_command`, `git_askpass`, `git_external_diff`, `git_config_parameters`, `git_config_count`.

A **git context** is folded text that contains a git config path (`.git/`, `.gitmodules`, `.gitattributes`, `.gitconfig`, `git_config`), or the word `git` (not inside a longer word) followed later on the same line by `-c`, `config` or `--config-env`. A line that ends in `\` continues on the next, as the shell reads it (fixture case 44). Outside a git context these keys are ordinary text: a note about `protocol.version` is not refused.

Source: rules section 4, cases 27, 31 and 32, and the session 6 findings that named each key (`plans/parallel/session_6.md`).

## 4. Matching

1. Normalize the tool name (section 2).
2. Run the reject list (section 3). A hit is `deny`; stop.
3. Collect the rules that match: `tool` equals the normalized name, or is `<server>/*` with the same server, or is `*`; `harness` absent or equal; `chat` absent or equal.
4. None: the answer is `ask`, reason `no rule`.
5. Rank the matches by, in order: `chat` set beats unset; then `harness` set beats unset; then the tool pattern, exact beats `server/*` beats `*`. The top rank wins. Among rules of equal rank the most restrictive action wins: `deny`, then `ask`, then `allow`. So a rule scoped to a chat overrides everything else for that chat, which is how a chat is locked down.
6. If the answer is `allow` and any `allow` rule of the top rank has `descriptionSha256`: when the call carries no description, or `sha256(description)` differs from any of those pins, the answer is `ask`, reason `description changed`. Every tied pin must hold (tk-core's rule; the TypeScript matchers took the first rule of a tie until integration w1's security finding S10).
7. If the answer is still `allow`:
   a. If the folded input, or the folded input with quotes removed, names a control file, the answer is `ask`, reason `input names a control file (<path>)`. The list: `.tk/`, `tk/daemon.`, `tk/otlp.endpoint`, `tk_policy`, `tk_chat_token`, `.pi/agent/`, `.claude/settings`, `.claude.json`, `.claude/plugins`, `disableallhooks`, `managed-settings`, `.codex/config.toml`, `.codex/hooks.json`, `.codex/plugins`, `requirements.toml`, `opencode.json`, `.config/opencode/`, `.cache/opencode/`; git's own config and hooks, which name programs git runs later: `.git/config`, `.git/hooks/`, `.gitconfig`, `.config/git/`, `.gitmodules` (finding S4); and a harness's whole project or home folder, which holds the settings, hooks, plugins, commands and skills it loads: `.claude/`, `.codex/`, `.pi/` (finding S7). So an allowed tool cannot quietly rewrite the policy, switch a gate off or plant a program git or a harness runs later (lanes A and F; integration w1; fixture cases 27, 32, 36, 41 to 43, 45). A best-effort narrowing: a shell command that changes folder first (`cd /home/u/.claude; sed -i ... settings.json`) still names no marker. `bash: allow` stays full user power, as lane A's README says.
   b. If the tool is a shell tool (`bash`, `shell`, `exec_command`, `local_shell`, `run_shell_command`, `user_bash`, `terminal`, `powershell`) and the folded input contains a character or form the shell reads in a way a matcher cannot (`$`, a backtick, `!`, `{`, `}`, `~`, `(e:`, `(+`, `(-`, `\`, and the globs `*`, `?`, `[`, which name files the matcher never sees: finding S1, fixture cases 37 to 40), the answer is `ask`, reason `shell text the matcher cannot read: ` then the first token found in that list's order in backticks, and ` (a glob)` after `*`, `?` or `[` (integration w1 fix pass, Mac finding M9; the fixtures match the part before the colon). Case 30. The check reads the folded text, so a fullwidth `＄` asks too (case 33): stricter than the raw text, the side that fails safe. Not in the list, on purpose: a word-initial `=` (zsh's command-path expansion) and Unicode spaces. tk's rules name tools, not command strings, so neither can rename a command around a rule, and the reject list already reads the folded text (fixture cases 29, 30). Other agents' shell tools under other names (`run_terminal_command`, `Execute`, `file_system.bash`) map to `bash` through their preset rows from wave 4 (note AQ section 6).
8. `ask` waits `askTimeoutSeconds` for an answer, then is `deny`, reason `ask timed out`. A client that cannot show the question answers `deny` at once (case 63).

The answer carries `action`, `normalizedTool` and `reason`. The reason is shown in the card or the refusal; it never contains the input text.

Codex 0.163.0's hooks see every shell call as `Bash` (lane F, read in `codex-rs` at `rust-v0.163.0` and seen in the contract run), so `shell`, `exec_command` and `local_shell` never reach a Codex hook; they stay in the list for other readers.

A policy path given in `TK_POLICY` that lies inside the session's folder is not used (the hook core's `loadPolicyFrom`, integration w1): a project's settings file can set the variable there. The default `~/.tk/policy.json` is exempt.

## 5. Per harness

| Harness | Where it runs | `ask` becomes |
|---|---|---|
| Pi | lane A's `tool_call` handler | `ctx.ui.confirm` with the timeout |
| Claude Code | lane F's `PreToolUse` hook | `permissionDecision: "ask"` |
| Codex | lane F's `PreToolUse` hook | the daemon's `approve`, else `deny` with the reason (Codex hooks have no ask; note AP section 4) |
| OpenCode | lane F's `tool.execute.before` | the daemon's `approve` when it runs, else `deny` |

| Any, through the daemon | `approve` in `daemon-protocol.md` | the first approver's answer, else `deny` at the timeout |

A hook reaches the daemon only with a chat token: `TK_CHAT_TOKEN`, or the file `TK_CHAT_TOKEN_FILE` names, which `tk run --boundary none` writes. It never reads `daemon.token`, refuses a token file named `daemon.token`, and closes the connection when the daemon's `hello` reply says the token has daemon scope (integration w1; security finding S3). With no chat token, Claude Code's `ask` falls back to its own prompt, and Codex's and OpenCode's are `deny`.
