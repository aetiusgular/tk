# library/mirror

The hourly mirror of the official MCP registry (toolkit plan T3.2, lane G).
tk's library is a subregistry: it copies the registry's servers, keeps their
status current, records liveness and flags, and writes a tk entry for each
latest version. It runs in CI on a schedule (`ci/mirror.yml`, a proposal
integration installs), never from the daemon.

```
node library/mirror/cli.ts pull --out <dir> [--full] [--no-probe] [--probe-budget N]
                                [--cap N] [--base-url <https url>] [--max-pages N] [--attempts N]
node library/mirror/cli.ts probe <url>
```

## What it calls

- `GET https://registry.modelcontextprotocol.io/v0.1/servers` with `limit=100`
  and `updated_since` (or `include_deleted=true` on a full pull), one request a
  second. The registry allows 600 list requests a minute per source IP per
  replica and 180 elsewhere (`deploy/pkg/k8s/registry.go:330-336, 403-407` at
  `9c4a96c`); a 429 or 5xx is retried after `Retry-After` or a doubling wait.
- With probes on (the default), each active latest remote's own host: a `HEAD`
  and a `server/discover` POST (MCP 2026-07-28), at most 2,000 URLs a run, each
  URL at most once a day. `--no-probe` turns this off. No credential is ever
  sent; hosts that resolve to a private or special-purpose address, IP
  literals, templated URLs and anything but https are never dialled, and
  redirects are never followed (`netguard.ts`, after the registry's own
  well-known fetcher, `internal/api/handlers/v0/auth/http.go`).

## What it writes

| File | Holds |
|---|---|
| `state.json` | the source, the watermark (the greatest `updatedAt` of a finished pull), the last runs, counts |
| `servers/<name>/<version>.json` | each version as the registry served it: the publisher's `server` and the registry's `_meta[official]` |
| `entries/<name>.json` | tk's entry for the latest version: the publisher's server.json, any object under tk's own key removed, tk's record (`tier: mirrored`) added |
| `liveness.json` | the last probe of each remote URL: state, reason, HTTP statuses, era; a URL the probe refuses is recorded `unknown`, "not probed: <why>", outside the budget |
| `index.json` | per server: latest version, version count, status, liveness, flags, `missingUpstream`; and `bulkPublishers`, each over-cap publisher and day with its count, kept once here so one more version does not rewrite every row of that publisher |
| `skipped.json` | records the last run refused, with the reason |

Names are percent-encoded, upper-case letters included, so names that differ
only in case never share a path on a case-insensitive disk. Every file is
written in one form (sorted keys, two-space indent), so a commit diffs only
what changed, and the branch's history is the drift record. Nothing is
deleted: a deleted server keeps its metadata with `status: deleted`, and a
server a full pull no longer returns is flagged `missingUpstream`.

## Rules it follows

- **`isLatest` is recomputed.** The registry clears the old version's
  `is_latest` without moving its `updated_at` (`internal/database/postgres.go:996`),
  so `updated_since` never returns that change; the latest is the marked version
  published last. A weekly full pull resets anything an incremental pull missed.
- **A ten-minute overlap** is re-read before the watermark on every incremental
  pull, because the filter is strict (`updated_at > since`) and rows commit out
  of order (inference). Reading a version twice changes nothing.
- **The watermark moves only when every page arrived.** A failed run keeps the
  files it wrote and the next run re-reads them.
- **A publisher's object under `io.github.aetiusgular.tk`** (in any spelling
  that folds to it) is removed and flagged `tk-key-removed`; tk's own record is
  the only one under that key.
- **Flags, never verdicts:** `duplicate-remote`, `duplicate-description` (across
  publishers), `bulk-publisher` (more than `--cap`, default 20, versions first
  published by one publisher in one UTC day; the row names the publisher, day
  and cap, the count is in `bulkPublishers`), `tk-key-removed`, `reject-list`
  (the server.json names a key of `contracts/policy.md` section 3.1 or
  install-time execution, as `library/validate/rejects.ts` reads them).
- **Liveness** uses the contract's states: `alive` when the remote answers
  (a result, a JSON-RPC error, 401 or 403, a 4xx from a legacy server, a
  redirect), `dead` on a network failure, a 5xx, a tunnel's error page or a 404
  without a JSON-RPC body, `unknown` when not probed. A name with IPv6 and IPv4
  addresses gets each address in turn (Node's family autoselection), so a
  broken IPv6 path does not make a live server dead. The budget
  (`--probe-budget`) goes to URLs a probe would dial, oldest first.
