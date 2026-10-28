# Catalog

One folder per entry. Session 0 left this empty; lane E seeded it on 27 Oct 2026.

| Folder | Kind | What |
|---|---|---|
| `exa/`, `context7/`, `hugging-face/`, `granola/`, `vercel/` | server (remote) | Tony's hosted connectors, as pointers to the vendors' own endpoints |
| `shopify-dev/` | server (npm, runs as you) | Shopify's Dev MCP; the library sets its telemetry opt-out |
| `tk-computer-use/`, `docker-mcp-gateway/` | server (host, runs as you) | tk's own: the computer-use helper and the gateway |
| `web-search/`, `library-docs/`, `hf-hub/`, `meeting-notes/`, `deployments/` | skill | Entry skills, one per connector; the text lives in the bundle |
| `tk-connectors/` | bundle | The skills as one Claude Code and Codex plugin |
| `sets.json` | | Named sets the exporters turn into a marketplace, a profile, a Pi package and a registry |
| `brands.json` | | The seed brand list for the lookalike check |

Layout per kind: `<id>/server.json` plus `tools.json`; `<id>/skill.json` with the SKILL.md in the folder the git package names; `<id>/.claude-plugin/plugin.json` with `skills/`. The folder name is the id `tk connect` and the policy file use; it must equal the manifest's short name.

`tools.json` is a record of what the server advertised when it was recorded (`source`, `recordedAt`), not a promise; `dynamic: true` means the server adds tools at run time. Every `contentSha256` here is a placeholder of zeros until wave 2 (lane G computes hashes at publish, lane H keeps the scans).

Validate: `node library/cli.ts validate`. Export: `node library/cli.ts export <set> <out-dir>`.
