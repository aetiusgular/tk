# tk-opencode-plugin

The tk policy gate for OpenCode (T2.3), written against OpenCode 1.19.4's source. Not run against a real OpenCode yet (box test).

- **tool.execute.before** checks every built-in and MCP tool call against `~/.tk/policy.json` (or `$TK_POLICY`). A deny throws, which stops the call before the tool runs; OpenCode shows `tk: <reason>` as the tool's error.
- OpenCode has no ask a plugin can raise, so a policy `ask` goes to the tk daemon's approver (`tk watch --approve`). Only an approver's yes lets the call through.
- Secret-like values in a tool's input are redacted in the very object the tool runs with. In a shell command or a write they make the call an ask.
- **tool.execute.after** redacts secret-like values in the output (a built-in tool's `output` and `metadata`, an MCP result's text content) in place, hashes the result and records the call in the tk daemon (no content).
- MCP tools reach the plugin as `<server>_<tool>`. List your server names in the policy's `servers` so the matcher can split them. A server name with `-` in it does not split today (a contract gap, recorded for integration).

The plugin loads nothing when OpenCode starts it: the policy file is read on the first tool call. It makes no network call and has no dependencies; it runs inside OpenCode's Bun.

## Install

Add it to `~/.config/opencode/opencode.json` (`opencode.fragment.json` has the block, with OpenCode's own `permission` prompts kept as a second layer). tk never writes a project-level `opencode.json`, which an agent can also write.

## Environment

As the Claude Code plugin: `TK_POLICY`, `TK_DAEMON_SOCKET`, `TK_CHAT_TOKEN`, `TK_CHAT_ID` (else OpenCode's `sessionID`).
