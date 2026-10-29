// Codex adapter (T2.2). Read in Codex's source at rust-v0.163.0 (83086188),
// codex-rs/hooks/src (see plans/parallel/tk_lane_F.md, Research):
// - PreToolUse blocks on exit 2 with a non-empty stderr reason
//   (events/pre_tool_use.rs:261-274) or on permissionDecision "deny".
// - "ask" is not supported: Codex marks the run failed and the call goes on.
//   So this hook never emits ask; a policy ask goes to the tk daemon's
//   approver, and anything but an approver's yes is a deny.
// - Every failed run (spawn error, timeout, exit other than 0 or 2, exit 2
//   with empty stderr, invalid JSON) leaves the call unblocked: Codex fails
//   open. This hook therefore never crashes on purpose: every path ends in
//   a decision, and bin/tk-hook.mjs turns a load failure into exit 2.
// - updatedInput needs permissionDecision "allow" and replaces an MCP tool's
//   arguments whole (core/src/tools/handlers/mcp.rs:463-467); for Bash and
//   apply_patch it must carry a string `command`. This hook rewrites MCP
//   input only.
// - PostToolUse cannot rewrite output (updatedMCPToolOutput is refused);
//   decision "block" replaces the tool result with the reason.

import { createHash, randomUUID } from "node:crypto";
import { approve, decisionId, postEvent } from "./core/daemon.ts";
import { decideCall, resolveAsk, type GateCall } from "./core/gate.ts";
import { loadPolicyFrom, type LoadedPolicy } from "./core/policy.ts";
import { redactValue } from "./core/redact.ts";

export const HARNESS = "codex";
const MAX_STDIN_BYTES = 32 * 1024 * 1024;
const MAX_REASON_CHARS = 20_000;

export type HookResult = { stdout: string; stderr: string; exitCode: number };

type Deps = {
  env: NodeJS.ProcessEnv;
  loadPolicy: () => LoadedPolicy;
  resolveAsk: typeof resolveAsk;
  postEvent: typeof postEvent;
};

function defaultDeps(env: NodeJS.ProcessEnv): Deps {
  return {
    env,
    loadPolicy: () => loadPolicyFrom(env),
    resolveAsk: (answer, call, canAsk) => resolveAsk(answer, call, canAsk, (r) => approve(r, env)),
    postEvent: (e) => postEvent(e, env),
  };
}

const NONE: HookResult = { stdout: "", stderr: "", exitCode: 0 };

/** Codex reads stderr as the block reason; it must not be empty (pre_tool_use.rs:261-274). */
export function blockPre(reason: string): HookResult {
  const text = reason.trim() === "" ? "blocked" : reason.replace(/\s+/g, " ").trim();
  return { stdout: "", stderr: `tk: ${text}\n`, exitCode: 2 };
}

function chatOf(input: Record<string, unknown>, env: NodeJS.ProcessEnv): string | undefined {
  const fromEnv = env.TK_CHAT_ID?.trim();
  if (fromEnv) return fromEnv;
  return typeof input.session_id === "string" && input.session_id !== "" ? input.session_id : undefined;
}

export async function handlePre(input: Record<string, unknown>, deps: Deps): Promise<HookResult> {
  if (typeof input.tool_name !== "string" || input.tool_name === "") return blockPre("hook input has no tool_name");
  const chat = chatOf(input, deps.env);
  const call: GateCall = {
    harness: HARNESS,
    tool: input.tool_name,
    toolInput: input.tool_input ?? {},
    ...(chat !== undefined ? { chat } : {}),
    ...(typeof input.tool_use_id === "string" ? { toolUseId: input.tool_use_id } : {}),
  };
  const answer = decideCall(deps.loadPolicy(), call);
  let action = answer.action;
  let reason = answer.reason;
  if (action === "ask") {
    const r = await deps.resolveAsk(answer, call, false);
    action = r.kind === "allow" ? "allow" : "deny";
    reason = r.reason;
  }
  const isMcp = input.tool_name.startsWith("mcp__");
  if (action === "allow" && answer.updatedInput !== undefined && !isMcp) {
    action = "deny";
    reason = "secret-like value in input of a tool tk cannot rewrite on Codex";
  }
  await deps.postEvent({
    // A decision is written once per chat and id (the daemon, S9), so
    // one with no call id gets its own id instead of a shared "unknown".
    id: decisionId(HARNESS, call.toolUseId),
    chatId: chat ?? "",
    harness: HARNESS,
    kind: "approval",
    summary: `${answer.normalizedTool} ${action}`,
    detail: {
      tool: answer.normalizedTool,
      decision: action,
      policyAction: answer.action,
      reason,
      redacted: answer.redacted,
      ...(call.toolUseId ? { toolUseId: call.toolUseId } : {}),
    },
  });
  if (action === "deny") return blockPre(reason);
  if (answer.updatedInput !== undefined) {
    return {
      stdout: `${JSON.stringify({ hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "allow", permissionDecisionReason: `tk: ${reason}`, updatedInput: answer.updatedInput } })}\n`,
      stderr: "",
      exitCode: 0,
    };
  }
  // Allow: say nothing, so Codex's own approval flow runs as configured.
  return NONE;
}

