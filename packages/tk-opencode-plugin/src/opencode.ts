// OpenCode adapter (T2.3). Read in OpenCode's source at v1.19.4 (148842ef),
// see plans/parallel/tk_lane_F.md, Research:
// - tool.execute.before(input {tool, sessionID, callID}, output {args}) runs
//   before every built-in and MCP tool (session/tools.ts:105-110, 401-405).
//   A throw stops the call: the trigger awaits each hook inside the tool's
//   effect (plugin/index.ts:291-295), before item.execute runs.
// - output.args is the very object the tool then runs with; replacing
//   output.args changes nothing, so redaction edits it in place.
// - tool.execute.after(input {tool, sessionID, callID, args}, output): for a
//   built-in tool output is {title, output, metadata, attachments}; for an
//   MCP tool it is the MCP result {content: [...]}. It is edited in place.
// - MCP tools are named sanitize(server) + "_" + sanitize(tool)
//   (mcp/catalog.ts:117-119): the policy's `servers` list splits them.
// - OpenCode has no ask a hook can raise, so a policy ask goes to the tk
//   daemon's approver; anything but an approver's yes is a deny
//   (contracts/policy.md section 5).

import { createHash, randomUUID } from "node:crypto";
import { approve, postEvent } from "./core/daemon.ts";
import { decideCall, resolveAsk, type GateCall } from "./core/gate.ts";
import { loadPolicyFrom, type LoadedPolicy } from "./core/policy.ts";
import { redactText, redactValue } from "./core/redact.ts";

export const HARNESS = "opencode";

export type BeforeInput = { tool: string; sessionID: string; callID: string };
export type BeforeOutput = { args: unknown };
export type AfterInput = { tool: string; sessionID: string; callID: string; args?: unknown };

export type Deps = {
  env: NodeJS.ProcessEnv;
  loadPolicy: () => LoadedPolicy;
  resolveAsk: typeof resolveAsk;
  postEvent: typeof postEvent;
};

export function defaultDeps(env: NodeJS.ProcessEnv = process.env): Deps {
  return {
    env,
    loadPolicy: () => loadPolicyFrom(env),
    resolveAsk: (answer, call, canAsk) => resolveAsk(answer, call, canAsk, (r) => approve(r, env)),
    postEvent: (e) => postEvent(e, env),
  };
}

/** The error a blocked call throws; OpenCode shows its message as the tool's failure. */
export class TkBlocked extends Error {
  constructor(reason: string) {
    super(`tk: ${reason}`);
    this.name = "TkBlocked";
  }
}

function chatOf(sessionID: unknown, env: NodeJS.ProcessEnv): string | undefined {
  const fromEnv = env.TK_CHAT_ID?.trim();
  if (fromEnv) return fromEnv;
  return typeof sessionID === "string" && sessionID !== "" ? sessionID : undefined;
}

/** Replaces the contents of `target` with `source`, keeping the same object. */
function replaceInPlace(target: unknown, source: unknown): boolean {
  if (typeof target !== "object" || target === null || typeof source !== "object" || source === null) return false;
  if (Array.isArray(target) && Array.isArray(source)) {
    target.splice(0, target.length, ...source);
    return true;
  }
  if (Array.isArray(target) || Array.isArray(source)) return false;
  const t = target as Record<string, unknown>;
  for (const k of Object.keys(t)) delete t[k];
  Object.assign(t, source);
  return true;
}

export async function before(input: BeforeInput, output: BeforeOutput, deps: Deps): Promise<void> {
  try {
    if (typeof input?.tool !== "string" || input.tool === "") throw new TkBlocked("tool call has no tool name");
    const chat = chatOf(input.sessionID, deps.env);
    const call: GateCall = {
      harness: HARNESS,
      tool: input.tool,
      toolInput: output?.args ?? {},
      ...(chat !== undefined ? { chat } : {}),
      ...(typeof input.callID === "string" ? { toolUseId: input.callID } : {}),
    };
    const answer = decideCall(deps.loadPolicy(), call);
    let action = answer.action;
    let reason = answer.reason;
    if (action === "ask") {
      const r = await deps.resolveAsk(answer, call, false);
      action = r.kind === "allow" ? "allow" : "deny";
      reason = r.reason;
    }
    if (action === "allow" && answer.updatedInput !== undefined && !replaceInPlace(output.args, answer.updatedInput)) {
      action = "deny";
      reason = "secret-like value in input that tk cannot rewrite";
    }
    await deps.postEvent({
      // A decision is written once per chat and id (the daemon, S9), so
      // one with no call id gets its own id instead of a shared "unknown".
      id: `${HARNESS}-${call.toolUseId ?? `unknown-${randomUUID()}`}-decision`,
      chatId: chat ?? "",
      harness: HARNESS,
      kind: "approval",
      summary: `${answer.normalizedTool} ${action}`,
      detail: { tool: answer.normalizedTool, decision: action, policyAction: answer.action, reason, redacted: answer.redacted, ...(call.toolUseId ? { toolUseId: call.toolUseId } : {}) },
    });
    if (action === "deny") throw new TkBlocked(reason);
  } catch (error) {
    if (error instanceof TkBlocked) throw error;
    throw new TkBlocked(`hook error: ${(error as Error)?.message ?? String(error)}`);
  }
}

export async function after(input: AfterInput, output: unknown, deps: Deps): Promise<void> {
  try {
    let serialized = "";
    try {
      serialized = JSON.stringify(output) ?? "";
    } catch {
      serialized = "";
    }
    const sha = createHash("sha256").update(serialized, "utf8").digest("hex");
    let count = 0;
    if (typeof output === "object" && output !== null) {
      const o = output as Record<string, unknown>;
      if (typeof o.output === "string") {
        const r = redactText(o.output);
        count += r.count;
        o.output = r.text;
      }
      if (Array.isArray(o.content)) {
        const r = redactValue(o.content);
        if (r.count && replaceInPlace(o.content, r.value)) count += r.count;
      }
      if (typeof o.metadata === "object" && o.metadata !== null) {
        const r = redactValue(o.metadata);
        if (r.count && replaceInPlace(o.metadata, r.value)) count += r.count;
      }
    }
    const chat = chatOf(input?.sessionID, deps.env);
    const tool = decideCall(deps.loadPolicy(), { harness: HARNESS, tool: String(input?.tool ?? ""), toolInput: {} }).normalizedTool;
    await deps.postEvent({
      id: `${HARNESS}-${typeof input?.callID === "string" ? input.callID : "unknown"}`,
      chatId: chat ?? "",
      harness: HARNESS,
      kind: "tool.call",
      summary: `${tool} done${count ? `, ${count} redacted` : ""}`,
      detail: { tool, resultSha256: sha, resultBytes: Buffer.byteLength(serialized, "utf8"), redacted: count, ...(typeof input?.callID === "string" ? { toolUseId: input.callID } : {}) },
    });
  } catch {
    // After the call nothing is gained by failing: the tool already ran.
  }
}
