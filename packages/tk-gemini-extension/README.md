# tk-gemini-extension

The tk policy gate for Gemini CLI (T2.4, wave 2 lane I). Written against Gemini CLI's source at v0.66.0 and run against Gemini CLI 0.66.0 and 0.65.0 with a fake Gemini API (`tests/cli-contract/checks/I/geminiGate.mjs`). Not run interactively, and not on your machines yet.

## What it does

- **BeforeTool** (`hooks/hooks.json`, hook `tk-gate`) checks every built-in and MCP tool call against `~/.tk/policy.json` (or `$TK_POLICY`) with tk's shared gate core (`src/core/`, a copy of `packages/tk-claude-plugin/src/core/`; do not edit it here).
  - **deny** stops the call: Gemini sends `tk: <reason>` to the model as the tool's error. Works under `--yolo`.
  - **ask** goes to the tk daemon's approver first (`tk watch --approve`). With no approver, or no daemon, Gemini shows its own prompt. A headless run (`gemini -p`) does not pass that prompt: the call does not run. An approver's no, or a timeout, is a deny.
  - **allow** prints nothing. A Gemini hook cannot approve a call, only deny or ask; Gemini's own policy then decides. Under `--yolo` that is a run.
  - Secret-like values in an MCP tool's input are rewritten before the call. In a shell command or a file write they make the call an ask.
  - A call whose input names Gemini's control files (`.gemini/`, `/etc/gemini-cli/`, the macOS managed folder) asks, the way the core asks about tk's and other harnesses' files. `~/.gemini/settings.json` can switch every hook off (`hooksConfig.enabled: false`) or this one by name (`hooksConfig.disabled: ["tk-gate"]`).
- **AfterTool** (`tk-record`) records the call in the tk daemon (a hash and a size, no content) and, when the result holds a secret-like value, replaces what the model reads with the redacted text.
- **`policies/tk.toml`**, Gemini's extension tier: denies writes into `.tk/` and `.gemini/` and shell commands naming them, without the hook. Gemini ignores `allow` in an extension's policies.
- **`bin/tk-gemini-policy.mjs`** prints your tk policy as Gemini policy rules for the user tier. Save the output as `~/.gemini/policies/tk.toml` yourself; it writes nothing.

## Gemini fails open

When a hook crashes, is killed, exits with anything but 0 or 2, or times out, Gemini lets the call run. There is no switch for that (run on 0.66.0 and 0.65.0). So:

- the hook never crashes on purpose; every path ends in a decision, and `bin/tk-hook.mjs` turns a load failure into a block;
- an ask waits at most 90 seconds, under the hook's 120 second timeout;
- the two policy files above are the layer that holds when the hook does not. Save the user-tier file: it is what makes a tk deny hold with the hook down.

## Install

```
gemini extensions install /abs/path/tk/packages/tk-gemini-extension --consent
```

Gemini asks whether to trust the source folder first; answer y. It copies the folder to `~/.gemini/extensions/tk`; run `gemini extensions update tk` after a change. Then:

```
node ~/.gemini/extensions/tk/bin/tk-gemini-policy.mjs > ~/.gemini/policies/tk.toml
```

Node 22.18 or later, for type stripping. The extension has no dependencies, makes no network call and reads nothing until the first tool call.

## Tool names

Rules name Gemini's own tools: `run_shell_command`, `read_file`, `write_file`, `replace`, `glob`, `grep_search`, `list_directory`, `web_fetch`, `google_web_search`. A rule for `bash` does not cover `run_shell_command` (the alias map is preset data for a later wave). MCP tools reach the gate as `server/tool`, from Gemini's `mcp_context`, so a server name with `_` or `-` splits right.

## Environment

As the Claude Code plugin: `TK_POLICY`, `TK_DAEMON_SOCKET`, `TK_CHAT_TOKEN`, `TK_CHAT_TOKEN_FILE`, `TK_CHAT_ID` (else Gemini's `session_id`). With Gemini's `security.environmentVariableRedaction.enabled`, or under GitHub Actions, Gemini drops every variable whose name holds `TOKEN` from the hook's environment; the hook then reads the token file's path from `GEMINI_CLI_TK_CHAT_TOKEN_FILE`, which Gemini always keeps. Only the path is aliased, never a token.

## Known limits

- Another BeforeTool hook can rewrite a call's input after tk decided: Gemini merges every hook's `tool_input` and runs the result. Orca's Gemini hook only observes.
- A tk ask turns a deny in your own Gemini policy into Gemini's prompt (Gemini applies a hook's ask over its policy).
- Gemini gives a hook no call id; the decision and result records share a key made from the session, the tool and its arguments.
