// The Node side of the Hermes plugin (T2.4). Hermes runs Python plugins; the
// plugin (`tk_gate.py`) hands each call to this script so the decision comes
// from tk's one gate core (`src/core/`, a byte copy of
// packages/tk-claude-plugin/src/core/), not a fourth matcher in Python.
//
// stdin, one JSON object:
//   {"event": "pre",  "tool_name", "args", "session_id", "tool_call_id"}
//   {"event": "post", "tool_name", "args", "result", "session_id", "tool_call_id", "status"}
// stdout, one JSON object:
//   pre:  {"decision": "allow" | "deny" | "ask", "reason", "args"?}  ("ask": no tk
//         approver answered; Hermes's own gate may ask, tk_gate.py decides)
//   post: {"result"?: <redacted text>, "redacted": n}
//
// Hermes facts it relies on (NousResearch/hermes-agent v0.22.5, bf0203a7;
// plans/parallel/tk_lane_I.md, Research):
// - the shell tool is `terminal` (field `command`), in the core's shell list;
//   MCP tools are `mcp__<server>__<tool>` (tools/mcp_tool_schema.py:150-185);
// - a pre_tool_call callback runs under plugins.hook_callback_timeout, 30 s by
//   default, and a timeout blocks (hermes_cli/plugins_dispatch.py:162-166,
//   279-286), so an ask here waits at most ASK_CAP_SECONDS;
// - a `modify` directive is merged shallowly over the args (plugins.py:1967-1976).

import { createHash } from "node:crypto";
import { approve, postEvent } from "./core/daemon.ts";
import { decideCall, inputTextFor, resolveAsk, type GateCall } from "./core/gate.ts";
import { loadPolicyFrom, type LoadedPolicy } from "./core/policy.ts";
import { redactText } from "./core/redact.ts";
import { narrowForControl } from "./control.ts";

export const HARNESS = "hermes";
/** Under Hermes's default 30 s callback timeout, with room for Node's start and the reply. */
export const ASK_CAP_SECONDS = 20;
const MAX_STDIN_BYTES = 32 * 1024 * 1024;
const MAX_RESULT_CHARS = 200_000;

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

export type PreAnswer = { decision: "allow" | "deny" | "ask"; reason: string; args?: unknown };
export type PostAnswer = { result?: string; redacted: number };

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function chatOf(input: Record<string, unknown>, env: NodeJS.ProcessEnv): string | undefined {
  const fromEnv = env.TK_CHAT_ID?.trim();
  if (fromEnv) return fromEnv;
  return typeof input.session_id === "string" && input.session_id !== "" ? input.session_id : undefined;
}

function callId(input: Record<string, unknown>): string | undefined {
  return typeof input.tool_call_id === "string" && input.tool_call_id !== "" ? input.tool_call_id : undefined;
}

export async function pre(input: Record<string, unknown>, deps: Deps): Promise<PreAnswer> {
  if (typeof input.tool_name !== "string" || input.tool_name === "") return { decision: "deny", reason: "tool call has no tool name" };
  const chat = chatOf(input, deps.env);
  const id = callId(input);
  const call: GateCall = {
    harness: HARNESS,
    tool: input.tool_name,
    toolInput: input.args ?? {},
    ...(chat !== undefined ? { chat } : {}),
    ...(id !== undefined ? { toolUseId: id } : {}),
  };
  const answer = decideCall(deps.loadPolicy(), call);
  const narrowed = narrowForControl(answer.action, inputTextFor(answer.normalizedTool, call.toolInput));
  if (narrowed.control) {
    answer.action = narrowed.action;
    answer.reason = `input names a control file (${narrowed.control})`;
  }
  answer.askTimeoutSeconds = Math.min(answer.askTimeoutSeconds, ASK_CAP_SECONDS);
  let decision: PreAnswer["decision"] = answer.action;
  let reason = answer.reason;
  if (decision === "ask") {
    // Hermes has its own human gate, so with no tk approver the ask goes
    // there (tk_gate.py refuses that under yolo, where Hermes's gate approves
    // everything); an approver's no or a timeout is a deny.
    const r = await deps.resolveAsk(answer, call, true);
    decision = r.kind === "allow" ? "allow" : r.kind === "deny" ? "deny" : "ask";
    reason = r.reason;
  }
  if (decision === "allow" && answer.updatedInput !== undefined && !isPlainObject(answer.updatedInput)) {
    decision = "deny";
    reason = "secret-like value in input that tk cannot rewrite on Hermes";
  }
  await deps.postEvent({
    id: `${HARNESS}-${id ?? "unknown"}-decision`,
    chatId: chat ?? "",
    harness: HARNESS,
    kind: "approval",
    summary: `${answer.normalizedTool} ${decision}`,
    detail: { tool: answer.normalizedTool, decision, policyAction: answer.action, reason, redacted: answer.redacted, ...(id ? { toolUseId: id } : {}) },
  });
  const out: PreAnswer = { decision, reason };
  if (decision !== "deny" && answer.updatedInput !== undefined) out.args = answer.updatedInput;
  return out;
}

export async function post(input: Record<string, unknown>, deps: Deps): Promise<PostAnswer> {
  const raw = typeof input.result === "string" ? input.result : JSON.stringify(input.result ?? null) ?? "";
  const sha = createHash("sha256").update(raw, "utf8").digest("hex");
  const { text, count } = redactText(raw);
  const chat = chatOf(input, deps.env);
  const id = callId(input);
  const tool = decideCall(deps.loadPolicy(), { harness: HARNESS, tool: String(input.tool_name ?? ""), toolInput: {} }).normalizedTool;
  await deps.postEvent({
    id: `${HARNESS}-${id ?? "unknown"}`,
    chatId: chat ?? "",
    harness: HARNESS,
    kind: "tool.call",
    summary: `${tool} done${count ? `, ${count} redacted` : ""}`,
    detail: { tool, resultSha256: sha, resultBytes: Buffer.byteLength(raw, "utf8"), redacted: count, ...(id ? { toolUseId: id } : {}) },
  });
  if (count === 0) return { redacted: 0 };
  const result = text.length > MAX_RESULT_CHARS ? `${text.slice(0, MAX_RESULT_CHARS)}\n[tk: output cut at ${MAX_RESULT_CHARS} characters]` : text;
  return { result, redacted: count };
}

/** One request in, one answer out. A pre call that cannot be decided is a deny. */
export async function handle(stdinText: string, deps: Deps = defaultDeps()): Promise<PreAnswer | PostAnswer> {
  let input: unknown;
  try {
    if (Buffer.byteLength(stdinText, "utf8") > MAX_STDIN_BYTES) return { decision: "deny", reason: "hook input too large" };
    input = JSON.parse(stdinText);
  } catch {
    return { decision: "deny", reason: "hook input is not JSON" };
  }
  if (!isPlainObject(input)) return { decision: "deny", reason: "hook input is not an object" };
  try {
    if (input.event === "post") return await post(input, deps);
    if (input.event !== "pre") return { decision: "deny", reason: `unknown event ${String(input.event)}` };
    return await pre(input, deps);
  } catch (error) {
    if (input.event === "post") return { redacted: 0 };
    return { decision: "deny", reason: `hook error: ${(error as Error)?.message ?? String(error)}` };
  }
}

export async function main(): Promise<number> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of process.stdin) {
    const b = chunk as Buffer;
    size += b.length;
    if (size > MAX_STDIN_BYTES + 1) break;
    chunks.push(b);
  }
  process.stdout.write(`${JSON.stringify(await handle(Buffer.concat(chunks).toString("utf8")))}\n`);
  return 0;
}
