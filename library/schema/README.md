# Schemas

| File | What | Source |
|---|---|---|
| `server.schema.json` | The official MCP registry's `server.json` schema, version 2025-12-11, draft-07. Vendored byte for byte | `modelcontextprotocol/registry` at `9c4a96c90fbcd3c3a169cdcd5443419c7a0c39d7` (27 Oct 2026), `internal/validators/schemas/2025-12-11.json`, sha256 `2e1e2dc758185bfc91023df4e5ace0e1c1bdb7b7b3d6614bb9ac6b9e9a9c2f68`. The registry is MIT and Apache-2.0 (its LICENSE); the published URL is `https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json`, which the cloud workspace could not reach. A `NOTICE` line is a request for integration (lane E does not edit `NOTICE`) |
| `thv-registry.schema.json`, `thv-skill.schema.json`, `thv-plugin.schema.json` | ToolHive's registry envelope, skill and plugin schemas, 2020-12. Vendored byte for byte | `stacklok/toolhive-core`, `registry/types/data/`, fetched 27 Oct 2026 from raw.githubusercontent.com `main`; sha256 `7763fe17…`, `84f7c1c2…`, `e0f68698…` (full values in the lane E status doc). Apache-2.0 |
| `tk-server.schema.json` | What the library requires of a server entry on top of upstream: the `$schema` pin, the tk `_meta` object (contract `entry.schema.json`, kind `server`), a package or a remote | lane E |
| `tk-skill.schema.json` | ToolHive's `skill.json` fields plus the tk `_meta` object, kind `skill` | lane E, from `thv-skill.schema.json` and `contracts/shapes/skill.schema.json` |
| `tk-bundle.schema.json` | Claude Code's `plugin.json` fields plus the tk object at `metadata`, kind `bundle` | lane E, from `contracts/shapes/plugin.schema.json` and the manifest reference session 0 read |
| `tools.schema.json` | The `tools.json` record beside a `server.json` | lane E |
| `docker-profile.schema.json` | Docker MCP Gateway profile, version 1 | lane E, from `docker/mcp-gateway` at `7e0f34277c91855ad861eeff536b4fdc77420593`, `pkg/workingset/workingset.go` and `docs/profiles.md` |
| `sets.schema.json` | `library/catalog/sets.json` | lane E |

The upstream schema is checked with a draft-07 Ajv in non-strict mode (it carries `example` keywords); everything tk writes is 2020-12 and checked strict. `library/validate/schemas.ts` loads them.
