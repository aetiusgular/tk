// OpenClaw adapter (T2.4). Read in the npm package openclaw 2026.10.8
// (dc2523e: docs/ and dist/ as shipped; plans/parallel/tk_lane_I.md, Research):
// - `api.on("before_tool_call", handler, {priority, timeoutMs})`; higher
//   priority runs first, default 0 (docs/plugins/hooks/reference.md:16-45;
//   dist/hooks-kvgtfJyA.mjs getHooksForName sorts by priority, higher first).
// - The handler gets {toolName, params, toolKind?, toolCallId?, runId?} and
//   ctx {sessionKey, sessionId, ...} (docs/plugins/hooks/tool-policy.md:16-40).
//   toolName is normalized first: `bash` arrives as `exec`.
// - It returns {block: true, blockReason}, which is final; {params}, where the
//   last returned params win and every handler sees the original event, not an
//   earlier rewrite; or {requireApproval: {...}}, where the first one wins and
//   freezes the params (tool-policy.md:44-85).
// - A throw or a timeout blocks: the global runner sets before_tool_call to
//   fail-closed (dist/hook-runner-global-BED4qrKY.mjs:96-107).
//
// So tk registers at the lowest priority and runs after every other plugin.
// It decides on the original call and, when it allows, returns those params:
// the last params win, so the call that runs is the call tk checked (rules
// case 88; a higher-priority plugin's rewrite is replaced by the original, and
// Codex's native relay reads returning the same params as no rewrite,
// dist/native-hook-relay-6I61vqHJ.mjs:178-181). Not covered: a higher-priority
// plugin that already asked for approval froze its own params snapshot.

import { createHash, randomUUID } from "node:crypto";
import { approve, decisionId, postEvent } from "./core/daemon.ts";
import { decideCall, inputTextFor, resolveAsk, type GateCall } from "./core/gate.ts";
import { loadPolicyFrom, type LoadedPolicy } from "./core/policy.ts";
import { redactValue } from "./core/redact.ts";
import { narrowForControl } from "./control.ts";

export const HARNESS = "openclaw";
/** Lower than any plugin's ordinary priority, so tk runs last. */
export const TK_PRIORITY = -1_000_000;
/** OpenClaw's default per-handler budget is 15 s (hooks-kvgtfJyA.mjs:351-360); tk's ask may wait longer. */
export const TK_TIMEOUT_MS = 120_000;

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

export type BeforeEvent = { toolName?: unknown; params?: unknown; toolKind?: unknown; toolCallId?: unknown; runId?: unknown };
export type BeforeCtx = { sessionKey?: unknown; sessionId?: unknown } | undefined;
export type BeforeResult =
  | { block: true; blockReason: string }
  | { params: Record<string, unknown> }
  | { requireApproval: { title: string; description: string; severity: "warning"; timeoutMs: number; allowedDecisions: ("allow-once" | "deny")[] } };

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function chatOf(ctx: BeforeCtx, env: NodeJS.ProcessEnv): string | undefined {
  const fromEnv = env.TK_CHAT_ID?.trim();
  if (fromEnv) return fromEnv;
  const id = ctx?.sessionId ?? ctx?.sessionKey;
  return typeof id === "string" && id !== "" ? id : undefined;
}

/**
 * The name the gate matches. OpenClaw's shell tool is `exec` (`bash` is
 * normalized to it), which the core's shell list does not name, so a shell
 * `exec` is gated as `bash` and gets the shell-text check (rules case 82).
 * Code Mode's outer `exec` (toolKind code_mode_exec) is a code tool and keeps
 * its own name.
 */
export function toolForGate(event: BeforeEvent): string {
  const name = String(event.toolName ?? "");
  if (name === "exec" && event.toolKind !== "code_mode_exec") return "bash";
  return name;
}

function block(reason: string): BeforeResult {
  return { block: true, blockReason: `tk: ${reason.replace(/\s+/g, " ").trim() || "blocked"}` };
}

