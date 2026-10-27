# The tk library

A curated subregistry of the official MCP registry, plus skills and bundles, with the control plane's data (`tier`, `runner`, `exposure`, `entrySkill`, `annotationsOverride`, `permissions`, `trust`, `harnessPresets`) under one reverse-DNS key: `_meta["io.modelcontextprotocol.registry/publisher-provided"]["io.github.aetiusgular.tk"]` (`contracts/entry.schema.json`). Not a registry: the official one is designed to be mirrored and says so (`plans/toolkit_layer_plan.md` section 1; `research-notes/AP_tool_library_prior_art.md` section 8). Lane E built the format, the validator, the exports and the seed catalog in wave 1 (T3.1, T3.4); the mirror and the publish flow (lane G), the trust records (lane H) and the browser (lane J) come in wave 2.

## The inclusion rule

An entry is in the library when all of these hold:

1. **Permissive licence.** MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, 0BSD, MPL-2.0, Unlicense, CC0-1.0, Zlib or BSL-1.0 (Docker's and ToolHive's rule; plan section 11, call 4). The validator checks the `license` field of skills and bundles; a server's source licence is checked at review, since `server.json` has no licence field.
2. **It validates.** `node library/cli.ts validate` exits 0: upstream's schema and semantic rules, the Agent Skills specification for every `SKILL.md`, and the library's rules below. Warnings are shown on the card; errors keep the entry out.
3. **No install-time execution, no remote code loading, no endpoint or sandbox key.** The reject list of `contracts/policy.md` section 3.1 (`ANTHROPIC_BASE_URL`, `chatgpt_base_url`, `settings.local.json`, `dangerouslyDisableSandbox`, `approval_policy`, `--dangerously-*`), `postinstall` and `prepare` scripts, "Prerequisites: download", `curl | sh`, `eval`, `new Function`, network `import()`, in any letter case or Unicode spelling. These are the mechanisms of every 2025 and 2026 marketplace incident on record (note AP section 5).
4. **No literal secret.** Headers, environment variables and config carry `${VAR}` references, never a value (rules section 5, item 1).
5. **It says where it runs.** `runner` is an image (pinned by digest), an `srt` profile, `remote`, or `host` with the note `runs as you`; a toggle never starts a bare host process without the card saying so (plan R2).
6. **One name, one tool.** Server ids are `[a-z0-9-]` (Gemini splits at `_`); tool names are unique across the catalog once a harness has mangled them to `[a-z0-9_]` (Pi, OpenCode, ToolHive vMCP); every tool has a `readOnlyHint`, from the server or from `annotationsOverride`, because the spec makes clients treat server annotations as untrusted and the gate's plan turn needs the hint.
7. **It does not look like someone else.** The id is checked against the seed brand list (`catalog/brands.json`) by folding, edit distance and suffix; a pointer at a brand's own host, image or package is that brand's service, whoever published the pointer (corner case 59).
8. **Deferred by default.** `exposure.default` is `deferred` or `codemode` unless the set is small; an `entrySkill` of about 100 tokens introduces the tools (plan R3).

Trust records (`trust`) are shown with their dates and never called a verdict (plan R7). In wave 1 every `contentSha256` is a placeholder and `scans` is empty; lane G and lane H fill them.

## Layout

```
library/
  schema/     the vendored upstream schema, ToolHive's, and tk's own (schema/README.md)
  validate/   the validator: schemas.ts, upstream.ts, skill-md.ts, rejects.ts, names.ts, lookalikes.ts, catalog.ts, index.ts
  export/     the exporters: index.ts, yaml.ts
  catalog/    the entries, sets.json, brands.json (catalog/README.md)
  cli.ts      node library/cli.ts validate [catalog-dir] | export <set|all> <out-dir> [catalog-dir]
```

Runs on Node 22.18 or later with type stripping; no build. Dependencies: `ajv` 8.20.0 (the workspace's) and `yaml` 2.9.1 (no dependencies; parses SKILL.md frontmatter and round-trips the Docker profile in tests).

## The validator

Rule names are stable and tested one by one (`validate/validate.test.ts`, `validate/rules.test.ts`):

| Family | Rules |
|---|---|
| `schema/*` | `schema/upstream` (the vendored 2025-12-11 schema, draft-07), `schema/tk-server`, `schema/tk-skill`, `schema/tk-bundle`, `schema/tools`, `schema/sets`, `schema/marketplace` |
| `upstream/*` | The registry's semantic checks transcribed from `internal/validators/validators.go` at `9c4a96c`: `server-name`, `version`, `repository`, `package`, `remote`, `meta-size` (the 4 KB publish limit on `_meta`, a warning here). `upstream/online/*` when `TK_LIBRARY_ONLINE=1` calls `POST /v0.1/validate` |
| `skill/*` | The Agent Skills specification as `skills-ref` 0.1.0 applies it: `frontmatter`, `unexpected-field`, `name`, `dir-name`, `description`, `compatibility`, `metadata`, `allowed-tools`, `missing`, `name-matches-index`, `empty-body`; `skill/skills-ref` when `TK_LIBRARY_SKILLS_REF=1` and the binary is on PATH |
| `lib/*` | `meta`, `id`, `id-matches-name`, `reject`, `literal-secret`, `image-digest`, `entry-skill`, `liveness`, `tools-record`, `tool-name-charset`, `tool-name-unique`, `annotations`, `annotations-override`, `output-schema`, `exposure-direct`, `host-process`, `license`, `lookalike`, `server-id-unique`, `allowed-tools-known`, `set-entry` |

Nothing reads the network or runs a program unless the two environment variables above turn it on.

## Exports

`node library/cli.ts export <set> <out-dir>` writes, per set (`export/index.ts`):

| Folder | Format | Read by | Check |
|---|---|---|---|
| `claude/` | A marketplace root: `.claude-plugin/marketplace.json` and one generated plugin per server (`mcpServers` in `plugin.json`); bundles as `git-subdir` sources pinned to the export's commit | Claude Code, Codex | `claude plugin marketplace add <out>/<set>/claude` (box test) |
| `pi/` | A Pi package per bundle (`package.json` with `pi.skills`) and an `mcp.json` fragment of the servers with `exposure` and `description` | Pi 1.4.0 | `pi install ./<bundle>`; `pi mcp list` (box test) |
| `docker/` | A gateway profile, version 1, YAML and JSON: remotes as `type: remote`, images as `type: image` with a snapshot and the pinned digest, the recorded tool names as `tools`; host runners left out and named in its README | Docker MCP Gateway 0.44 | `docker mcp profile import` (box test) |
| `toolhive/` | `registry.json` in ToolHive's upstream envelope (`version`, `meta.last_updated`, `data.servers[]`, `data.skills[]`) | `thv config set-registry` | not run; the box test |
| `registry.json` | The official registry's list shape (`servers[].server`, `_meta[...official]`, `metadata.count`) | anything that reads `/v0.1/servers` | export.test.ts |

The exporter refuses a catalog with errors. Git sources are pinned to `git rev-parse HEAD` of the checkout the export was made from; outside a checkout the placeholder sha is written and a warning printed.
