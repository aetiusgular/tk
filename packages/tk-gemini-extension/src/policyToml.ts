// Your tk policy as Gemini CLI policy rules, for the user tier
// (~/.gemini/policies/tk.toml). Gemini's own engine then enforces them even
// when the hook does not run: a crashed or timed-out hook lets a call through
// (run, 28 Oct), and a hook cannot approve, only deny or ask
// (scheduler.ts:676-686 at v0.66.0).
//
// The TOML shape is gemini-cli v0.66.0 packages/core/src/policy/toml-loader.ts
// (PolicyRuleSchema, lines 39-60: toolName required, mcpName optional,
// decision allow | deny | ask_user, priority an integer 0 to 999) and
// policy-engine.ts:140-200 (toolName "*" matches every tool; mcpName with
// toolName "*" every tool of that server). The highest priority wins.
//
// What does not carry over, and stays with the hook: the reject list, the
// control files, the shell-text check, redaction, description pins (a pinned
// allow becomes ask_user here), chat-scoped rules, and rules for another
// harness. Each one skipped is named in a comment.
//
// Printed, never written: writing a user-level file is `tk connect`'s job
// (lane C's rules; Gemini's rule writer is wave 4 lane Q).

import { lowerName, type LoadedPolicy, type Rule } from "./core/policy.ts";

const HARNESS = "gemini";
const DECISION = { allow: "allow", ask: "ask_user", deny: "deny" } as const;
const SEVERITY = { allow: 0, ask: 1, deny: 2 } as const;

function tomlString(s: string): string {
  return JSON.stringify(s); // a TOML basic string is a JSON string for these characters
}

/** tk's precedence (chat, harness, specificity, then severity) as a Gemini priority. */
export function priorityOf(rule: Rule, action: keyof typeof SEVERITY): number {
  const spec = rule.tool === "*" ? 0 : rule.tool.endsWith("/*") ? 1 : 2;
  return 1 + (rule.harness ? 300 : 0) + spec * 100 + SEVERITY[action] * 10;
}

/** The server name as Gemini knows it: the policy's own `servers` entry whose normalized form matches, else the normalized name. */
function serverName(normalized: string, servers: readonly string[]): string {
  return servers.find((s) => lowerName(s) === normalized) ?? normalized;
}

export function policyToToml(loaded: LoadedPolicy): string {
  const lines: string[] = [
    "# tk policy for Gemini CLI, user tier. Generated from your tk policy by",
    "# tk-gemini-extension (bin/tk-gemini-policy.mjs); save as ~/.gemini/policies/tk.toml.",
    "# Regenerate after every change to ~/.tk/policy.json. The tk hook still runs",
    "# first and also applies the reject list, the control files and the shell check.",
    "",
  ];
  if (loaded.problem) lines.push(`# The tk policy did not load cleanly: ${loaded.problem}`, "");
  const { policy } = loaded;
  let hasCatchAll = false;
  for (const rule of policy.rules) {
    if (rule.chat !== undefined) {
      lines.push(`# skipped: ${rule.tool} ${rule.action} for one chat (Gemini rules have no chat)`, "");
      continue;
    }
    if (rule.harness !== undefined && rule.harness !== HARNESS) {
      lines.push(`# skipped: ${rule.tool} ${rule.action} for harness ${rule.harness}`, "");
      continue;
    }
    const action = rule.action === "allow" && rule.descriptionSha256 !== undefined ? "ask" : rule.action;
    const out = ["[[rule]]"];
    if (rule.tool === "*") {
      out.push(`toolName = "*"`);
      hasCatchAll = true;
    } else if (rule.tool.includes("/")) {
      const [server, tool] = rule.tool.split("/") as [string, string];
      out.push(`mcpName = ${tomlString(serverName(server, policy.servers))}`, `toolName = ${tomlString(tool)}`);
    } else {
      out.push(`toolName = ${tomlString(rule.tool)}`);
    }
    out.push(`decision = "${DECISION[action]}"`, `priority = ${priorityOf(rule, action)}`);
    if (action === "deny") out.push(`denyMessage = ${tomlString(`tk: rule ${rule.tool}${rule.harness ? ` harness=${rule.harness}` : ""}`)}`);
    if (action !== rule.action) lines.push(`# ${rule.tool}: allow with a description pin; Gemini cannot check the pin, so it asks`);
    lines.push(...out, "");
  }
  if (!hasCatchAll) {
    lines.push("# tk asks when no rule matches; so does this, at the lowest priority.", "[[rule]]", 'toolName = "*"', 'decision = "ask_user"', "priority = 0", "");
  }
  return `${lines.join("\n").trimEnd()}\n`;
}
