# tk-codex-plugin

The tk policy gate for Codex (T2.2), contract-checked on Codex 0.163.0.

- **PreToolUse** checks every tool call against `~/.tk/policy.json` (or `$TK_POLICY`), the contract in `contracts/policy.md`. A deny exits 2 with the reason on stderr, which Codex shows as "Command blocked by PreToolUse hook: tk: <reason>". An allow says nothing, so Codex's own approvals still run as configured.
- **ask is not supported by Codex hooks** ("parsed but not supported yet": Codex marks the hook failed and runs the call). So a policy `ask` goes to the tk daemon's approver (`tk watch --approve`). Only an approver's yes lets the call through; no approver, no daemon, a no or a timeout is a deny, and the reason says which.
- Secret-like values in an MCP tool's input are redacted through `updatedInput`. In a shell command or a patch they make the call an ask, so a deny unless approved.
- **PostToolUse** hashes the result and records the call in the tk daemon (no content). Codex cannot rewrite a tool's output from a hook, so output that holds secret-like values is replaced through `decision: "block"`: the model sees tk's redacted copy as the result.

## Codex fails open

At 0.163.0 every failed hook run lets the call through: a hook that cannot start, times out, exits with a code other than 0 or 2, or exits 2 with an empty stderr (`codex-rs/hooks/src/events/pre_tool_use.rs:204-290`). This hook never crashes on purpose: every path ends in a decision, the launcher turns a load failure into exit 2, and an ask waits at most 90 s against a 120 s hook timeout. A killed Node process still fails open. The lock that does not fail open is Codex's own sandbox and approval policy, and `requirements.toml` for MCP servers.

## What the hooks run

Plugin hooks run with your full user rights and Codex's whole environment, outside the sandbox. From `hooks/hooks.json`:

```
node --no-warnings "${PLUGIN_ROOT}/bin/tk-hook.mjs" pre     # PreToolUse, every tool, timeout 120 s
node --no-warnings "${PLUGIN_ROOT}/bin/tk-hook.mjs" post    # PostToolUse, every tool, timeout 30 s
```

Codex substitutes `${PLUGIN_ROOT}` into the text and runs it through your shell. The hook reads only the policy file and, when present, the daemon's socket and token file. It makes no network call and has no dependencies. Node 22.18 or later must be on `PATH`.

## Install

```
codex plugin marketplace add <tk marketplace folder or repo>
codex plugin add tk@tk
```

Then trust the two hooks in Codex (`/hooks` in the TUI). Codex skips an untrusted plugin hook without a word, and re-asks after any change to a hook entry. For an organization, `requirements.fragment.toml` pins MCP servers by identity; `tk connect codex --allowlist` prints it and tk never writes a managed file.

## Environment

As the Claude Code plugin: `TK_POLICY`, `TK_DAEMON_SOCKET`, `TK_CHAT_TOKEN`, `TK_CHAT_ID` (else Codex's `session_id`).
