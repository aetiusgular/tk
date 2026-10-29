# Contract: the daemon's socket and HTTP receiver

Written by tk session 0, 27 Oct 2026; brought to what lane B built, and to integration w1's decisions, by integration w1 the same day. Lane B implements the server (`crates/tk-daemon`). Lane A (the Pi emitter), lane D (`tk run`), lane F (the hooks' `approve`) and, in wave 2, lane J (`tk watch`, the fork's adapter file) are clients. The Rust types are `crates/tk-daemon/src/protocol.rs`; the recorded exchange is `fixtures/daemon/exchange.jsonl`, parsed by that file's test. From wave 2 on, the `tail` line shape is fixed: the fork's React reads it (lane J).

## 1. Where it listens

| OS | Socket |
|---|---|
| macOS | `~/Library/Application Support/tk/daemon.sock` |
| Linux | `$XDG_RUNTIME_DIR/tk/daemon.sock`, else `~/.tk/run/tk/daemon.sock` |

Beside the socket: `daemon.pid`, `daemon.token`, `otlp.endpoint`, each 0600, and on Linux `chats/<chat>.token` while a `tk run --boundary none` chat runs (section 2, rule 4). The activity log is elsewhere on Linux: `$XDG_DATA_HOME/tk/activity`, else `~/.local/share/tk/activity`. On macOS everything is under `~/Library/Application Support/tk/`. `tk-daemon --foreground [--socket PATH] [--data-dir PATH]`; the two flags are for tests.

- The socket file is mode 0600 and its folder 0700, owned by the user; a folder that is a link or another user's is refused. A stale socket file is replaced at start.
- One daemon per user: `daemon.pid` is held with `flock` for the daemon's life (lane B), so a second start is refused and a killed daemon's successor starts.
- The daemon makes no outbound connection of any kind (rules, section 5, item 8). It listens on this socket and on the loopback HTTP receiver only.
- The daemon's own token is 64 lowercase hex characters (`tk_core::token`), written to `daemon.token` beside the socket, mode 0600, new on every start: a long-lived client re-reads it after a restart. A process that can read that file is the user; the socket's mode already admits only the user. The token stops a confused process from another user's session and makes scope explicit. Only `tk run`, `tk watch`, `tk-audit-feed` and the daemon's own tools present it; a hook never does (section 2, rule 4).

## 2. Lines

UTF-8 JSON, one object per line, `\n`-terminated, at most 1 MiB per line (longer: the daemon answers `{"ok":false,"error":"line too long"}` and closes). Every client line has an `op`. The daemon answers every request with exactly one reply line (`ok` true or false); `tail` and `approver` then stream push lines on the same connection until the client closes. Unknown ops and unknown fields are refused (`ok: false`), never ignored.

| op | Needs | Fields | Reply | Then |
|---|---|---|---|---|
| `hello` | first line | `token`, `client` (free text, for the log) | `scope` (`daemon` or `chat`), `chat` when chat scope, `daemon {version, pid, protocol}` | |
| `event` | any scope | `event` (section 3) | `id` | |
| `tail` | daemon scope; a chat scope must name its own chat | `since?` (ms), `chat?` | `ok` | `{"event": {...}}` per event, backlog first |
| `report` | daemon scope | `chat?`, `since`, `until` | `report` (below) | |
| `approve` | any scope | `request {id, chat, harness, tool, summary, timeoutSeconds}` | `answer` (`yes`/`no`), `by` (`approver`, `timeout`, `none`) | |
| `approver` | daemon scope, and a peer that passes the check in rule 3 | none | `ok`, or `approver refused: <why>` | `{"approve": {...}}` per request |
| `answer` | an approver connection | `id`, `answer`, `chat` (needed only while two chats wait on one id) | `ok` | |
| `token.mint` | daemon scope | `chat`, `harness` | `token` | |
| `cu` | daemon scope | `action` (`status`, `stop`, `stop-all`, `revoke`), `chat` for `stop` and `revoke`, `app` (a bundle id) for `revoke` | `report` (the service's status or stop report) | |

Rules:

1. A first line that is not `hello`, or a `hello` whose token is malformed or unknown, gets one error reply (`hello first`, `bad token`) and the connection closes. The token is compared by its SHA-256 (`tk_core::token::hash`), never logged. A connection has 10 seconds to say `hello`; at most 64 connections at once.
2. **What a chat token may do.** A chat token sits in the harness's environment and its tools' shells, so it is the agent's too (rules case 66). It posts only `source` `adapter` and `otel` events, only into its own chat: the daemon overwrites `chatId` and `harness` with the token's (as the fork's receiver does: `otel_receiver/normalize.rs:1-7`), and a record replaces an earlier one with the same id only inside one chat. A gate's decision (`kind: "approval"` from `adapter` or `otel`) is written once per chat and id: readers keep the first copy (integration w2, security finding S9; the hook and the agent share the token). Because the agent can post first, a chat-scope post under the id of one still in the ring is never dropped: it is kept as `<id>-conflict-<uuid>` with `detail.conflictsWith`, and the daemon writes a `gate.conflict` record (source `approval`, which only it writes) naming both, which `tk watch report` prints as `GATE CONFLICT` (integration w2's review, W2-S3). An `adapter` decision naming the same call (`detail.toolUseId`) as one held in the chat is a conflict too, whatever its id, since decision ids end in random bits (fix pass 1's review, FP1-S4); then `conflictsWith` and the flag's `detail.id` name the decision held, and the flag's `detail.posted` the id the second one was posted under. It may `approve` and `tail` its own chat. It may not `mint`, `report`, become an `approver`, post `run`, `auditd`, `eslogger`, `cua` or `approval` records, or name its chat's folders. The daemon scope may post anything but `approval`.
3. `approver` (integration w2, security finding S2, part a): the daemon reads the socket peer when it accepts the connection (`SO_PEERCRED` and, on Linux 6.5 and later, `SO_PEERPIDFD`; `LOCAL_PEERCRED` and `LOCAL_PEERPID` on macOS, whose pid is the socket's last user, not necessarily the connecting process: XNU `uipc_socket.c`, lane J). It refuses an approver when it cannot read the peer, when the peer's uid is not its own, when the peer is a running chat's harness (the `pid` of a daemon-scope `run.start` until its `run.stop`) or descends from one, when an ancestor's program is named `tk` (a chat's shell, maybe from before a restart), or when the pidfd says the connecting process has exited, or the peer's pid no longer has the start time read when the connection was accepted (a child that connected and exited; integration w2's review, W2-S5). Every registration and refusal is an `approval`-source event of kind `approver` with `detail.peer` `{pid, uid, program, startTime, pidfd}`, and each answered request's record carries `detail.approver`. In-process clients (the collector, computer use) are not checked. This does not stop an agent that leaves its chat's tree (a double fork, `setsid`, tmux, `systemd-run --user`); S2 stays open until the approver proves a person or agents run as another user.
4. `approve`: the daemon sends the request to every connected approver; the first `answer` wins and the rest are told nothing more. No approver connected: `no` at once, `by: "none"` (case 63). No answer within `timeoutSeconds` (held to 1 to 3600): `no`, `by: "timeout"`. Waiting requests are keyed by (chat, id) (integration w2, security finding S8): a duplicate waiting id in the same chat, a 17th waiting request from one chat, or a 257th overall: `no` at once; another chat's request under the same id waits on its own, and an `answer` without `chat` is refused while two chats wait on that id. Every request and its outcome is recorded as an event with `source: "approval"`, one record per request id (`approval-<uuid>`, made by the daemon).
5. `token.mint` returns a fresh chat token and stores only its hash. Since integration w2 (box finding BX8) the daemon keeps `chats.json` beside its socket (0600, written through a new file and a rename): the live tokens' SHA-256s with chat, harness and mint time, each running chat's harness pid and start time (and, since the w2 fix pass, its `run.start`'s harness, boundary, time and full-access flags under `info`, each cut, for `report.running`), and the OTLP port, never a token. At start it takes back a chat's tokens while that chat's harness still runs (the same pid with the same start time), and a token minted in the last hour whose chat never started; a chat that started and whose harness has gone does not come back, and a run without its recorded start time or with a pid of 1 or less never counts as running (integration w2's review, W2-S9). The file is opened without following a link and checked on the handle. It binds the old OTLP port again when that port is free, so a running harness's `TK_OTLP_ENDPOINT` still reaches it; when it is not free the chat's OTLP feed ends there (its socket posts still work). A state file that is a link, not this user's, or readable by others is ignored. A daemon-scope `run.stop` for a chat revokes every token minted for that chat, for new hellos and OTLP posts; a connection already greeted keeps its scope until it closes (integration w1). The token goes into a child's environment (`TK_CHAT_TOKEN`), a 0600 file named by `TK_CHAT_TOKEN_FILE`, or a header, never into an argument (rules, section 5, item 1). A hook presents only that chat token, never `daemon.token` (integration w1; `contracts/policy.md` section 5).
6. A client that sends a line the daemon cannot parse gets `ok: false` with the parse error and may continue.

The `report` object (lane B): `{since, until, chat, events, flags, bySource, byKind, approvals, chainOk, truncated, dropped, check}`. `events` and `flags` are counts; `approvals` counts `"<answer>:<by>"`; `chainOk` is the log's hash chain over the window; `check` is the fork's `FlagsReport` (the flags, the events they name, the expected rows, the suppressed counts). Since integration w2's fix pass (box test w2, BW3) it also holds `running`: the chats `tk run` started that have not stopped, from the daemon's own list (not the window), each `{chat, pid, alive, harness, boundary, since, fullAccessFlags}`, where `alive` means the pid still has the start time recorded at `run.start`, and `since` is when the daemon received it. With `chat`, only that chat. `tk status` reads it.

`tail`: with `since`, the backlog is the log on disk plus the in-memory ring of the last 4,096 events, the last copy per (chat, id), at most 5,000; without `since`, live events only. A tail more than 4,096 lines behind is dropped; ingest never waits for a reader.

Variables `tk run` sets in the harness (lane D, integration w1), each removed from what the child inherits first: `TK_CHAT_ID`, `TK_HARNESS`, `TK_BOUNDARY`, `TK_CHAT_TOKEN`, `TK_CHAT_TOKEN_FILE` (`--boundary none` only), `GEMINI_CLI_TK_CHAT_TOKEN_FILE` (the same path, Gemini only, because Gemini's env redaction drops names holding `TOKEN` and keeps `GEMINI_CLI_*`; lane I, integration w2), `TK_OTLP_ENDPOINT`, `TK_GATEWAY_URL`. Readers may also honour `TK_POLICY` and `TK_DAEMON_SOCKET`; a hook refuses either when it points inside the session folder, and since integration w2 refuses any daemon socket path inside the session folder or relative, whichever variable produced it (`XDG_RUNTIME_DIR` too; security finding S14).

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
| `harness` | A harness id from the preset table (`crates/tk-cli/src/connect/presets/`), or `""` |
| `source` | `otel`, `adapter`, `eslogger`, `auditd`, `cua`, `approval`, `run` |
| `kind` | `tool.call`, `approval`, `model.request`, `model.error`, `prompt`, `session.start`, `message`, `run.start`, `run.stop`, `net.connect`, `config.write`, or `otel.<name>` for an unknown OTel event; and for the kernel feeds (`eslogger`, `auditd`) the macOS collector's kinds, which the daemon's check reads: `exec`, `fork`, `exit`, `write`, `create`, `open`, `truncate`, `rename`, `unlink`, `link`. `fs.write` and `exec` as session 0 named them are not what the check reads (lane B, lane D) |
| `summary` | One line, at most 200 characters |
| `detail` | Free JSON, at most 16 KiB, never rendered as markup |

`run.start` and `run.stop` (lane D) carry `detail: {boundary, argv, kit, host, cwd, folders, stateDirs, sandboxName, runsAs, dangerousFlags, trust}`, and `run.stop` adds `exitCode`, `signal` and `durationMs`, and `daemonRestarted: true` when the daemon restarted during the run and `tk run` posted the stop with the token file read again (integration w1 fix pass, box finding BX8). Since integration w2 the chat's tokens and, when free, its OTLP port survive the restart (section 2, rule 5), so the feed goes on. `argv` never holds a token, and holds the harness's own arguments only as flag names and lengths (`-p`, `[25 chars]`) unless `tk run` has `TK_LOG_CONTENT=1` (integration w1 fix pass, Mac finding M8); tk's own part (the wrapper, its flags, the harness program) is kept. `detail.host` is the machine, not a network host. A daemon-scope `run.start` sets the chat's folders from `detail.cwd` and `detail.folders` (absolute paths) for the check's `writeOutsideFolders`; without them every write counts as outside.

Kernel feed records (`auditd` from `tk-audit-feed`, lane D; `eslogger` from the Mac collector, wave 2) carry the collector's detail keys (`path`, `dest`, `exe`, `argv`, `child`, `ppid`, `tier`) and the event's `pid`; `auditd` records also carry `detail.preflags` (`writeOutside`, `sensitive`, `trustedFile`, `newHost`, `denied`), the feed's own marks, which the daemon's check does not rely on. Ids are `auditd-<ms>-<serial>`, so re-reading a log replaces records.

`config.write` (lane C, `tk connect --write`): `source: "adapter"`, `detail: {path, server, action, sha256Before, sha256After, backup, origin}`, posted with the daemon token when the daemon runs, else written to `~/.tk/connect.log`. Seen in the daemon's tail by integration w1's cross-lane run.

## 4. The HTTP OTLP receiver

As in the fork (`otel_receiver.rs` at `159ef3e`): loopback only, a free port, `POST /v1/logs` with JSON or protobuf, the chat token in a header, refused before any of the body is read when the token is unknown, `Host` checked against the listener, bounded (16 connections, 16 KiB head, 4 MiB body, 2,000 records per request). One rename: the header `x-monocode-token` becomes `x-tk-token` (`harness_env.rs:15`). It stands down when the user already exports OTel (`harness_env.rs:40-60`).

The endpoint is published to clients as `TK_OTLP_ENDPOINT` (`http://127.0.0.1:<port>`) by `tk run`, and lane B writes it to `otlp.endpoint` beside the socket for clients started another way.

## 5. Not decided here

- Retention: the fork's 7 days of logs, 3 of screenshots and a 1 GiB cap stay (`activity/retention.rs`); lane B also runs it hourly.
- The units are written (`crates/tk-daemon/units/`); the box test and the Mac test install them.
- Persisting chat-token hashes across a daemon restart (lane B: a small follow-up).
- An OTLP endpoint for a harness inside sbx: `host.docker.internal` is not loopback, so lane A's emitter refuses it and a Pi chat under sbx has the sbx policy log as its only feed in wave 1 (integration w1 decision; the box test records whether sbx keeps the `Host` header).
