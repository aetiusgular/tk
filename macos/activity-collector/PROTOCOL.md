# Activity collector protocol

Wave 3, lane B. The contract between MonoCode (`src-tauri/src/activity/`) and
the collector daemon in this folder. Both sides build against this file.

Detection only. Nothing here blocks an action.

## Processes and files

| What | Where | Owner, mode |
|---|---|---|
| The daemon | `/Library/PrivilegedHelperTools/com.monocode.desktop.activity-collector` | root:wheel 0755 |
| Its launchd job | `/Library/LaunchDaemons/com.monocode.desktop.activity-collector.plist`, label `com.monocode.desktop.activity-collector` | root:wheel 0644 |
| Sockets | `/var/run/monocode-activity/register.sock`, `/var/run/monocode-activity/read.sock` | folder root:wheel 0755, sockets 0666 (every caller is checked, see below) |
| Log | `/Library/Application Support/MonoCode/activity/es-YYYY-MM-DD.jsonl`, rolled to `.jsonl.gz` | folder root:wheel 0700, files 0600 |

When installed through `SMAppService`, the binary and plist live in the app
bundle and launchd runs them from there; the sockets and log are the same.

The daemon starts `/usr/bin/eslogger` with an argument list (never a shell
string) and a fixed event list:

`exec fork exit create rename unlink close open tcc_modify truncate link copyfile clone exchangedata`

`write` is never asked for: too many events, with no way to select
processes (note AA, appendix A).

At start it runs `/usr/bin/eslogger --list-events` and drops any of these the
running macOS does not offer, recording which in its status. `open` events are
kept only for sensitive paths; `close` only when the file was modified (it
stands in for writes).

## Caller check

Both sockets accept a connection only from MonoCode. On macOS the daemon reads
the peer's audit token (`LOCAL_PEERTOKEN`) and checks its code signature
against a requirement built from the daemon's own designated requirement, with
the identifier swapped for the app's (`com.monocode.desktop`). So the peer must
be signed by the same certificate as the daemon. A daemon whose own
requirement cannot be read, or does not start with `identifier "<own id>" and `,
refuses every caller (fail closed).

Development builds are unsigned. Only for them, the plist may carry
`--dev-peer-path <absolute path>`: a peer whose executable is exactly that path
is accepted. The plist is root-owned, written by an install the user ran.
This is weaker than the signature check and the install says so.

## Registration socket

Newline-delimited JSON, one request per line, at most 4,096 bytes a line, at
most 64 lines a connection. Each request gets one reply line:
`{"ok":true}` or `{"ok":false,"error":"<short reason>"}`. A malformed line is
answered with an error and the connection closed.

```json
{"v":1,"op":"register","pid":4242,"chatId":"chat-1","kind":"harness","at":1760000000000}
{"v":1,"op":"grant","chatId":"chat-1","bundleId":"com.apple.calculator","appPath":"/System/Applications/Calculator.app","at":1760000000000}
{"v":1,"op":"grantEnded","chatId":"chat-1","bundleId":"com.apple.calculator","at":1760000005000}
```

- `kind` is `harness` or `cuaRuntime`.
- `chatId`: 1 to 128 characters from `[A-Za-z0-9_.:-]`.
- `bundleId`: 1 to 255 characters from `[A-Za-z0-9.-]`.
- `appPath`: absolute, ends in `.app`, no `..`, no NUL, at most 1,024 bytes. Optional.
- There is no `unregister`. The daemon learns exits from `eslogger`, so an
  agent that reached the socket could not stop its own tracking.
- At most 4,096 tracked roots and 64 open grants. Beyond that, a request is
  refused with `"full"`.

## What the daemon follows

- A registered PID and every descendant, through `fork` and `exec`, until it
  exits.
- During a grant, any process whose executable sits under
  `<appPath>/Contents/`, and its descendants, attributed to the grant's chat.
  A grant without `appPath` records nothing.
- Everything else is dropped before it is written.

## Log lines

One JSON object a line:

```json
{"seq":12,"prev":"<64 hex>","hash":"<64 hex>","event":{ ...ActivityEvent... }}
```

- `event` is an `ActivityEvent` (`src-tauri/src/activity/types.rs`) with
  `source` `"eslogger"`, `harness` `""` (the app fills it from the chat), and
  `id` `es-<YYYY-MM-DD>-<seq>`.
