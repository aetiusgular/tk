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

`contracts.test.ts` validates every fixture against its schema. The Rust fixture tests are `crates/tk-core/tests/policy_fixture.rs` and the test in `crates/tk-daemon/src/protocol.rs`.
