// OpenCode plugin (lane F, T2.3): the policy decision for one tool call,
// without the daemon. The plugin itself is src/plugin.ts. Input that is not a
// tool call is denied, so nothing installed by mistake allows a call.
import { decideCall } from "./core/gate.ts";
import { loadPolicy, type Action, type LoadedPolicy } from "./core/policy.ts";

export type Decision = { action: Action; reason: string };

export function decide(input: unknown, loaded?: LoadedPolicy): Decision {
  if (typeof input !== "object" || input === null) return { action: "deny", reason: "not a tool call" };
  const o = input as Record<string, unknown>;
  if (typeof o.tool !== "string" || o.tool === "") return { action: "deny", reason: "not a tool call" };
  const a = decideCall(loaded ?? loadPolicy(), { harness: "opencode", tool: o.tool, toolInput: o.args ?? {} });
  return { action: a.action, reason: a.reason };
}
