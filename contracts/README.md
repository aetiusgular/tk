# Contracts

Written by tk session 0 on 27 Oct 2026. Lanes build against these and their fixtures, not against each other's code (`plans/parallel_build_tk.md`, section 2, point 3). No lane edits this folder during a wave; a change is a request for integration.

| Contract | Fixture | Filled by | Read by |
|---|---|---|---|
| `policy.md`, `policy.schema.json` | `fixtures/policy/` (10 rules, 21 cases) | B (Rust), A (TypeScript) | A, B, C, F |
| `daemon-protocol.md` | `fixtures/daemon/exchange.jsonl` | B | A, D, F; J in wave 2 |
| `entry.schema.json`, `shapes/` | `fixtures/entry/` (one per kind, plus a marketplace) | E | C, E; G, H in wave 2 |
| `harness-presets.json` | itself | C | A, C, D, E, F |
| `events.md` | `tests/fixtures/otlp/` | A emits, B decodes | A, B |
| `docker-args.md` | the copied `docker_mcp.rs` | C | A (the redactor), C |

A server's tool list is not in `server.json` (upstream's 2025-12-11 schema has no tool list, and tk's `_meta` object is `additionalProperties: false`): it lives in a sibling `tools.json` in the entry's folder, as the seed catalog does since wave 1. Integration w2 blessed that form at lane G's request: a tool list inside `_meta` would push a large server past the official registry's 4,096-byte cap on the publisher object (lane G, `validators.go:727-737`), and a mirrored entry has no tool list at all unless something connected to the server.

`contracts.test.ts` validates every fixture against its schema. The Rust fixture tests are `crates/tk-core/tests/policy_fixture.rs` and the test in `crates/tk-daemon/src/protocol.rs`.
