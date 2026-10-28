# tk-pi

The Pi package of tk: a `tool_call` gate and an OTLP emitter (lane A). A memory extension follows in wave 2 (lane K); its file exists and is not loaded.

Built and tested against Pi 1.4.0 (`@earendil-works/pi-coding-agent`). No runtime dependencies: it imports nothing from Pi or npm, only Node's own modules.

## Install

From a clone of the tk repository, as a local package (Pi loads a local path without copying it; Pi `docs/packages.md`):

```bash
pi install /absolute/path/to/tk/packages/tk-pi
pi list
```

Personal installs go into `~/.pi/agent/settings.json`, so the package loads in every project without project trust (rules case 47). Do not install it with `--local`.

A git source cannot name a subfolder: Pi clones `git:github.com/<owner>/<repo>` whole (Pi 1.4.0 `dist/utils/git.js`), so `pi install git:github.com/aetiusgular/tk/packages/tk-pi@<tag>` does not work. An npm form comes when the package is published.

To try it for one run without installing: `pi -e /absolute/path/to/tk/packages/tk-pi`.

### oh-my-pi (wave 2, lane I)

The same package runs in oh-my-pi (`omp`), which reads its `pi.extensions` field when a package has no `omp` field. Run and checked against omp 18.13.3 (`tests/cli-contract/checks/I/piFamily.mjs`):

```bash
omp install /absolute/path/to/tk/packages/tk-pi
```

`omp install <path>` links the folder (a symlink under `~/.omp/plugins/node_modules/`), it does not copy it, so a `git pull` in the clone updates it. Under omp the gate answers as harness `oh-my-pi`, so a rule with `"harness": "oh-my-pi"` applies there and not in Pi; it knows it is in omp because omp runs on Bun and Pi on Node.

What differs in omp:

- **omp's default approval mode is `yolo`: tk's gate is the only check.** Keep `bash`, `edit` and `write` at `ask` or tighter, or set `tools.approvalMode` in `~/.omp/agent/config.yml`.
- omp names an MCP tool `mcp__<server>_<tool>` and drops a tool's own `<server>_` prefix, so server `echo`'s tool `echo_it` arrives as `mcp__echo_it`. With the server in the policy's `servers`, the gate reads `echo/it`; write rules as `echo/it` or `echo/*`.
- In print mode (`-p`), and in ACP without form elicitation, `ctx.ui.confirm` answers no at once, so every ask is a no.
- A `tool_call` handler that does not answer within 30 seconds blocks the call (omp's own limit); dialogs do not count against it.
- `~/.omp/` is a control path: `agent/config.yml` holds the approval mode and the extensions list, and the plugin lock file can switch this package off.

## What it runs

Pi shows that a package has extensions, not what they do, so here is all of it. Both files register handlers when Pi loads them and do nothing else until an event arrives: no file read, no timer, no network (rules case 46).

| File | Pi event | What it does |
|---|---|---|
| `extensions/gate.ts` | `tool_call` | Reads `~/.tk/policy.json` (first call, then again only when it changes), matches the call, and blocks, asks (`ctx.ui.confirm`) or lets it run. Any error blocks. |
| | `tool_result` | Replaces secret-like text in the result with `[REDACTED by tk]` before the model reads it. |
| | `user_bash` | Refuses a `!` or `!!` command you type that hits the reject list. |
| `extensions/emitter.ts` | `before_agent_start`, `tool_execution_start`, `tool_execution_end`, `message_end`, `session_start`, `session_shutdown`, and the gate's decisions | Sends OTLP log records to `TK_OTLP_ENDPOINT` (loopback only), once a second. Sends nothing without `TK_CHAT_TOKEN`. |

## The input that runs is the input the gate allowed

Pi runs every extension's `tool_call` handler in load order on one event, and a handler that runs after tk's can change the call's input after tk allowed it (Pi 1.4.0 `docs/extensions.md`, "Events and concurrency"); no handler can run after all others. So when the gate allows a call it freezes the input. A later extension that tries to change it throws, and Pi fails the call with that error instead of running input tk never saw ("Cannot assign to read only property", run on Pi 1.4.0 and omp 18.13.3). An extension that only reads the input, as Orca's status extension does, is unaffected. `TK_PI_FREEZE_INPUT=0` in the harness's environment turns this off, for a tool that changes its own arguments while it runs (none of Pi's built-ins does).