- `hash` is the lowercase hex SHA-256 of `prev`, a `\n`, then
  `serde_json::to_string` of `event` read as a `serde_json::Value`.
- The first line of a day has `prev` equal to the last `hash` of the day
  before, or 64 zeros.
- The chain shows a cut or edited file. It cannot stop root rewriting a whole
  file with a fresh chain.

`kind` values: `exec`, `fork`, `exit`, `create`, `write` (a modified close),
`rename`, `unlink`, `open` (sensitive path), `tcc`, `truncate`, `link`,
`copyfile`, `clone`, `exchangedata`. `detail` holds `path`, `dest`, `argv`
(exec, at most 32 items of 512 bytes), `ppid`, `exe`, `grant` (the bundle id,
when attributed by a grant), each only when known, and:

- `child` (fork only): the new process's pid. A fork is logged under the
  parent: `pid` is the process that forked, `detail.child` the one it made.
- `originalPpid`, `groupId`, `sessionId`: `es_process_t`'s `original_ppid`,
  `group_id` and `session_id`, so a process reparented to launchd (`ppid` 1)
  can still be placed.
- `tier`: `root` when the acting pid is a registered root, `child` for a
  descendant or a process followed through a grant. The app's check splits
  `child` further; the collector never filters on it.

Paths per kind (Apple's `es_event_*_t` members): `truncate` `path` =
`target`; `link` `path` = `source`, `dest` = `target_dir` + `target_filename`;
`copyfile` `path` = `source`, `dest` = `target_file` or `target_dir` +
`target_name`; `clone` `path` = `source`, `dest` = `target_dir` +
`target_name`; `exchangedata` `path` = `file1`, `dest` = `file2`.

## Read socket

Same framing. One request a connection.

```json
{"v":1,"op":"events","since":1760000000000,"until":1760086400000,"chatId":"chat-1","limit":2000}
{"v":1,"op":"status"}
```

- `events` answers with one `ActivityEvent` a line, oldest first, then
  `{"end":true,"truncated":<bool>,"chainOk":<bool>}`. `limit` is at most 10,000.
  When more events match than the limit, the NEWEST `limit` are sent and
  `truncated` is true.
- `status` answers with one line:
  `{"version":"…","eslogger":"running"|"stopped"|"missing","esloggerError":"…","events":[…],"missingEvents":[…],"tracked":<n>,"grants":<n>,"dropped":<n>,"logBytes":<n>}`.
  `esloggerError` is eslogger's last stderr line (control characters
  dropped, at most 300 characters), present only while it is not running.

Socket paths must be under 104 bytes, `sun_path` on macOS; the daemon
refuses a longer one with a clear error.

## Retention

7 days of logs, a 1 GB cap with the oldest day deleted first, each day
compressed when it rolls over. Screenshots are not the collector's: they stay
in the app's log folder.

## Clarifications, as built (16 Oct)

What the first draft left open, settled by the collector's code and tests:

1. `tcc` events also carry `service` and `identity` in `detail`, as eslogger
   names them. The least certain shape of all; the Mac test checks it.
2. An `events` request without `chatId` returns every chat. Without `limit`,
   the limit is 10,000. `since` after `until` is refused with `"bad range"`.
3. `grantEnded` also stops following the processes the grant attributed.
4. A refused caller gets one `{"ok":false,"error":"refused"}` line, then the
   connection closes. At most 8 connections at once; more are closed with no
   reply.
5. An event dated before the writer's current day goes into the current
   day's file, so day files stay in order.
6. Retention never deletes today's file or the file the writer has open.
7. `chainOk` checks every file in the range and its link to the day before.
8. After a restart the writer continues from the last line that parses, so a
   sequence number never repeats after an edited line. A cut tail gets a
   newline first, so the cut stays visible.
9. `/private/etc/sudoers` is sensitive (the real path behind `/etc`). Chrome's
   `Cookies` and `Login Data` match at any depth under Chrome's folder.
10. `verify <file>` prints `{"ok","lines","firstBadLine","cut"}` and exits 1
    when the chain is broken. `parse-fixture <file> --pid <n> --chat <id>
    [--grant <chat> <bundle> <appPath>]` prints what a tracker would log, for
    the Mac test's real capture.
