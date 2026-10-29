# tk

Working name. A local control plane for AI coding harnesses (a gate, a daemon with one audit log, `tk connect`, `tk run`) and a curated tool library on the official MCP registry's format.

Status: wave 1 merged to `main`; wave 2 integrated on `integration/w2`, not merged to `main`. Wave 1 ran in a Linux cloud workspace, then on the box (Pi in the TUI and in Orca, a real OpenCode, the systemd user unit) and on the Mac (the Claude Code and Codex plugins, MonoCode with Claude Code). Wave 2 ran in the cloud only; its box test and Mac test come next. Built in parallel lanes; each crate and package says which lane owns it (`scripts/lane-ownership.json`).

| Path | What |
|---|---|
| `crates/tk-core` | Shared types: policy, event, token; the Rust matcher |
| `crates/tk-daemon` | The OTLP receiver, the activity log, the socket server, the approval channel; computer use served on macOS (`tk-daemon cu-mcp`); the Mac collector's registration (`tk-daemon collector-plan`) |
| `crates/tk-cli` | `tk connect`, `tk run`, `tk watch`, `tk status`, `tk cu`; `tk acp` and `tk doctor` are stubs for wave 4. The preset table is one file per agent in `src/connect/presets/` |
| `crates/tk-audit-linux` | The Linux OS feed from auditd (`tk-audit-feed`) |
| `packages/tk-pi` | The Pi package (also oh-my-pi's): gate, emitter, memory extension |
| `packages/tk-claude-plugin`, `tk-codex-plugin`, `tk-opencode-plugin` | Claude Code, Codex and OpenCode adapters; the canonical hook core is `tk-claude-plugin/src/core/` |
| `packages/tk-gemini-extension`, `tk-hermes-plugin`, `tk-openclaw-plugin`, `tk-kilo-plugin` | Gemini CLI, Hermes Agent, OpenClaw and Kilo Code adapters (wave 2) |
| `packages/tk-hooks-claude-compat`, `tk-hooks-native`, `tk-amp-plugin`, `tk-cline` | Scaffolds for wave 4 (the hook core is synced in; nothing runs yet) |
| `library/` | Schema, validator, exports, catalog; the registry mirror (`library/mirror/`), publish (`library/publish/`), trust records (`library/trust/`), the static browser (`library/site/`) |
| `contracts/` | What the parts agree on, with fixtures; `hook-dialects.md` for wave 4's agents |
| `tests/cli-contract/` | Checks against the real harness CLIs with fake model servers |
| `tests/integration/` | Cross-lane checks with the real daemon, `tk` and Pi |
| `macos/` | The activity collector and the computer-use helper, own workspaces |

Build: `cargo test --workspace`, `npm ci --ignore-scripts`, `npx tsc --noEmit`, `npx vitest run`, `python3 -m unittest discover -s packages/tk-hermes-plugin/tests`. Node 22.19 or later (the hooks and `library/cli.ts` run through Node's type stripping); CI runs Node 24.

Install from a clone (no published packages yet):

- Pi: `pi install /abs/path/tk/packages/tk-pi` (a git source cannot name a subfolder).
- Claude Code: `claude plugin marketplace add /abs/path/tk`, then `claude plugin install tk@tk`.
- Codex: `codex plugin marketplace add /abs/path/tk`, then `codex plugin add tk@tk`, then trust both hooks in `/hooks`.
- OpenCode: `"plugin": ["file:///abs/path/tk/packages/tk-opencode-plugin"]` in `~/.config/opencode/opencode.json` (`opencode.fragment.json`).
- oh-my-pi: `omp install /abs/path/tk/packages/tk-pi`.
- Gemini CLI: `gemini extensions install /abs/path/tk/packages/tk-gemini-extension --consent`, then save the user policy it prints.
- Kilo Code: `"plugin": ["file:///abs/path/tk/packages/tk-kilo-plugin"]` in `~/.config/kilo/kilo.json`, or the one line in `~/.config/kilo/plugin/tk.ts` (its README).
- Hermes Agent: `cp -r /abs/path/tk/packages/tk-hermes-plugin ~/.hermes/plugins/tk && hermes plugins enable tk` (Node on Hermes's PATH).
- OpenClaw: `openclaw plugins install --link /abs/path/tk/packages/tk-openclaw-plugin --force`, then `openclaw plugins enable tk`.

The library's commands run with Node, not as `tk` subcommands yet: `node library/cli.ts validate`, `node library/publish/cli.ts publish <entry-dir> --library <clone>` (what the plan calls `tk publish`; prints the pull request's steps unless `--open-pr`), `node library/mirror/cli.ts pull --out <dir>`, `node library/trust/cli.ts record|scan|osv|pin`, `node library/site/build.ts <out-dir>`.

## Harness coverage

How tk attaches to each agent in Orca's list. Tier G: tk registers a gate that runs before each tool call. Tier A: tk writes the agent's own allow, ask and deny rules. Tier O: tk wraps the agent from outside only (`tk run`, `tk watch`). Every agent also gets the outside shape. "Fails open" means the agent lets a call through when the gate crashes or times out and has no switch to stop that. Status says what ran, where and at which version; "planned" rows are research only (`research-notes/AQ_harness_coverage_matrix.md`).

| Agent | Tier | tk attaches through | Status |
|---|---|---|---|
| Pi | G | `packages/tk-pi` (`tool_call`) | Built, wave 1. Run in a real Pi 1.4.0 in the cloud (JSON and RPC modes; 14 live checks), and under `tk run` with the real daemon. Not run in Orca, MonoCode or the TUI |
| Claude Code | G | `packages/tk-claude-plugin` (PreToolUse, `onFailure: "block"`) | Built, wave 1. Contract checks pass against Claude Code 2.1.311; installed from this repository's marketplace as `tk@tk`. `bypassPermissions` not run (root) |
| Codex | G, fails open | `packages/tk-codex-plugin` (PreToolUse) | Built, wave 1. Contract check passes against Codex 0.163.0 and 0.164.0 |
| OpenCode | G | `packages/tk-opencode-plugin` (`tool.execute.before`) | Built, wave 1. Unit tests only; never run against a real OpenCode (source read at 1.19.4) |
| Gemini CLI | G, fails open | BeforeTool hook | Planned, wave 2. Scaffolded; the fake Gemini API served a real Gemini CLI 0.65.0 turn |
| Hermes Agent | G | Python plugin `pre_tool_call` | Planned, wave 2. Scaffolded |
| oh-my-pi | G | `tk-pi`, installed with `omp install` | Planned, wave 2 |
| Kilo Code | G | the OpenCode plugin with Kilo's paths | Planned, wave 2. Scaffolded |
| OpenClaude | G, fails open | PreToolUse in `~/.openclaude/settings.json` | Planned, wave 4 |
| Qwen Code | G, fails open | PreToolUse in `~/.qwen/settings.json` | Planned, wave 4 |
| Droid | G, fails open | PreToolUse in `~/.factory/settings.json`, beside Orca's entry | Planned, wave 4; fixtures only without a seat |
| Command Code | G | PreToolUse with `failClosed` | Planned, wave 4 |
| Kimi Code | G, fails open | `[[hooks]]` in `~/.kimi-code/config.toml` | Planned, wave 4 |
| Auggie | G, fails open | PreToolUse or a script policy | Planned, wave 4; fixtures only without a seat |
| GitHub Copilot CLI | G, open on timeout | preToolUse in `~/.copilot/hooks/` | Planned, wave 4 |
| Grok Build | G | PreToolUse in `~/.grok/hooks/` with `failClosed` | Planned, wave 4 |
| Cursor CLI | G | `~/.cursor/hooks.json` with `failClosed` | Planned, wave 4; fixtures only without a seat |
| Antigravity CLI | G, fail mode unknown | a bundle in `~/.gemini/config/hooks.json` | Planned, wave 4 |
| goose | G | a plugin's PreToolUse with `on_failure: "block"` | Planned, wave 4 |
| Mistral Vibe | G | `pre_tool` in `~/.vibe/hooks.toml` with `strict` | Planned, wave 4 |
| Charm Crush | G, fails open | PreToolUse in `crush.json` | Planned, wave 4 |
| Kiro CLI | G | a hook file in `~/.kiro/hooks/` | Planned, wave 4; fixtures only without a seat |
| Amp | G, fail mode unknown | a Bun plugin (`tool.call`) | Planned, wave 4; fixtures only without a seat |
| Cline CLI | G | the approval-file callback | Planned, wave 4 |
| Continue | A | `~/.continue/permissions.yaml` | Planned, wave 4 |
| Rovo Dev | A | `toolPermissions` in `~/.rovodev/config.yml` | Planned, wave 4; fixtures only without a seat |
| Codebuff | O | `tk run`, the MCP file | Planned, wave 4 |
| Any other CLI agent | O | `tk run -- <command>` | Planned, wave 4 |

Orca's ids differ for three agents: Orca says `claude`, `copilot` and `aug`; tk says `claude-code`, `copilot-cli` and (wave 4) Auggie's own id. The machine-readable form of this table is the preset table, one file per agent in `crates/tk-cli/src/connect/presets/` (the `coverage` field on each row; `index.json` lists the `harnesses` and the `planned` rows).

MIT. See `NOTICE` for what was copied from where.
