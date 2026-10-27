# Contract: the daemon's socket and HTTP receiver

Written by tk session 0, 27 Oct 2026. Lane B implements the server (`crates/tk-daemon`). Lane A (the Pi emitter), lane D (`tk run`), lane F (the hooks' `approve`) and, in wave 2, lane J (`tk watch`, the fork's adapter file) are clients. The Rust types are `crates/tk-daemon/src/protocol.rs`; the recorded exchange is `fixtures/daemon/exchange.jsonl`, parsed by that file's test. From wave 2 on, the `tail` line shape is fixed: the fork's React reads it (lane J).

## 1. Where it listens

| OS | Socket |
|---|---|
| macOS | `~/Library/Application Support/tk/daemon.sock` |
| Linux | `$XDG_RUNTIME_DIR/tk/daemon.sock`, else `~/.tk/run/tk/daemon.sock` |

- The socket file is mode 0600 and its folder 0700. A stale socket file is replaced at start.
- A pid file sits beside it (`daemon.pid`). The daemon refuses to start when the pid in it is alive and is a `tk-daemon` (lane B decides how to check).
- The daemon makes no outbound connection of any kind (rules, section 5, item 8). It listens on this socket and on the loopback HTTP receiver only.
- The daemon's own token is 64 lowercase hex characters (`tk_core::token`), written to `daemon.token` beside the socket, mode 0600. A process that can read that file is the user; the socket's mode already admits only the user. The token stops a confused process from another user's session and makes scope explicit.

## 2. Lines

UTF-8 JSON, one object per line, `\n`-terminated, at most 1 MiB per line (longer: the daemon answers `{"ok":false,"error":"line too long"}` and closes). Every client line has an `op`. The daemon answers every request with exactly one reply line (`ok` true or false); `tail` and `approver` then stream push lines on the same connection until the client closes. Unknown ops and unknown fields are refused (`ok: false`), never ignored.

| op | Needs | Fields | Reply | Then |
|---|---|---|---|---|
| `hello` | first line | `token`, `client` (free text, for the log) | `scope` (`daemon` or `chat`), `chat` when chat scope, `daemon {version, pid, protocol}` | |
| `event` | any scope | `event` (section 3) | `id` | |
| `tail` | daemon scope; chat scope sees only its chat | `since?` (ms), `chat?` | `ok` | `{"event": {...}}` per event, backlog first |
| `report` | daemon scope | `chat?`, `since`, `until` | `report` (lane B defines the object; lane J renders it) | |
| `approve` | any scope | `request {id, chat, harness, tool, summary, timeoutSeconds}` | `answer` (`yes`/`no`), `by` (`approver`, `timeout`, `none`) | |
| `approver` | daemon scope | none | `ok` | `{"approve": {...}}` per request |
| `answer` | an approver connection | `id`, `answer` | `ok` | |
| `token.mint` | daemon scope | `chat`, `harness` | `token` | |

Rules:

1. A first line that is not `hello`, or a `hello` whose token is malformed or unknown, gets one error reply (`hello first`, `bad token`) and the connection closes. The token is compared by its SHA-256 (`tk_core::token::hash`), never logged.
2. A chat-scope connection can post events only into its own chat: the daemon overwrites `chatId` and `harness` with the token's (as the fork's receiver does: `otel_receiver/normalize.rs:1-7`).
3. `approve`: the daemon sends the request to every connected approver; the first `answer` wins and the rest are told nothing more. No approver connected: `no` at once, `by: "none"` (case 63). No answer within `timeoutSeconds`: `no`, `by: "timeout"`. Every request and its outcome is recorded as an event with `source: "approval"`.
4. `token.mint` returns a fresh chat token and stores only its hash. The token goes into a child's environment (`TK_CHAT_TOKEN`) or a header, never into an argument (rules, section 5, item 1).
5. A client that sends a line the daemon cannot parse gets `ok: false` with the parse error and may continue.

## 3. The event

`tk_core::event::Event`, serde camelCase. The fork's `ActivityEvent` (`activity/types.rs` at `159ef3e`) with two sources added.

```json
{ "id": "pi-2f1c...", "at": 1792204985350, "chatId": "chat-b", "harness": "pi",
  "source": "adapter", "kind": "tool.call", "summary": "exa/search ok, 120 ms",
  "detail": { "trust": "client" }, "pid": 4242, "approvalId": "ap-1" }
```

| Field | Meaning |
|---|---|
| `id` | Unique per record. A second record with the same id replaces the first; the daemon keeps the last copy and never reuses an id after a restart (case 35) |
| `at` | Milliseconds since the epoch, stamped by the daemon on receipt. A harness's own time goes in `detail.harnessAt` (case 35) |
| `chatId` | `""` when not attributed |
| `harness` | A harness id from `contracts/harness-presets.json`, or `""` |
| `source` | `otel`, `adapter`, `eslogger`, `auditd`, `cua`, `approval`, `run` |
| `kind` | `tool.call`, `approval`, `model.request`, `model.error`, `prompt`, `session.start`, `message`, `run.start`, `run.stop`, `fs.write`, `net.connect`, `exec`, or `otel.<name>` for an unknown OTel event |
| `summary` | One line, at most 200 characters |
| `detail` | Free JSON, at most 16 KiB, never rendered as markup |

`run.start` and `run.stop` (lane D) carry `detail: {boundary, argv, kit?, host}`; `argv` never holds a token.

## 4. The HTTP OTLP receiver

As in the fork (`otel_receiver.rs` at `159ef3e`): loopback only, a free port, `POST /v1/logs` with JSON or protobuf, the chat token in a header, refused before any of the body is read when the token is unknown, `Host` checked against the listener, bounded (16 connections, 16 KiB head, 4 MiB body, 2,000 records per request). One rename: the header `x-monocode-token` becomes `x-tk-token` (`harness_env.rs:15`). It stands down when the user already exports OTel (`harness_env.rs:40-60`).

The endpoint is published to clients as `TK_OTLP_ENDPOINT` (`http://127.0.0.1:<port>`) by `tk run`, and lane B writes it to `otlp.endpoint` beside the socket for clients started another way.

## 5. Not decided here

- The `report` object's fields (lane B, then lane J).
- Retention: the fork's 7 days (`activity/retention.rs`) stays unless lane B records a reason.
- launchd and systemd units (lane B writes them; the box test and the Mac test install them).
