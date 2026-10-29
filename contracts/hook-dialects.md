# Hook dialects: how each wave 4 agent runs tk's gate

Written by integration w2 on 28 Oct 2026, for wave 4's lanes L, N and P (`plans/parallel_build_tk.md`, section 7a). It turns note AQ section 4 (`research-notes/AQ_harness_coverage_matrix.md`, read 27 Oct) and a fresh read of each agent's docs and source on 28 Oct into one contract per agent: where the gate is registered, what it reads on stdin, how it answers, the switch that makes a failed gate block, and where an `ask` goes. Each agent has a fixture folder, `fixtures/hook-dialects/<preset id>/`, with a registration, the pre-tool input for a shell call (and an MCP call where the agent documents one), the replies it accepts, and `sources.json`, which says for every file whether it is the vendor's documented example verbatim or built from the documented fields, its source, and a mark. `contracts.test.ts` holds the folders to that shape.

Marks: **V** read in a primary source at the version named (docs, source at a tag, the npm tarball's code or bundled docs); **I** inference; **NF** looked for and not found. Nothing here was run against a real agent: integration w2 installed none of them. Each lane reads its agents' rows, checks them against the agent at the version it builds for (rule 6), and records any difference in its status doc; a difference that changes the gate is a request for integration, which fixes this file.

## 1. What every dialect does

Common to all sixteen, from section 7a and the corner cases:

1. **One decision, the core's.** The dialect turns the agent's input into the core's `{tool, input, harness, chat}` (`packages/tk-claude-plugin/src/core/gate.ts`, synced into each package), calls it, and turns `allow`, `ask` and `deny` into the agent's reply. The agent's tool name goes through the matcher with the agent's id, so its name form is preset data (`toolNameForm` in its preset file), not dialect code.
2. **Allow only where a rule says allow** (case 72). An agent where a hook's allow skips its own prompt gets an explicit allow only for a rule's `allow`; with no rule, the dialect gives the agent's "no decision" only where that does not run the call under the agent's own auto-approve mode, else the daemon approver or deny.
3. **Ask** (cases 71, 63): the agent's own prompt where it can show one (column "Own ask"), else the daemon approver's answer within the hook's timeout, else deny. Never "no decision".
4. **Deny one call, never the run** (case 79): each agent's per-call form; the run-ending forms are listed per agent and never used.
5. **Fail closed where the agent allows it** (case 70): set the switch in column "Fail switch" in the registration; where there is none the row says "fails open" and `tk status` shows it. Keep the gate fast: a timeout is the failure most agents let through.
6. **Output** (case 77): print only the agent's decision. Where stdout on success reaches the model (Kiro), print nothing on allow. Never echo the tool input.
7. **Token** (case 76): the chat token from `TK_CHAT_TOKEN` or the 0600 file `TK_CHAT_TOKEN_FILE` names, never `daemon.token`. Where an agent strips variables by name, the row says whether `TK_CHAT_TOKEN_FILE` survives.
8. **Shell tools** (case 82): the agent's shell tool names map to `bash` in its preset file (`coverage.shellAliases`), so case 30's checks run.
9. **Beside Orca** (case 75): registered next to Orca's own status hook in the same file where Orca writes one, never replacing it; tests run with Orca's entry installed.
10. **Full access** (case 83): tests also run under Orca's full-access flag for that agent.
11. **User level only** (case 78): registration goes in the user-level file; `tk connect <agent> --gate` (lane Q) writes it under lane C's rules.

## 2. The agents

Read 28 Oct 2026 by two read-only research agents for integration w2, in the vendors' repositories at a tag or commit, the npm tarballs' bundled docs and code, and binary strings; each claim's file and line are in the agent's `sources.json`. The vendors' web pages could not be fetched in this session (each fetch waited on a permission prompt no one was there to answer), so the agents read primary sources instead, and **Cursor, Kiro and Antigravity have no fixtures yet** (their `sources.json` holds note AQ's claims, marked NF for this read). Amp's docs page was not read either; its type definitions were.

### 2a. Lane L: Claude-format command hooks

