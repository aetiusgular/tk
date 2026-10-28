# tk-hermes-plugin

The tk policy gate for Hermes Agent (T2.4, wave 2 lane I). Written against Hermes's source at v0.22.5 (`bf0203a7`). **Not run against a real Hermes yet**: installing Hermes was refused in the lane's workspace. The plugin's own code is tested (Python `unittest`, and the Node side with vitest, both with a real policy file); `tests/cli-contract/checks/I/hermesGate.mjs` is written for the box test and has not run.

## How it works

Hermes runs Python plugins. This one (`__init__.py`, `tk_gate.py`) registers two hooks and hands each call to `bin/tk-decide.mjs`, which decides with tk's one gate core (`src/core/`, a copy of `packages/tk-claude-plugin/src/core/`; do not edit it here). So the rules are the same as in every other harness, at the cost of one Node start per tool call.

- **pre_tool_call**:
  - **deny** returns `{"action": "block", "message": "tk: <reason>"}`; Hermes gives the message to the model as the tool's result.
  - **ask** goes to the tk daemon's approver first (`tk watch --approve`). With no approver it goes to Hermes's own approval gate (`{"action": "approve"}`), which asks you in a foreground chat or through the gateway and blocks when no one can answer. **Under `--yolo`, `/yolo`, an unattended turn or `approvals.mode: off`, Hermes's gate approves everything, so there tk blocks the ask instead.** An approver's no, or a timeout, is a block.
  - **allow** returns nothing; with secret-like values redacted from an MCP tool's input it returns `{"action": "modify", "args"}`. In a shell command or a write they make the call an ask.
  - A call whose input names Hermes's own files or switches (`~/.hermes/`, `HERMES_HOME`, `HERMES_SAFE_MODE`, `HERMES_YOLO_MODE`, `HERMES_ENABLE_PROJECT_PLUGINS`, `HERMES_ACCEPT_HOOKS`) asks.
- **transform_tool_result** replaces a result that holds a secret-like value with the redacted text, and records the call in the tk daemon (a hash, no content).
- **Everything that goes wrong blocks**: no `node` on PATH, a Node older than 22.18, a decision that takes over 25 seconds, an answer the plugin cannot read. Hermes also blocks a `pre_tool_call` callback that raises or runs past `plugins.hook_callback_timeout` (30 s by default). An ask waits at most 20 seconds. If you lower `plugins.hook_callback_timeout` under 25 seconds, every slow decision blocks.

MCP tools reach the gate as `mcp__<server>__<tool>`; the shell tool is `terminal`.

## Install

Hermes installs plugins per profile, into `$HERMES_HOME/plugins/` (default `~/.hermes`). From a clone of tk:

```bash
cp -r /abs/path/tk/packages/tk-hermes-plugin ~/.hermes/plugins/tk
hermes plugins enable tk
```

Then restart the gateway. `plugins.enabled` is opt-in: a plugin that is not listed does not load, and `plugins.disabled` always wins. `HERMES_SAFE_MODE=1` loads no plugin at all. For another profile, copy it into `~/.hermes/profiles/<name>/plugins/tk` and enable it there. tk never installs into a project's `.hermes/plugins/`.

Needs `node` (22.18 or later) on the PATH Hermes runs with. No Python dependencies.

## Known limits

- **A later plugin's `modify` changes the call after tk decided.** Hermes gives every `pre_tool_call` callback the original args and merges all `modify` returns afterwards (`hermes_cli/plugins.py:1967-1976`). A `block` from tk still wins over everything. Orca's Hermes plugin only observes.
- One Node process per tool call adds its start time (about 0.1 to 0.3 s) to every call.

## Environment

As the Claude Code plugin: `TK_POLICY`, `TK_DAEMON_SOCKET`, `TK_CHAT_TOKEN`, `TK_CHAT_TOKEN_FILE`, `TK_CHAT_ID` (else Hermes's `session_id`), read from Hermes's own environment.
