# tk-openclaw-plugin

The tk policy gate for OpenClaw (T2.4, wave 2 lane I). OpenClaw is not in Orca's agent grid; it is in the toolkit plan. Written against the npm package openclaw 2026.10.8 (its shipped docs and `dist/`) and run against it, on Node 24.22.1, with a fake chat-completions server (`tests/cli-contract/checks/I/openclawGate.mjs`). Not on your machines yet.

- **before_tool_call**, registered at the lowest priority (`-1000000`), so it runs after every other plugin.
  - **deny** returns `{block: true, blockReason: "tk: <reason>"}`, which is final in OpenClaw.
  - **ask** goes to the tk daemon's approver first (`tk watch --approve`). With no approver it returns `requireApproval`, which asks you through OpenClaw's plugin approvals (`/approve`). A non-interactive run (`openclaw agent exec`) has no one to ask, and OpenClaw refuses the call. An approver's no, or a timeout, is a block.
  - **allow** returns the params tk checked (with secret-like values redacted where tk redacted them). OpenClaw runs the last returned params, and every handler sees the original call, so a plugin that rewrote the params before tk is overridden: the call that runs is the call tk checked (run: `openclaw-tk-runs-last`). Codex's native relay in OpenClaw reads the same params handed back as no rewrite.
  - OpenClaw's shell tool `exec` (it turns `bash` into `exec`) is gated as **`bash`**, so a rule for `bash` covers it and the shell-text check runs. Code Mode's outer `exec` is gated as `exec`.
  - A call whose input names OpenClaw's config (`~/.openclaw/`, `openclaw.json`, `OPENCLAW_CONFIG_PATH`, `OPENCLAW_STATE_DIR`, `OPENCLAW_HOME`) asks: the config enables plugins and hot-reloads.
- **after_tool_call** records the call in the tk daemon (a hash and a size, no content). OpenClaw's `after_tool_call` cannot change a result, so tk does not redact OpenClaw's results.
- A throw or a timeout blocks: OpenClaw runs `before_tool_call` fail-closed. tk's handler has a 120 second budget (OpenClaw's default is 15), so an ask can wait; `plugins.entries.tk.hooks.timeouts.before_tool_call` overrides it.

MCP tools reach the gate as `<server>__<tool>` (no `mcp__` prefix, `-` kept); the gate reads `server/tool`.

## Install

From a clone of tk (OpenClaw needs Node 24.22.1 or later):

```bash
openclaw plugins install --link /abs/path/tk/packages/tk-openclaw-plugin --force
openclaw plugins enable tk
```

or list the folder in `~/.openclaw/openclaw.json` under `plugins.load.paths` and set `plugins.entries.tk.enabled: true` (the route the checks use). tk never installs into a workspace's `.openclaw/extensions`.

The plugin imports nothing from OpenClaw or npm and reads nothing until the first tool call.

## Known limits

- A plugin with a higher priority that already returned `requireApproval` freezes its own params snapshot, which tk's returned params cannot replace.
- Results are not redacted (no hook can change them before the model reads them; `tool_result_persist` changes only what is stored).

## Environment

As the Claude Code plugin: `TK_POLICY`, `TK_DAEMON_SOCKET`, `TK_CHAT_TOKEN`, `TK_CHAT_TOKEN_FILE`, `TK_CHAT_ID` (else OpenClaw's `sessionId`).
