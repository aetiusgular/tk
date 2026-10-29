// Gemini CLI adapter (T2.4). Read in gemini-cli at v0.66.0 (970413207732)
// and run against Gemini CLI 0.66.0 and 0.65.0 with the fake Gemini API on
// 28 Oct 2026 (plans/parallel/tk_lane_I.md, Research):
// - BeforeTool gets {session_id, transcript_path, cwd, hook_event_name,
//   timestamp, tool_name, tool_input} and, for an MCP tool, mcp_context
//   {server_name, tool_name, ...} (docs/hooks/reference.md:92-112;
//   core/coreToolHookTriggers.ts:27-55).
// - Block: {"decision":"deny","reason"} on exit 0, or exit 2 with the reason on
//   stderr. Both ran: the call is not executed and the reason goes to the
//   model as the tool's error. This hook does both (stdout JSON, stderr text,
//   exit 2), so a block survives either reading.
// - Ask: {"decision":"ask"} forces Gemini's own confirmation over its policy
//   (scheduler.ts:676-686, 721-735). Run headless (-p, --approval-mode yolo):
//   the call did not run and no result came back. Interactive: not run here.
// - Allow cannot approve: Gemini's policy check runs after the hook
//   (scheduler.ts:676). So an allow prints nothing and Gemini decides.
// - Rewrite: hookSpecificOutput.tool_input replaces the arguments (the
//   tool is rebuilt from it, hook-utils.ts:84-99; ran).
// - Fails open: a hook killed by a signal, an exit other than 0 or 2, and a
//   timeout all let the call run (run, every one; hookRunner.ts:434-487 reads
//   a null exit code as success). There is no switch. So this hook never
//   crashes on purpose, caps its own waits below its timeout, and
//   bin/tk-hook.mjs turns a load failure into exit 2.
// - AfterTool: {"decision":"deny","reason"} replaces the tool's result with
//   the reason (docs/hooks/reference.md:114-139).

import { createHash } from "node:crypto";
import { approve, decisionId, postEvent } from "./core/daemon.ts";
import { decideCall, inputTextFor, resolveAsk, type GateCall } from "./core/gate.ts";
import { loadPolicyFrom, type LoadedPolicy } from "./core/policy.ts";
import { redactValue } from "./core/redact.ts";
import { narrowForControl } from "./control.ts";
import { hookEnv } from "./env.ts";

export const HARNESS = "gemini";
const MAX_STDIN_BYTES = 32 * 1024 * 1024;
const MAX_REASON_CHARS = 20_000;

export type HookResult = { stdout: string; stderr: string; exitCode: number };

export type Deps = {
  env: NodeJS.ProcessEnv;
  loadPolicy: () => LoadedPolicy;
  resolveAsk: typeof resolveAsk;
  postEvent: typeof postEvent;
};

export function defaultDeps(rawEnv: NodeJS.ProcessEnv): Deps {
  const env = hookEnv(rawEnv);
  return {
    env,
    loadPolicy: () => loadPolicyFrom(env),
    resolveAsk: (answer, call, canAsk) => resolveAsk(answer, call, canAsk, (r) => approve(r, env)),
    postEvent: (e) => postEvent(e, env),
  };
}

const NONE: HookResult = { stdout: "", stderr: "", exitCode: 0 };

function oneLine(reason: string): string {
  const text = reason.replace(/\s+/g, " ").trim();
  return text === "" ? "blocked" : text;
}

/** A block Gemini reads either way: JSON deny on stdout, the reason on stderr, exit 2. */
export function block(reason: string): HookResult {
  const text = `tk: ${oneLine(reason)}`;
  return { stdout: `${JSON.stringify({ decision: "deny", reason: text })}\n`, stderr: `${text}\n`, exitCode: 2 };
}

function ask(reason: string): HookResult {
  return { stdout: `${JSON.stringify({ decision: "ask", reason: `tk: ${oneLine(reason)}`, systemMessage: `tk asks: ${oneLine(reason)}` })}\n`, stderr: "", exitCode: 0 };
}