export async function handlePost(input: Record<string, unknown>, deps: Deps): Promise<HookResult> {
  const toolName = typeof input.tool_name === "string" ? input.tool_name : "";
  const toolUseId = typeof input.tool_use_id === "string" ? input.tool_use_id : undefined;
  const response = input.tool_response;
  let serialized = "";
  try {
    serialized = typeof response === "string" ? response : (JSON.stringify(response) ?? "");
  } catch {
    serialized = "";
  }
  const sha = createHash("sha256").update(serialized, "utf8").digest("hex");
  const { value, count } = redactValue(response);
  const chat = chatOf(input, deps.env);
  const tool = decideCall(deps.loadPolicy(), { harness: HARNESS, tool: toolName, toolInput: {} }).normalizedTool;
  await deps.postEvent({
    id: `${HARNESS}-${toolUseId ?? "unknown"}`,
    chatId: chat ?? "",
    harness: HARNESS,
    kind: "tool.call",
    summary: `${tool} done${count ? `, ${count} redacted` : ""}`,
    detail: { tool, resultSha256: sha, resultBytes: Buffer.byteLength(serialized, "utf8"), redacted: count, ...(toolUseId ? { toolUseId } : {}) },
  });
  if (count === 0) return NONE;
  let text = typeof value === "string" ? value : JSON.stringify(value);
  if (text.length > MAX_REASON_CHARS) text = `${text.slice(0, MAX_REASON_CHARS)}\n[tk: output cut at ${MAX_REASON_CHARS} characters]`;
  return {
    stdout: `${JSON.stringify({ decision: "block", reason: `tk replaced this output: ${count} secret-like value${count === 1 ? "" : "s"} redacted.\n${text}` })}\n`,
    stderr: "",
    exitCode: 0,
  };
}

export async function runHook(event: string, stdinText: string, env: NodeJS.ProcessEnv = process.env, deps?: Partial<Deps>): Promise<HookResult> {
  const d: Deps = { ...defaultDeps(env), ...deps };
  const pre = event === "pre";
  try {
    if (Buffer.byteLength(stdinText, "utf8") > MAX_STDIN_BYTES) return pre ? blockPre("hook input too large") : NONE;
    let input: unknown;
    try {
      input = JSON.parse(stdinText);
    } catch {
      return pre ? blockPre("hook input is not JSON") : NONE;
    }
    if (typeof input !== "object" || input === null || Array.isArray(input)) return pre ? blockPre("hook input is not an object") : NONE;
    const obj = input as Record<string, unknown>;
    if (pre) {
      if (obj.hook_event_name !== undefined && obj.hook_event_name !== "PreToolUse") return blockPre(`unexpected hook event ${String(obj.hook_event_name)}`);
      return await handlePre(obj, d);
    }
    if (event === "post") return await handlePost(obj, d);
    return { stdout: "", stderr: `tk: unknown hook event ${event}\n`, exitCode: 0 };
  } catch (error) {
    return pre ? blockPre(`hook error: ${(error as Error)?.message ?? String(error)}`) : NONE;
  }
}

export async function main(event: string): Promise<number> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of process.stdin) {
    const b = chunk as Buffer;
    size += b.length;
    if (size > MAX_STDIN_BYTES + 1) break;
    chunks.push(b);
  }
  const result = await runHook(event, Buffer.concat(chunks).toString("utf8"));
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  return result.exitCode;
}
