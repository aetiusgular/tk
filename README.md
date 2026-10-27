# tk

Working name. A local control plane for AI coding harnesses (a gate, a daemon with one audit log, `tk connect`, `tk run`) and a curated tool library on the official MCP registry's format.

Status: scaffold. Nothing here works yet. Built in parallel lanes; each crate and package says which lane owns it.

| Path | What |
|---|---|
| `crates/tk-core` | Shared types: policy, event, token |
| `crates/tk-daemon` | The OTLP receiver, the activity log, the socket server |
| `crates/tk-cli` | `tk connect`, `tk run`, `tk watch` |
| `crates/tk-audit-linux` | The Linux OS feed from auditd |
| `packages/tk-pi` | The Pi package: gate and emitter |
| `packages/tk-*-plugin` | Claude Code, Codex and OpenCode adapters |
| `library/` | Schema, validator, exports, catalog |
| `contracts/` | What the parts agree on, with fixtures |
| `macos/` | The activity collector and the computer-use helper, own workspaces |

Build: `cargo test --workspace`, `npm install --ignore-scripts`, `npx tsc --noEmit`, `npx vitest run`.

MIT. See `NOTICE` for what was copied from where.