function chatOf(input: Record<string, unknown>, env: NodeJS.ProcessEnv): string | undefined {
  const fromEnv = env.TK_CHAT_ID?.trim();
  if (fromEnv) return fromEnv;
  return typeof input.session_id === "string" && input.session_id !== "" ? input.session_id : undefined;
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Gemini gives a hook no call id (the input keys, run 28 Oct). BeforeTool and
 * AfterTool both carry the tool's name and its arguments as the model sent
 * them, so the two records share a key made from those; each record's own id
 * also takes the hook's timestamp, so two identical calls stay two records.
 */
export function callKey(input: Record<string, unknown>): string {
  let args = "";
  try {
    args = JSON.stringify(input.tool_input ?? {}) ?? "";
  } catch {
    args = "";
  }
  return createHash("sha256").update(`${String(input.session_id ?? "")}\n${String(input.tool_name ?? "")}\n${args}`, "utf8").digest("hex").slice(0, 16);
}

function recordId(input: Record<string, unknown>, suffix: string): string {
  const at = typeof input.timestamp === "string" ? input.timestamp.replace(/[^0-9A-Za-z]/g, "") : String(Date.now());
  return `${HARNESS}-${callKey(input)}-${at}${suffix}`;
}

/**
 * The name the gate matches. An MCP tool reaches the hook as
 * `mcp_<server>_<tool>`, which splits at the first `_` and so misreads a
 * server whose name has one; Gemini also sends mcp_context with the server and
 * tool apart, so the gate gets `mcp__<server>__<tool>` from those when both are
 * there. Anything else is matched by its own name.
 */
export function toolForGate(input: Record<string, unknown>): string {
  const name = String(input.tool_name);
  const ctx = input.mcp_context;
  if (isPlainObject(ctx) && typeof ctx.server_name === "string" && ctx.server_name !== "" && typeof ctx.tool_name === "string" && ctx.tool_name !== "") {
    return `mcp__${ctx.server_name}__${ctx.tool_name}`;
  }
  return name;
}

export async function handleBefore(input: Record<string, unknown>, deps: Deps): Promise<HookResult> {
  if (typeof input.tool_name !== "string" || input.tool_name === "") return block("hook input has no tool_name");
  const chat = chatOf(input, deps.env);
  const toolInput = input.tool_input ?? {};
  const call: GateCall = {
    harness: HARNESS,
    tool: toolForGate(input),
    toolInput,
    ...(chat !== undefined ? { chat } : {}),
  };
  const answer = decideCall(deps.loadPolicy(), call);
  const narrowed = narrowForControl(answer.action, inputTextFor(answer.normalizedTool, toolInput));
  if (narrowed.control) {
    answer.action = narrowed.action;
    answer.reason = `input names a control file (${narrowed.control})`;
  }
  let action = answer.action;
  let reason = answer.reason;
  let geminiAsks = false;
  if (action === "ask") {
    // Gemini can show its own prompt, so with no tk approver the ask goes to
    // Gemini (as Claude Code's does); an approver's no, or a timeout, denies.
    const r = await deps.resolveAsk(answer, call, true);
    if (r.kind === "allow") action = "allow";
    else if (r.kind === "deny") action = "deny";
    else geminiAsks = true;
    reason = r.reason;
  }
  await deps.postEvent({
    id: decisionId(HARNESS, callKey(input)),
    chatId: chat ?? "",
    harness: HARNESS,
    kind: "approval",
    summary: `${answer.normalizedTool} ${geminiAsks ? "ask" : action}`,
    detail: { tool: answer.normalizedTool, decision: geminiAsks ? "ask" : action, policyAction: answer.action, reason, redacted: answer.redacted, callKey: callKey(input) },
  });
  if (action === "deny") return block(reason);
  if (geminiAsks) return ask(reason);
  if (answer.updatedInput !== undefined) {
    if (!isPlainObject(answer.updatedInput)) return block("secret-like value in input that tk cannot rewrite on Gemini");
    return {
      stdout: `${JSON.stringify({ hookSpecificOutput: { hookEventName: "BeforeTool", tool_input: answer.updatedInput }, systemMessage: `tk: ${oneLine(reason)}` })}\n`,
      stderr: "",
      exitCode: 0,
    };
  }
  // Allow: print nothing, so Gemini's own policy decides (a hook cannot approve).
  return NONE;
}

export async function handleAfter(input: Record<string, unknown>, deps: Deps): Promise<HookResult> {
  const chat = chatOf(input, deps.env);
  const response = input.tool_response;
  let serialized = "";
  try {
    serialized = typeof response === "string" ? response : (JSON.stringify(response) ?? "");
  } catch {
    serialized = "";
  }
  const sha = createHash("sha256").update(serialized, "utf8").digest("hex");
  // What the model reads is llmContent; returnDisplay is what the terminal shows.
  const seen = isPlainObject(response) ? { llmContent: response.llmContent, returnDisplay: response.returnDisplay, error: response.error } : response;
  const { value, count } = redactValue(seen);
  const tool = decideCall(deps.loadPolicy(), { harness: HARNESS, tool: typeof input.tool_name === "string" ? toolForGate(input) : "", toolInput: {} }).normalizedTool;
  await deps.postEvent({
    id: recordId(input, ""),
    chatId: chat ?? "",
    harness: HARNESS,
    kind: "tool.call",
    summary: `${tool} done${count ? `, ${count} redacted` : ""}`,
    detail: { tool, resultSha256: sha, resultBytes: Buffer.byteLength(serialized, "utf8"), redacted: count, callKey: callKey(input) },
  });
  if (count === 0) return NONE;
  const redacted = isPlainObject(value) ? value.llmContent : value;
  let text = typeof redacted === "string" ? redacted : JSON.stringify(redacted);
  if (text.length > MAX_REASON_CHARS) text = `${text.slice(0, MAX_REASON_CHARS)}\n[tk: output cut at ${MAX_REASON_CHARS} characters]`;
  return {
    stdout: `${JSON.stringify({ decision: "deny", reason: `tk replaced this output: ${count} secret-like value${count === 1 ? "" : "s"} redacted.\n${text}` })}\n`,
    stderr: "",
    exitCode: 0,
  };
}

export async function runHook(event: string, stdinText: string, env: NodeJS.ProcessEnv = process.env, deps?: Partial<Deps>): Promise<HookResult> {
  const d: Deps = { ...defaultDeps(env), ...deps };
  const before = event === "before";
  try {
    if (Buffer.byteLength(stdinText, "utf8") > MAX_STDIN_BYTES) return before ? block("hook input too large") : NONE;
    let input: unknown;
    try {
      input = JSON.parse(stdinText);
    } catch {
      return before ? block("hook input is not JSON") : NONE;
    }
    if (!isPlainObject(input)) return before ? block("hook input is not an object") : NONE;
    if (before) {
      if (input.hook_event_name !== undefined && input.hook_event_name !== "BeforeTool") return block(`unexpected hook event ${String(input.hook_event_name)}`);
      return await handleBefore(input, d);
    }
    if (event === "after") return await handleAfter(input, d);
    return { stdout: "", stderr: `tk: unknown hook event ${event}\n`, exitCode: 0 };
  } catch (error) {
    return before ? block(`hook error: ${(error as Error)?.message ?? String(error)}`) : NONE;
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
