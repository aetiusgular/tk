# What each harness's own `mcp add` wrote

Recorded by lane C on 27 Oct 2026 in the cloud workspace, each CLI installed
with `npm install --prefix /tmp/cli/<dir> --ignore-scripts <package>@<version>`
and run with `env -i HOME=<scratch> PATH=<node>:/usr/bin:/bin`, so no real
config was read or written. `connect/tests.rs` compares tk's output with
these files byte for byte. The Claude files' `machineID` was zeroed.

| File | Made by |
|---|---|
| `claude-1-before.json` | Claude Code 2.1.311 (`@anthropic-ai/claude-code-linux-x64` binary): `claude mcp add-json -s user exa '{"type":"http","url":"https://mcp.exa.ai/mcp"}'` in an empty HOME (Claude adds its own start-up keys) |
| `claude-1-after.json` | then `claude mcp add-json -s user fs '{"type":"stdio","command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/tmp"],"env":{"TOKEN":"${TOKEN}"}}'` |
| `claude-2-after.json` | then `claude mcp add-json -s user plain '{"command":"uvx","args":["mcp-server-time"]}'` |
| `claude-3-after.json` | then `claude mcp add-json -s user hdr '{"type":"http","url":"https://mcp.example.com/mcp","headers":{"Authorization":"Bearer ${EX_TOKEN}"}}'` (a repeated `exa` in between exited 1 and wrote nothing) |
| `codex-1-before.toml` | written by hand: a comment, `model = "o3" # inline`, `[profiles.local]` |
| `codex-1-after.toml` | codex-cli 0.163.0: `codex mcp add exa --url https://mcp.exa.ai/mcp --bearer-token-env-var EXA_KEY` |
| `codex-2-after.toml` | then `codex mcp add time -- uvx mcp-server-time` |
| `pi-1-after.json` | Pi 1.4.0 (`@earendil-works/pi-coding-agent`): `pi mcp add fs --env 'TOKEN=${TOKEN}' --exposure deferred -- npx -y @modelcontextprotocol/server-filesystem /tmp` in an empty HOME |
| `pi-2-after.json` | then `pi mcp add exa --url https://mcp.exa.ai/mcp --bearer-token-env-var EXA_KEY --exposure deferred` |
| `pi-3-after.json` | then `pi mcp add hdr --url https://x.example/mcp --header 'X-Key=${XK}'` |
| `pi-4-after.json` | then `pi mcp add plain -- uvx mcp-server-time` |
| `gemini-2-after.json` | Gemini CLI 0.65.0 (`@google/gemini-cli`): `gemini mcp add -s user fs npx -y @modelcontextprotocol/server-filesystem /tmp -e 'TOKEN=$TOKEN'`, then `gemini mcp add -s user -t http exa https://mcp.exa.ai/mcp -H 'Authorization: Bearer $EXA_KEY'`, empty HOME |
| `qwen-2-after.json` | Qwen Code 0.26.1 (`@qwen-code/qwen-code`): the same two commands as Gemini |
| `copilot-2-after.json` | GitHub Copilot CLI 1.0.104 (`@github/copilot`): `copilot mcp add fs --env 'TOKEN=$TOKEN' -- npx -y @modelcontextprotocol/server-filesystem /tmp`, then `copilot mcp add --transport http --header 'X-Key: $XK' exa https://mcp.exa.ai/mcp`, empty HOME. Re-recorded with whole-value `$VAR` after the research found that Copilot reads a reference only when the value starts with `$` |

Also seen, not kept as files: Gemini and Qwen keep `//` comments when they
write (comment-json); Pi refuses a file with comments and keeps the file's
indentation; Codex 0.163.0 keeps comments and silently replaces an existing
name; `claude mcp add-json` refuses an existing name with exit 1.

## Environment-reference probes (27 Oct)

A stdio "server" (`node probe.js <out> <arg>`) that writes `process.env.TOKEN`
and its arguments to a file and exits, put in each harness's user config
with `env: {"TOKEN": <ref>}` and the same `<ref>` as an argument, started by
the harness's own list or get command with `TOKEN=secret123` in the
environment:

| Harness, command | `${TOKEN}` in env | in args | `$TOKEN` |
|---|---|---|---|
| Claude Code 2.1.311, `claude mcp get p` | expanded | expanded | not tried |
| Pi 1.4.0, `pi mcp list` | expanded | literal | not tried |
| Gemini CLI 0.65.0, `gemini mcp list` (folder trust off) | expanded | expanded | expanded in both |
| Qwen Code 0.26.1, `qwen mcp list` | expanded | expanded | expanded in both |
| Copilot CLI 1.0.104, `copilot mcp list` | not started by `list` | | |

Codex 0.163.0: `codex mcp get` read back `env_vars`, `env_http_headers` and
`http_headers` from a hand-written entry, and accepted an unknown key
without a word.