export async function beforeToolCall(event: BeforeEvent, ctx: BeforeCtx, deps: Deps): Promise<BeforeResult> {
  try {
    if (typeof event?.toolName !== "string" || event.toolName === "") return block("tool call has no tool name");
    const params = isPlainObject(event.params) ? event.params : {};
    const chat = chatOf(ctx, deps.env);
    const id = typeof event.toolCallId === "string" && event.toolCallId !== "" ? event.toolCallId : undefined;
    const call: GateCall = { harness: HARNESS, tool: toolForGate(event), toolInput: params, ...(chat !== undefined ? { chat } : {}), ...(id !== undefined ? { toolUseId: id } : {}) };
    const answer = decideCall(deps.loadPolicy(), call);
    const narrowed = narrowForControl(answer.action, inputTextFor(answer.normalizedTool, params));
    if (narrowed.control) {
      answer.action = narrowed.action;
      answer.reason = `input names a control file (${narrowed.control})`;
    }
    let action: "allow" | "deny" | "approval" = answer.action === "ask" ? "approval" : answer.action;
    let reason = answer.reason;
    if (answer.action === "ask") {
      const r = await deps.resolveAsk(answer, call, true);
      action = r.kind === "allow" ? "allow" : r.kind === "deny" ? "deny" : "approval";
      reason = r.reason;
    }
    if (action !== "deny" && answer.updatedInput !== undefined && !isPlainObject(answer.updatedInput)) {
      action = "deny";
      reason = "secret-like value in input that tk cannot rewrite on OpenClaw";
    }
    await deps.postEvent({
      // A decision is written once per chat and id (the daemon, S9), so
      // one with no call id gets its own id instead of a shared "unknown".
      id: decisionId(HARNESS, id),
      chatId: chat ?? "",
      harness: HARNESS,
      kind: "approval",
      summary: `${answer.normalizedTool} ${action === "approval" ? "ask" : action}`,
      detail: { tool: answer.normalizedTool, decision: action === "approval" ? "ask" : action, policyAction: answer.action, reason, redacted: answer.redacted, ...(id ? { toolUseId: id } : {}) },
    });
    if (action === "deny") return block(reason);
    if (action === "approval") {
      return {
        requireApproval: {
          title: `tk: allow ${answer.normalizedTool}?`,
          description: `tk asks: ${reason}`,
          severity: "warning",
          timeoutMs: answer.askTimeoutSeconds * 1000,
          allowedDecisions: ["allow-once", "deny"],
        },
      };
    }
    // Allow: hand back the params tk checked (redacted where it redacted), so
    // they are the ones that run.
    return { params: (isPlainObject(answer.updatedInput) ? answer.updatedInput : params) as Record<string, unknown> };
  } catch (error) {
    return block(`hook error: ${(error as Error)?.message ?? String(error)}`);
  }
}

export type AfterEvent = { toolName?: unknown; result?: unknown; error?: unknown; toolCallId?: unknown };

/** after_tool_call only observes (tool-policy.md:266-309): tk records the call, with no content. */
export async function afterToolCall(event: AfterEvent, ctx: BeforeCtx, deps: Deps): Promise<void> {
  try {
    let serialized = "";
    try {
      serialized = JSON.stringify(event?.result ?? event?.error ?? null) ?? "";
    } catch {
      serialized = "";
    }
    const sha = createHash("sha256").update(serialized, "utf8").digest("hex");
    const { count } = redactValue(event?.result);
    const chat = chatOf(ctx, deps.env);
    const id = typeof event?.toolCallId === "string" ? event.toolCallId : undefined;
    const tool = decideCall(deps.loadPolicy(), { harness: HARNESS, tool: toolForGate(event as BeforeEvent), toolInput: {} }).normalizedTool;
    await deps.postEvent({
      id: `${HARNESS}-${id ?? "unknown"}`,
      chatId: chat ?? "",
      harness: HARNESS,
      kind: "tool.call",
      summary: `${tool} done${count ? `, ${count} secret-like` : ""}`,
      detail: { tool, resultSha256: sha, resultBytes: Buffer.byteLength(serialized, "utf8"), secretLike: count, ...(id ? { toolUseId: id } : {}) },
    });
  } catch {
    // The tool already ran.
  }
}