Not covered: in omp, a later extension can also return `{ input }`, which replaces the arguments without touching the frozen object (omp's documented rewrite). Only the extensions you install can do that.

## The policy file

`~/.tk/policy.json`, or the absolute path in `TK_POLICY`. The format is `contracts/policy.md` in the tk repository. Make it yours alone:

```bash
mkdir -p ~/.tk && chmod 700 ~/.tk
$EDITOR ~/.tk/policy.json && chmod 600 ~/.tk/policy.json
```

The gate does not use a file that another user owns or that group or others can write, a symbolic link, a FIFO or anything else that is not a regular file, a relative `TK_POLICY` or one inside the project folder, or a file over 1 MiB. It opens the file once per check and reads from that same handle, so the file it checked is the file it reads. With no usable file every call is asked about, and an unanswered question is a no.

An example for Pi on the box:

```json
{
  "version": 1,
  "askTimeoutSeconds": 60,
  "servers": ["gateway", "cu"],
  "rules": [
    { "tool": "read", "action": "allow" },
    { "tool": "grep", "action": "allow" },
    { "tool": "find", "action": "allow" },
    { "tool": "ls", "action": "allow" },
    { "tool": "edit", "action": "ask" },
    { "tool": "write", "action": "ask" },
    { "tool": "bash", "action": "ask" },
    { "tool": "gateway/*", "action": "ask" },
    { "tool": "cu/*", "action": "ask" }
  ]
}
```

Pi names an MCP tool `mcp__<server>__<tool>` (Pi `docs/mcp.md`); the gate reads it as `<server>/<tool>`, so a rule for `cu/*` covers every tool of the server named `cu`.

What each answer does:

- `allow`: the call runs. Even so, it is asked about when it is a shell command with text the matcher cannot read (`$`, a backtick, `!`, braces, `~`, `\`), when it names a tk, Pi or omp control file (`~/.tk/`, the daemon's token, `~/.pi/`, `~/.omp/`, `PI_CODING_AGENT_DIR`, `PI_CONFIG_DIR`), or when the tool's description no longer matches a pinned `descriptionSha256`.
- `ask`: a confirm dialog: in the TUI, in Orca as a card, in MonoCode's host as a card, through Pi's RPC protocol to any client. No answer within `askTimeoutSeconds` is a no. Where there is no UI (`--mode json`, `--print`) it is a no at once.
- `deny`: blocked; the model reads `Blocked by tk: <reason>`.
- Whatever the rules say, a call whose input names an endpoint, sandbox or approval key (`ANTHROPIC_BASE_URL`, `chatgpt_base_url`, `dangerouslyDisableSandbox`, `approval_policy`, `--dangerously-`, `settings.local.json`), or a git setting that runs a program inside a git command, is denied, in any letter case and in look-alike spellings.

## Computer use on the Mac (T0.4; documented, not run)

The fork's computer-use server is the MonoCode binary started as `computer-use-mcp`. It reaches MonoCode only through `MONOCODE_CU_ENDPOINT` and `MONOCODE_CU_TOKEN`, which MonoCode puts in the environment of a harness it starts (fork `computer_use/bridge_cli.rs:1-27`). Pi passes its whole environment to a stdio MCP server (pi-mcp 1.4.0 `dist/transports/stdio.js:75`), so the entry needs no `--env`:

```bash
pi mcp add cu --description "Computer use on this Mac; every call asks" -- "<path to the MonoCode binary>" computer-use-mcp
pi mcp list
```

and in the policy: `"servers": ["cu"]` and `{ "tool": "cu/*", "action": "ask" }`.

This works only in a Pi that MonoCode started. In the TUI or Orca the variables are missing and the server has nothing to connect to. Never write the token into `mcp.json` with `--env MONOCODE_CU_TOKEN=...`.

## Environment

| Variable | Read by | Meaning |
|---|---|---|
| `TK_POLICY` | gate | Absolute path of the policy file |
| `TK_CHAT_ID` | gate | The chat id rules match; else Pi's session id |
| `TK_REDACT` | gate | `0` or `off` turns the result redactor off |
| `TK_CHAT_TOKEN` | emitter | The chat token for `x-tk-token`; nothing is sent without it |
| `TK_OTLP_ENDPOINT` | emitter | The daemon's receiver, `http://127.0.0.1:<port>`; anything not loopback is refused |
| `TK_LOG_CONTENT` | emitter | `1` sends prompt, reply and tool input text (redacted, cut to 4 KiB) |

## What it does not do

- A `bash: allow` rule lets the agent do what you can do, including editing the policy file through a command the matcher can read. Keep `bash` on `ask` unless the chat runs under a sandbox (`tk run --boundary sbx`).
- An extension loaded after this one can change a call's input after the gate decided (Pi `types.d.ts`: "No re-validation is performed after mutation"), and any extension can post on Pi's event bus. Load only extensions you trust; they run with your rights.
- The redactor is best-effort: a secret in a shape it does not know, split across lines, or encoded, passes through.
- `user_bash` is checked against the reject list only, since you typed it.

## Checks

- `npx vitest run packages/tk-pi` from the repository root: the matcher against `contracts/fixtures/policy`, the gate and emitter against a fake Pi, the redactor.
- `node packages/tk-pi/live/pi-live.mjs --pi "$(command -v pi)"`: the package inside a real Pi, against a fake model and a fake receiver, in a temp HOME. 14 checks.
- `node packages/tk-pi/scripts/gen-confusables.mjs <confusables.txt>` regenerates `src/confusables.ts` from Unicode 17.0.0's file and refuses any other.