| Agent (preset id), read at | Registration | Fail switch | Allow | Deny (one call) | Own ask | Ends the run (never use) | Shell; MCP name |
|---|---|---|---|---|---|---|---|
| OpenClaude (`openclaude`), 0.33.0 bundle | `hooks.PreToolUse[{matcher, hooks:[{type:"command", command, timeout}]}]` in `~/.openclaude/settings.json` (`OPENCLAUDE_CONFIG_DIR`); timeout in seconds, default 600 s | None: `onFailure` is not in its schema and is dropped (V). Fails open | `permissionDecision: "allow"` skips its prompt | `permissionDecision: "deny"` with a reason, or exit 2 | Yes, `"ask"` | `continue: false` | `Bash`; `mcp__s__t` |
| Qwen Code (`qwen-code`), v0.26.3 | `hooks.PreToolUse` in `~/.qwen/settings.json`; timeout seconds, default 60 (1000 and up read as ms) | None. Fails open | `"allow"` does not skip its own checks | `"deny"`, or exit 2 | Yes; deny in `-p`, background subagents and ACP | `continue: false` (I) | `run_shell_command` (Claude's `Bash` accepted as a matcher alias); `mcp__s__t` |
| Droid (`droid`), binary 0.251.0, docs `892925c` | `hooks` in `~/.factory/settings.json`. The binary merges `~/.factory/hooks.json` over it per event, so a user `hooks.json` PreToolUse array shadows tk's: never create that file, and `tk doctor` checks it (V). Timeout seconds, default 60 | None. Fails open | `"allow"` skips its permission check; `updatedInput` is merged | `"deny"`, or exit 2 | No: `ask` only logs in the binary (its docs say it prompts) | Exit 3 (undocumented); `continue: false` | `Execute`; `s___t` in the binary, `mcp__s__t` in the docs (match both) |
| Command Code (`command-code`), 1.88.1 | `hooks.PreToolUse[{matcher, hooks:[{type:"command", command, timeout, failClosed}]}]` in `~/.commandcode/settings.json`; timeout seconds, default 30. The matcher is a case-insensitive regex on `tool_display_name` (`^shell$`, not `shell`) | `failClosed: true` on the inner entry (undocumented, read in code) | Empty stdout | `permissionDecision: "deny"`, or exit 2 | No | `continue: false` | `shell_command`; `mcp__s__t`. Its hook environment drops every variable matching `/TOKEN/i` and 14 more patterns, **`TK_CHAT_TOKEN_FILE` included** (V): the dialect needs another way to the token file (a fixed path under the daemon's chat folder named by the chat id, or a wrapper argument). Hooks are skipped in plan mode |
| Kimi Code (`kimi`), 2.4.0 | `[[hooks]]` in `~/.kimi-code/config.toml` (`KIMI_CODE_HOME`) with exactly `event`, `matcher`, `command`, `timeout` (seconds 1 to 600, default 30): a fifth key fails the whole config (zod `.strict()`, V) | None. Fails open | Ignored | Exit 2, or `permissionDecision: "deny"` | No | None found (I) | `Bash`; `mcp__s__t` |
| Auggie (`auggie`), 0.36.0 | `hooks` in `~/.augment/settings.json`, Claude's shape; timeout in ms (default 60000, I); plain regexes never match MCP tools, use `mcp:*`. Or a `script-policy` entry in `toolPermissions` | Hooks: none, fail open. The script policy fails closed (non-zero, a 30 s timeout or a missing file deny) | Parsed, controls nothing (I) | Exit 2, `"deny"` or `continue: false`, each one call | No | None found | `launch-process`; `<tool>_<server>` |
| Copilot CLI (`copilot-cli`), docs `57f571f`; binary 1.0.105 unreadable | Any `~/.copilot/hooks/*.json` (`COPILOT_HOME`): `{version: 1, hooks: {preToolUse: [{type: "command", bash or exec plus args, timeoutSec}]}}`; default 30 s; matcher anchored | A crash or non-zero exit denies; a timeout always lets it through | `permissionDecision: "allow"` | `"deny"` with `permissionDecisionReason` | Yes, except autopilot and the cloud agent | `permissionRequest` deny with `interrupt: true` | `bash`, `powershell`; MCP form NF this read (note AQ had `<server>-<tool>`) |
| Grok Build (`grok`), 1.0.71 embedded docs | `~/.grok/hooks/<name>.json`: `{hooks: {PreToolUse: [{matcher, hooks: [{type: "command", command, timeout, failClosed}]}]}}`; timeout seconds, default 5; user hooks need folder trust | `failClosed: true` (also `fail_closed`, `onFailure: "block"`) from 1.0.71 only; npm `latest` is 1.0.70, which has none: pin 1.0.71 or check the version | `decision: "allow"` (or `hookSpecificOutput.permissionDecision`, which wins) | `"deny"`, or exit 2 | Yes, `"ask"` | `continue: false` | `run_terminal_command` (alias `Bash`); `s__t`. Compat imports off: `[compat.claude] hooks = false`, `[compat.cursor] hooks = false` |

### 2b. Lane N: own-dialect command hooks

| Agent, read at | Registration | Fail switch | Allow | Deny | Own ask | Ends the run | Shell; MCP name |
|---|---|---|---|---|---|---|---|
| goose (`goose`), v1.56.0 | A plugin `~/.agents/plugins/<name>/` with `plugin.json` and `hooks/hooks.json`: `PreToolUse[{matcher, hooks:[{type: "command", command, timeout, on_failure}]}]`; `matcher` an unanchored regex (`"*"` is invalid and skipped silently: use `.*`); runs under `sh -c`; default 30 s | `on_failure: "block"` (any other value skips the file) | Exit 0 with nothing, or `{"decision": "allow"}` | Exit 2 (stderr the reason) or `{"decision": "block", "reason"}` | No | None on PreToolUse | `shell`; `{extension}__{tool}`. Auto mode is its default: the hook runs after goose approves |
| Crush (`crush`), v0.98.2 | `hooks.PreToolUse[{name, matcher, command, timeout}]` (flat, not Claude's nested `hooks`) in `~/.config/crush/crush.json`, or `hook add` in `crushrc`; absolute command; default 30 s | None: a timeout, a crash, other exits and bad JSON are no opinion. Fails open | `decision: "allow"` pre-approves (skips its prompt) | `decision: "deny"`, or exit 2 | No: `ask` reads as no opinion | Exit 49 or `halt: true` | `bash`; `mcp_s_t`. Sub-agents' calls fire no hooks |
| Mistral Vibe (`mistral-vibe`), 3.0.3 | `[[hooks]]` in `~/.vibe/hooks.toml`: `name`, `type = "pre_tool"`, `command`, `match` (glob, or `re:` regex), `timeout` (default 60), `strict` | `strict = true` | No decision: the normal prompt follows | `{"decision": "deny", "reason"}` on exit 0. Exit 2 is a failure, not a deny | No | None on pre_tool | `file_system.bash`, `process.start` (background commands; `match = "bash"` misses it); `mcp_<s>.<t>` |
| Cursor (`cursor`) | `~/.cursor/hooks.json` version 1 (note AQ, not re-read: NF) | `failClosed: true` (AQ) | | | Shell and MCP events only (AQ) | | `MCP:<tool>` in preToolUse; `mcp_server_name` in beforeMCPExecution (AQ) |
| Antigravity (`antigravity`) | A named hook in `~/.gemini/config/hooks.json`, a map of named hooks; a Claude-style top-level `"hooks"` key loads as one hook named `hooks`; plugins do not read `hooks/hooks.json` (CHANGELOG 1.4.9, V); the rest NF | NF | | | `ask`, `force_ask` (AQ) | | `run_command` (AQ) |
| Kiro (`kiro`) | A version 1 hook file in `~/.kiro/hooks/` (AQ, NF here) | Any non-zero exit blocks (AQ) | Exit 0, print nothing: stdout reaches the model (case 77) | Non-zero exit (AQ) | No (AQ) | | `mcp_s_t` (AQ) |

### 2c. Lane P: plugins

| Agent, read at | Registration | On failure | Allow | Deny | Ask | Ends the run | Shell; MCP name |
|---|---|---|---|---|---|---|---|
| Amp (`amp`), `@ampcode/plugin` 0.0.0-20261029001459 `index.d.ts` | `~/.config/amp/plugins/<name>.ts`, run by Bun: `export default function (amp) { amp.on("tool.call", (event, ctx) => result) }`; the event is `{toolUseID, tool, input, thread: {id}}` (no cwd, no MCP server) | NF (a throw or a slow handler: the docs page was not read) | `{action: "allow"}` | `reject-and-continue` with a message | `await ctx.ui.confirm({title, message})`, which throws with no UI (`amp -x`) | `error` stops the thread worker | `Bash`, `shell_command`; MCP tools `mcp__s__t`, reached through `tool_search` and `code_exec` (whether `tool.call` fires inside `code_exec`: NF) |
| Cline (`cline`), 3.0.74 | The approval files: `CLINE_TOOL_APPROVAL_MODE=desktop`, **`CLINE_TOOL_APPROVAL_DIR=<dir>` (required: without it every gated call is denied)**, `--auto-approve false`; only in non-interactive runs (a prompt argument or piped stdin; the TUI and ACP use their own approvers). Request `<dir>/<sessionId>.request.<requestId>.json`; answer `<dir>/<sessionId>.decision.<requestId>.json` `{approved, reason?}`, written atomically (temp file and rename: a half-written answer is ignored); Cline polls every 200 ms and denies after 5 minutes | Denies after 5 minutes | `approved: true` | `approved: false`: that call only | The responder can wait on the daemon's approver | File hooks' `cancel` and a plugin's `stop` end the run | `run_commands` (`{commands: [...]}`); `${server}__${tool}`, sanitized and hashed past 64 characters. Read-only tools (`read_files`, `search_codebase`, `fetch_web_content`, `skills`, `ask_question`, `ask_followup_question`, `submit_and_exit`) never reach the file. Fallback: an SDK plugin's `beforeTool` returning `{skip, reason}` (3 s timeout, then degraded and the run goes on) |

Corrections to note AQ from this read, each in its `sources.json`: OpenClaude has no `onFailure`; Grok's `failClosed` starts at 1.0.71, which npm's `latest` is not yet; Droid's docs now call `hooks.json` the primary user file and its binary merges it over `settings.json`; Command Code's filter strips `TK_CHAT_TOKEN_FILE` too, so case 76's file variable does not reach its hook; Cline also needs `CLINE_TOOL_APPROVAL_DIR`, and desktop approval works only in prompt runs; Auggie's input rewrite (`updatedInput`) is verified and its hook timeout is likely in milliseconds; Copilot's `<server>-<tool>` was not found again (NF). Current versions on 28 Oct: Qwen 0.26.3, Droid 0.251.0, Command Code 1.88.1, Kimi 2.4.0, Copilot 1.0.105, Grok 1.0.70 latest (1.0.73 alpha).

The matcher already reads three of these name forms from the agent's id (`RAW_SEPARATORS` in the hook core and tk-core: Droid's `___`, Copilot's `-`, Mistral Vibe's `.`, and oh-my-pi's `_` after `mcp__`); any other separator the fold made, or capitals in a server name, turns an allow into an ask (integration w2 review, W2-S2). A lane that finds another form adds it there through integration.

## 3. Fixture folders

`fixtures/hook-dialects/<preset id>/`:

| File | What |
|---|---|
| `register.json`, `register.toml` or `register.ts` | One pre-tool registration as the agent's docs show it, the command replaced by `/ABSOLUTE/PATH/TO/tk-hook` |
| `stdin.pre-tool.json` | The pre-tool input for a shell call |
| `stdin.pre-tool.mcp.json` | The same for an MCP tool, where the agent documents one |
| `stdin.<event>.json` | Other events the gate reads (Cursor's `beforeMCPExecution`, `beforeShellExecution`) |
| `reply.allow.json`, `reply.deny.json`, `reply.ask.json` | The decisions the agent accepts on stdout; a decision by exit code alone is `reply.deny.exit`, the code on one line |
| `approval-request.json`, `approval-answer.*.json` | Cline's approval files |
| `sources.json` | Per file: `from` (`verbatim` or `built-from-fields`), `source`, `mark`; and `facts`, each claim with its source and mark |

A fixture marked `built-from-fields` is the shape the docs describe, with placeholder values; a lane whose agent runs in the cloud replaces it with a recorded input (a contract check that dumps what the agent sends) and says so in `sources.json`.

## Sources

Note AQ section 4 and its sources list (27 Oct); each agent's `sources.json` (28 Oct); `plans/parallel_build_tk.md` section 7a; rules section 4, cases 70 to 83.
