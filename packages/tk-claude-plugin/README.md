# tk-claude-plugin

The tk policy gate for Claude Code (T2.1). Two hooks:

- **PreToolUse** checks every tool call against `~/.tk/policy.json` (or `$TK_POLICY`), the contract in `contracts/policy.md`. `allow`, `deny` and `ask` become Claude Code's `permissionDecision`. An `ask` goes first to the tk daemon's approver (`tk watch --approve`); with no approver connected, or no daemon, Claude Code shows its own prompt. In `bypassPermissions` and `dontAsk` mode an unanswered `ask` is a deny. Secret-like values in the input of a tool that sends data out (MCP tools, web tools) are redacted through `updatedInput`; in a shell command or a file write they turn `allow` into `ask`.
- **PostToolUse** hashes the result, records the call in the tk daemon (no content), and replaces output that holds secret-like values through `updatedToolOutput`. Redaction is best effort: it finds secret-named keys and a few token shapes, nothing else.

A hook's `allow` never overrides a deny or ask rule in your Claude Code settings (Claude Code's permissions page: "Claude Code evaluates deny and ask rules regardless of what a PreToolUse hook returns").

## What the hooks run

Claude Code's plugin pane shows that a hook exists, not what it runs. These are the two commands, from `hooks/hooks.json`, run with your full user rights outside any sandbox:

```
node --no-warnings "${CLAUDE_PLUGIN_ROOT}/bin/tk-hook.mjs" pre     # PreToolUse, every tool, timeout 120 s, onFailure block
node --no-warnings "${CLAUDE_PLUGIN_ROOT}/bin/tk-hook.mjs" post    # PostToolUse, every tool, timeout 30 s
```

`bin/tk-hook.mjs` loads `src/hook.ts`, which reads only the policy file and, when present, the daemon's socket and token file (`~/Library/Application Support/tk/` on macOS, `$XDG_RUNTIME_DIR/tk/` on Linux). It makes no network call. It has no dependencies.

## Needs

- Claude Code 2.1.295 or later (`onFailure: "block"`: a crashed or timed-out PreToolUse hook blocks the call instead of letting it through). Contract-checked on 2.1.311.
- Node 22.18 or later on `PATH` (type stripping). An older Node makes the launcher deny every call with a reason. The plugin must not sit under a `node_modules` folder, where Node refuses to strip types.

## Install

From the tk marketplace (lane E exports it): `claude plugin marketplace add <path or repo>`, then `claude plugin install tk@tk`. For one session: `claude --plugin-dir <this folder>`.

For an organization, `managed-settings.fragment.json` locks hooks to managed ones and force-enables this plugin. `tk connect claude-code --allowlist` prints it; tk never writes a managed file.

A plugin installed with `--scope project` or `--scope local` loads only in the folder it was installed for: Claude Code uses such a record only when its `projectPath` is the folder it started in (read in 2.1.311's binary). `tk status` says so when you run it elsewhere.

## Deferred MCP tools

Claude Code defers MCP tool schemas behind its `ToolSearch` tool when many tools are loaded (seen on 2.1.304 with 571 tools; Mac test w2, MW2), and the gate sees `ToolSearch` as any other tool. With no rule for it, every deferred MCP tool costs one more ask before its own rule is read, and with no approver connected that ask is a deny. `ToolSearch` loads schemas and runs nothing, so a policy for this harness usually wants:

```json
{ "tool": "toolsearch", "harness": "claude-code", "action": "allow" }
```

tk does not add it for you: a rule allows a call only where you wrote it (rules case 72).

## Environment

| Variable | Use |
|---|---|
| `TK_POLICY` | Policy file path instead of `~/.tk/policy.json` |
| `TK_DAEMON_SOCKET` | Daemon socket path instead of the OS default |
| `TK_CHAT_TOKEN` | Chat token from `tk run`. Never the daemon's token: with no chat token the hook gets no approver, posts no record, and an `ask` falls back to Claude Code's own prompt |
| `TK_CHAT_TOKEN_FILE` | A file holding the chat token, which `tk run --boundary none` writes (0600) and names; refused when it is named `daemon.token`, sits inside the session folder, or is not yours and private |
| `TK_CHAT_ID` | Chat id for chat-scoped rules; else Claude Code's `session_id` |
