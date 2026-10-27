# Contract: the events lane A emits, and the adapter record

Written by tk session 0, 27 Oct 2026, from the fork's receiver at `159ef3e` (`src-tauri/src/otel_receiver/decode.rs`, `normalize.rs`) and its recorded fixture `tests/fixtures/otlp/claude-2.1.302-logs.json` (copied into this repository). Lane A emits; lane B decodes.

## 1. How the receiver reads a record (read in code)

- The event name is the `event.name` attribute if present, else the record's `eventName`, else the body when it is a string (`decode.rs:25-57`).
- Claude Code 2.1.302 sends the **short** name in `event.name` (`tool_result`) and the prefixed name in the body (`claude_code.tool_result`): every record in the fixture does.
- `normalize.rs:42-46` strips `claude_code.` and `codex.` only. A name `pi.tool_result` with no short `event.name` would land as kind `otel.pi.tool_result`.
- `describe` (`normalize.rs:106-170`) maps short names to kinds: `tool_result` to `tool.call`, `tool_decision` to `approval`, `api_request` to `model.request`, `api_error` to `model.error`, `user_prompt` to `prompt`, `conversation_starts` to `session.start`, `assistant_response` to `message`.
- Attributes starting `user.` or `organization.` and `account_id` are dropped; `prompt`, `prompt_text`, `user_prompt`, `response` and `*.prompt` are dropped unless content logging is on (`normalize.rs:28-40`).
- The receive time is the event's time; the record's own time goes to `detail.harnessAt` (`normalize.rs:206-211`, review S4).

## 2. What lane A emits

OTLP JSON logs (`POST $TK_OTLP_ENDPOINT/v1/logs`, header `x-tk-token: $TK_CHAT_TOKEN`), one `resourceLogs` entry per batch, batched at 1 second.

Resource attributes: `service.name` = `pi`, `service.version` = Pi's version, `host.arch`, `os.type`.

Per record: `timeUnixNano`, body `{"stringValue": "pi.<name>"}`, and these attributes, mirrored from Claude Code's:

| `event.name` (short) | From Pi event | Attributes |
|---|---|---|
| `user_prompt` | `before_agent_start` | `session.id`, `prompt.id`, `event.sequence`, `event.timestamp`, `prompt_length`; `prompt` only with `TK_LOG_CONTENT=1` |
| `tool_decision` | `tool_call` (after the gate decided) | the above, `tool_name` (as Pi gave it), `tool_name_normalized`, `tool_use_id`, `decision` (`accept`, `reject`, `ask_accept`, `ask_reject`, `ask_timeout`), `source` (`tk-policy`), `reason` |
| `tool_result` | `tool_execution_end` | the above, `tool_name`, `tool_use_id`, `success`, `duration_ms`, `tool_result_size_bytes`, `tool_result_sha256`; `tool_input` only with `TK_LOG_CONTENT=1` |
| `api_request` | `message_end` | the above, `model`, `input_tokens`, `output_tokens`, `duration_ms` when Pi gives them |
| `assistant_response` | `message_end` | the above, `model`, `response_length`; `response` only with `TK_LOG_CONTENT=1` |

Which Pi event carries which field is lane A's to confirm in Pi's `docs/extensions.md` and `types.ts` at 1.4.0 (note AI section 1.1 lists the event names; the field names are not read). The `event.name` values above are fixed: they are what `describe` already understands.

Rules:

- The emitter never blocks a turn. A failed POST is counted and dropped; the count is shown in Pi's status line if lane A adds one.
- No token, no POST. No endpoint, no POST. Nothing is sent anywhere but `TK_OTLP_ENDPOINT`, which must be a loopback URL (`127.0.0.1`, `::1`, `localhost`); any other host is refused and counted.
- Content off by default; hashes and sizes on.

## 3. What lane B changes in the receiver

- Strip `pi.` in `short_name` beside `claude_code.` and `codex.`.
- Rename the header to `x-tk-token`.
- Accept `pi` as a harness for a chat token (`harness_env.rs:21-35` qualifies Claude Code and Codex only).

## 4. The adapter record

Clients that are not OTel exporters (lane F's hook scripts, `tk run`) post `event` lines on the socket (`daemon-protocol.md`, section 2) with `source: "adapter"` (or `run`), their own `id`, and `detail.trust: "client"`. The id for a tool call is `<harness>-<tool_use_id>` when the harness gives a tool-use id, so the OTel record and the adapter record for one call can be joined (case 35: the last copy per id wins; one call, two records, joined by `tool_use_id` in `detail`).
