# Contract: `docker mcp` argument shapes, from the fork

Transcribed by tk session 0, 27 Oct 2026, from the fork at `w3/integration` `159ef3e`: `src/features/gateway-connectors/model/dockerArgs.ts` (508 lines, TypeScript builders) and `src-tauri/src/docker_mcp.rs` (897 lines, the Rust allowlist that runs them; copied to `crates/tk-cli/src/connect/gateway/docker_mcp.rs`). Lane C owns the Rust side. The TypeScript file is not copied; this page is its record.

## 1. Names

| Name | Pattern | Source |
|---|---|---|
| Profile id | `^[a-z0-9_]{1,64}$` | `dockerArgs.ts:24` |
| Own profile id | a profile id starting `monocode_` with something after it | `:20, 29-31` |
| Server name | `^[a-z0-9][a-z0-9._-]{0,99}$` | `:25` |
| Secret name | `^[A-Za-z0-9._-]{1,128}$`, and no `=` for `secret set` | `:26, :296` |
| Config key | `^[a-z0-9][a-z0-9._-]{0,99}\.[A-Za-z0-9_]{1,64}$`, property not secret-like | `:36, 43-47` |
| Config value | at most 1,000 characters, no `\n`, `\r` or NUL | `:40, 49-51` |
| Tool name | `^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$` | `:215` |
| Server whose tools can be switched | a server name with no `.` (the gateway splits `server.tool` at the first dot) | `:219-220` |
| Catalogs | `mcp/docker-mcp-catalog`, `monocode/extra` | `:8, 14` |
| Server ref | `catalog://<catalog>/<server name>` for one of the two catalogs; the Rust side also takes `file://<absolute path>` to a catalog file under the user's Docker MCP folder, no `..`, no control characters, at most 1,024 bytes | `:54-58`; `docker_mcp.rs:354-380` |

For tk, lane C renames the prefix `monocode_` to `tk_` and the user catalog `monocode/extra` to `tk/extra`, and records it. Profiles outside the prefix stay untouchable.

## 2. `SECRET_LIKE`

```
/token|secret|password|passwd|apikey|api_key|private_key/i
```

`dockerArgs.ts:38`. In the fork it tests a **config property name**: a property that matches is refused as config and must be saved as a secret (`:43-47`). It does not match secret **values**.

Lane A's card asks for a `tool_result` redactor "for secret-like strings (the `SECRET_LIKE` pattern)". That pattern alone cannot find a token in text. The reading closest to the plan: redact the value in `key=value`, `key: value` and `"key": "value"` pairs whose key matches `SECRET_LIKE`, plus well-known token shapes (lane A lists the ones it uses, with sources, for example GitHub's `ghp_`, `github_pat_` prefixes and AWS `AKIA` key ids), and record that the redactor is best-effort.

## 3. Allowed argument lists

Each is the whole argv after `docker`. `*` is one value checked by the pattern named. Nothing else runs.

| Verb | argv | Timeout |
|---|---|---|
| version | `mcp version` | 30 s |
| catalog show | `mcp catalog show <catalog> --format json` | 120 s |
| catalog pull | `mcp catalog pull mcp/docker-mcp-catalog` | 120 s |
| profile list | `mcp profile list --format json` | 30 s |
| profile show | `mcp profile show <profile id> --format json` | 30 s |
| profile create | `mcp profile create --name <server name> --id <own id> (--server <server ref>){1,32}` | 30 s |
| profile remove | `mcp profile remove <own id>` | 30 s |
| server add | `mcp profile server add <own id> --server <server ref>` | 30 s |
| server remove | `mcp profile server remove <own id> <server name>` | 30 s |
| config set | `mcp profile config <own id> (--set <config key>=<config value>){1,64}`; a key named as one of the server's secrets is refused | 30 s |
| config get | `mcp profile config <own id> --get-all --format json` | 30 s |
| config del | `mcp profile config <own id> --del <config key>` | 30 s |
| tools on/off | `mcp profile tools <own id> --enable|--disable <server>.<tool>` | 30 s |
| tools reset | `mcp profile tools <own id> --enable-all <server>` | 30 s |
| tools enable list | `mcp profile tools <own id> (--enable <server>.<tool>){1,200}` | 30 s |
| tool list | `mcp tools ls --format json --gateway-arg=--profile --gateway-arg=<own id>` | 180 s |
| dry run | `mcp gateway run --profile <own id> --dry-run --verbose` | 180 s |
| secret list | `mcp secret ls --json` | 30 s |
| secret set | `mcp secret set <secret name>`, the value on stdin only, at most 16 KiB | 30 s |
| secret remove | `mcp secret rm <secret name>` | 30 s |
| OAuth list | `mcp oauth ls --json` | 30 s |
| OAuth authorize | `mcp oauth authorize <server name> --open-browser` | 300 s |
| OAuth revoke | `mcp oauth revoke <server name>` | 30 s |

Timeouts are the Rust constants (`docker_mcp.rs:24-29`, `:154-245`). Everything not listed is refused (`_ => None`).

Not on the run list: `mcp gateway run --profile <profile id>` (`dockerArgs.ts:318-321`). It is the stdio command a harness config gets as its server entry; the app never runs it. `tk connect` writes it into configs the same way.

Secrets go in by stdin only and never appear in an argument, a log or an error message (rules, section 5, item 1). On 10 Oct `docker mcp client connect` printed other servers' keys; `client connect` is not on the list, and `tk connect` writes harness configs itself.

## 4. What lane C must replace in the copied file

`cargo check -p tk-cli --features fork-docker-mcp` at the base gives four errors: `crate::dirs_home` (the fork's `lib.rs:139`), `crate::harness::{exec_output_with_stdin, gui_search_path}` (the fork's `harness.rs`), and two `#[tauri::command]` attributes (`docker_mcp.rs:69, 82`). Lane C supplies the two helpers in `src/connect/` and drops the attributes.
