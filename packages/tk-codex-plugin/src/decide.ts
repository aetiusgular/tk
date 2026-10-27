// Codex plugin (lane F, T2.2): the policy decision for one PreToolUse
// input, without the daemon. The hook itself is src/hook.ts, run by
// bin/tk-hook.mjs from hooks/hooks.json. Input that is not a tool call is
// denied, so nothing installed by mistake allows a call.
import { decideCall } from "./core/gate.ts";
import { loadPolicy, type Action, type LoadedPolicy } from "./core/policy.ts";

export type Decision = { action: Action; reason: string };

export function decide(input: unknown, loaded?: LoadedPolicy): Decision {
  if (typeof input !== "object" || input === null) return { action: "deny", reason: "not a tool call" };
  const o = input as Record<string, unknown>;
  if (typeof o.tool_name !== "string" || o.tool_name === "") return { action: "deny", reason: "not a tool call" };
  const a = decideCall(loaded ?? loadPolicy(), { harness: "codex", tool: o.tool_name, toolInput: o.tool_input ?? {} });
  return { action: a.action, reason: a.reason };
}
