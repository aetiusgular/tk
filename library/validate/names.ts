// Tool and server names (lane E, T3.1; note AP section 8.7; contracts/policy.md section 2).
//
// Every harness spells an MCP tool its own way: Pi registers
// `mcp__<server>__<tool>` and replaces every character outside letters,
// digits and `_` with `_` (Pi docs/mcp.md at v1.4.0, "Configuration rules");
// Gemini's policy parser splits at the first `_` after `mcp_`, so a server
// name may not contain `_` (note AP section 4). The library therefore keeps
// one canonical spelling per tool and refuses two tools that collide once a
// harness has mangled them.

export const TOOL_NAME_CHARSET = /^[A-Za-z0-9_]+$/;

/** The server's short id the harnesses see: letters, digits and `-` only. */
export const SERVER_ID = /^[a-z0-9][a-z0-9-]{0,63}$/;

/** What a tool name becomes after a harness mangles it: lowercase, every
 *  character outside [a-z0-9_] replaced by `_`. Two tools whose mangled names
 *  are equal are one tool to a gate and a model. */
export function mangle(toolName: string): string {
  return toolName.trim().toLowerCase().replace(/[^a-z0-9_]/g, "_");
}

/** The `server/tool` key the policy file matches on (contracts/policy.md
 *  section 2): step 1 on both parts, so `hugging-face` becomes
 *  `hugging_face`, as the matchers normalize it and the policy schema's
 *  `tool` pattern requires (lane H's finding; integration w2). */
export function policyKey(serverId: string, toolName: string): string {
  return `${mangle(serverId)}/${mangle(toolName)}`;
}
