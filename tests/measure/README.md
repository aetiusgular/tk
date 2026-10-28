# tests/measure: T3.5, the 27B with deferred tools

The measurement in `plans/toolkit_layer_plan.md` T3.5: 20 tasks on Qwen3.8 27B through Pi with the library's MCP servers `deferred` against the same servers `direct`, k of 3, and the same tasks through Claude Code with `ENABLE_TOOL_SEARCH` unset, `auto` and `false`. Results and the host spec are in `plans/parallel/tk_measurement.md` (the project's status doc) and in `results/`.

Nothing here is part of tk. It runs on a machine with the model, not in CI.

## Parts

| File | What it does |
|---|---|
| `record-tools.mjs` | Records a live server's `tools/list` and instructions into `tools/<name>.json` (HTTP or stdio). Headers given on the command line are not written |
| `tools/*.json` | The recorded definitions: Context7, Exa, Hugging Face (anonymous), Shopify Dev MCP 1.17.0. Vercel and Granola refuse `initialize` without a sign-in and are not in the set |
| `tasks.json` | 20 tasks: 16 that need the MCP tools (6 of them two calls in a row, 1 across two servers) and 4 controls that need only built-in tools. Every entity is made up |
| `stub-server.mjs` | A stdio MCP server that replays one recorded server and answers from the running task. The token a task needs is only in the reply to the right tool with the right arguments. Logs every call |
| `proxy.mjs` | A loopback pass-through to the model server that records, per request, how many tools were declared, their bytes, the deferred ones, the betas, and the usage the server reported. It stores no prompt text. `--fold-system` moves Claude Code's `role: system` messages into the top-level `system` (see below) |
| `policy.json` | tk's gate policy for both harnesses: built-ins, `tool_search`/`ToolSearch` and the four servers allowed |
| `run.mjs` | Runs every task under each condition k times, one at a time (k outer, then task, then condition), and appends one scored line per run to `<out>/results.jsonl`; raw harness output to `<out>/raw/` |
| `summarize.mjs` | The tables |

## Running it

```sh
# the model server (llama-swap on 127.0.0.1:8080), then two proxies sharing one state folder
node proxy.mjs --listen 8091 --upstream http://127.0.0.1:8080 --state $OUT/state
node proxy.mjs --listen 8093 --upstream http://127.0.0.1:8080 --state $OUT/state --fold-system
export TKM_PI=$(command -v pi) TKM_CLAUDE=/path/to/claude TKM_TK=/path/to/tk
node run.mjs --out $OUT/pi --state $OUT/state --tag pi- --conditions pi-deferred,pi-direct --k 3 --tasks all
node run.mjs --out $OUT/cc --state $OUT/state --tag cc- --proxy http://127.0.0.1:8093 --conditions cc-unset,cc-auto,cc-false --k 3 --tasks all
node summarize.mjs $OUT/pi/results.jsonl $OUT/cc/results.jsonl
```

Pi runs with `PI_CODING_AGENT_DIR` in the output folder (its own `settings.json` with `tk-pi`, `models.json` pointing at the proxy, `mcp.json` with the four stubs at the condition's exposure); Claude Code with `CLAUDE_CONFIG_DIR` there, `ANTHROPIC_BASE_URL` at the proxy, `--mcp-config` with the stubs, tk's plugin through `--plugin-dir`, and `WebFetch` and `WebSearch` disallowed because Pi has neither. Your own `~/.pi/agent`, `~/.claude` and `~/.tk/policy.json` are not read.

## What a pass means

The final answer (or, for t20, the file) holds every `TKM-...` token the task lists. A token sits only in a stub's reply to the right call (or in the work folder, for the controls), so a pass means the model reached the right tool with usable arguments and carried its answer through.

## Claude Code against llama.cpp

Claude Code 2.1.311 sends its environment block as a `role: "system"` entry inside `messages`. llama.cpp's Anthropic endpoint hands it to the model's chat template, and Qwen3.8's template raises `System message must be at the beginning` (HTTP 500), so every request fails and Claude Code retries. `--fold-system` moves those entries to the end of the top-level `system`; nothing else in the request changes, and each folded request is marked `foldedSystem` in the proxy's log. Claude Code's own `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS=1` does not remove the entry (tried).

## Notes on the 28 Oct data (`results/`)

- `requests.jsonl` holds two HTTP 500 lines tagged `pi-r0002` and `pi-r0004` (11:52 PDT, 12 tools, Qwen3.8's "System message must be at the beginning"). They came from a Claude Code smoke run, stopped before the Pi run started, whose process kept retrying through the unfolding proxy; the tag is the task file that was current. No Pi request failed. The summary counts only HTTP 200 turn requests.
- `t10`'s library `ferrum-orm` turned out to exist on PyPI (0.1.17). In `pi-r0020` the model found it with `bash` and answered from its real docs; that is the one Pi failure.
- The proxy did not record the answers to Claude Code's `count_tokens` calls (llama.cpp returns `input_tokens` at the top level, not under `usage`); `auto` called it once per run and then sent every tool up front.
- Paths in the raw output name the box user's home folder. No key or token is in them (scanned for `sk-`, `ghp_`, `hf_`, bearer tokens).
