// The harness-neutral gate: one tool call in, one decision out. Each plugin
// maps a harness's hook input to `GateCall` and the answer back to that
// harness's output. Canonical copy: packages/tk-claude-plugin/src/core/.

import { randomUUID } from "node:crypto";
import { approve, type ApproveOutcome, type ApproveRequest } from "./daemon.ts";
import { evaluate, isShellTool, normalizeToolName, type Action, type LoadedPolicy } from "./policy.ts";
import { redactText, redactValue } from "./redact.ts";

/** An ask never waits longer than this inside a hook, whatever the policy says, so the harness's own hook timeout (120 s in our hooks.json) is never reached. */
export const HOOK_ASK_CAP_SECONDS = 90;

/** Tools that write files: a secret-like value in their input turns `allow` into `ask` instead of being rewritten. */
export const WRITE_TOOLS = ["write", "edit", "multiedit", "notebookedit", "apply_patch", "patch"] as const;

export type GateCall = {
  harness: string;
  /** The tool name as the harness gave it. */
  tool: string;
  toolInput: unknown;
  chat?: string;
  toolUseId?: string;
};

export type GateAnswer = {
  action: Action;
  normalizedTool: string;
  reason: string;
  /** Set when secret-like values were redacted from a tool that sends its input out (MCP, web). */
  updatedInput?: unknown;
  redacted: number;
  askTimeoutSeconds: number;
};

/** Every string, number and boolean value under `v`, in order (keys left out). */
function valuesOf(v: unknown, out: string[], depth: number): void {
  if (depth > 64) throw new Error("tool input nested too deep to check");
  if (typeof v === "string") out.push(v);
  else if (typeof v === "number" || typeof v === "boolean") out.push(String(v));
  else if (Array.isArray(v)) for (const x of v) valuesOf(x, out, depth + 1);
  else if (typeof v === "object" && v !== null) for (const x of Object.values(v)) valuesOf(x, out, depth + 1);
}

/**
 * The text the reject list and the shell check run over. For a shell tool:
 * the command, then on their own lines the values of every other field except
 * the free-text `description`, so a working folder (OpenCode's bash `workdir`)
 * is checked too (integration w1, security finding S7). Otherwise the
 * serialized input.
 */
export function inputTextFor(normalizedTool: string, toolInput: unknown): string {
  if (isShellTool(normalizedTool) && typeof toolInput === "object" && toolInput !== null && !Array.isArray(toolInput)) {
    const fields = toolInput as Record<string, unknown>;
    const command = fields.command;
    const head =
      typeof command === "string"
        ? command
        : Array.isArray(command) && command.every((c) => typeof c === "string")
          ? command.join(" ")
          : undefined;
    if (head !== undefined) {
      const rest: string[] = [];
      for (const [k, v] of Object.entries(fields)) if (k !== "command" && k !== "description") valuesOf(v, rest, 0);
      // A folder value is also read with a trailing `/`, so `/home/u/.claude`
      // meets the `.claude/` control marker.
      for (const v of [...rest]) if (v.includes("/") && !v.endsWith("/")) rest.push(`${v}/`);
      return rest.length ? `${head}\n${rest.join("\n")}` : head;
    }
  }
  if (typeof toolInput === "string") return toolInput;
  let serialized = "";
  try {
    serialized = JSON.stringify(toolInput) ?? "";
  } catch {
    serialized = "";
  }
  // Serializing doubles every backslash, which would hide `_` from the
  // escape decoder; so every string key and value also goes in as it is.
  const leaves: string[] = [];
  const walk = (v: unknown, depth: number): void => {
    if (depth > 64) return;
    if (typeof v === "string") leaves.push(v);
    else if (Array.isArray(v)) for (const x of v) walk(x, depth + 1);
    else if (typeof v === "object" && v !== null) {
      for (const [k, x] of Object.entries(v)) {
        leaves.push(k);
        walk(x, depth + 1);
      }
    }
  };
  walk(toolInput, 0);
  return leaves.length ? `${serialized}\n${leaves.join("\n")}` : serialized;
}

export function decideCall(loaded: LoadedPolicy, call: GateCall): GateAnswer {
  const normalizedTool = normalizeToolName(call.tool, loaded.policy.servers, call.harness);
  const decision = evaluate(loaded, {
    tool: call.tool,
    harness: call.harness,
    ...(call.chat !== undefined ? { chat: call.chat } : {}),
    inputText: inputTextFor(normalizedTool, call.toolInput),
  });
  const askTimeoutSeconds = Math.min(loaded.policy.askTimeoutSeconds, HOOK_ASK_CAP_SECONDS);
  const answer: GateAnswer = { ...decision, redacted: 0, askTimeoutSeconds };
  if (decision.action === "deny") return answer;

  const { value, count } = redactValue(call.toolInput);
  if (count === 0) return answer;
  answer.redacted = count;
  const writesOrRuns = isShellTool(normalizedTool) || (WRITE_TOOLS as readonly string[]).includes(normalizedTool);
  if (writesOrRuns) {
    // Rewriting a command or a file changes what it does. Ask instead.
    if (decision.action === "allow") {
      answer.action = "ask";
      answer.reason = "secret-like value in input";
    }
    return answer;
  }
  answer.updatedInput = value;
  answer.reason = `${decision.reason}; ${count} secret-like value${count === 1 ? "" : "s"} redacted`;
  return answer;
}

/** What the approver sees: the tool and a redacted preview of the input, at most 160 characters. */
export function approvalSummary(answer: GateAnswer, toolInput: unknown): string {
  const preview = redactText(inputTextFor(answer.normalizedTool, toolInput)).text.replace(/\s+/g, " ").trim();
  const text = `${answer.normalizedTool}: ${preview}`;
  return text.length > 160 ? `${text.slice(0, 159)}…` : text;
}

export type AskResolution =
  | { kind: "allow"; reason: string }
  | { kind: "deny"; reason: string }
  /** Let the harness show its own prompt (Claude Code only). */
  | { kind: "harness"; reason: string };

/**
 * Resolves a policy `ask` through the daemon's `approve`.
 * `harnessCanAsk` true (Claude Code): no approver connected, or no daemon,
 * falls back to the harness's own prompt. False (Codex, OpenCode): anything
 * but an approver's yes is a deny (contracts/policy.md section 5).
 */
export async function resolveAsk(
  answer: GateAnswer,
  call: GateCall,
  harnessCanAsk: boolean,
  approveFn: (r: ApproveRequest) => Promise<ApproveOutcome> = (r) => approve(r),
): Promise<AskResolution> {
  const request: ApproveRequest = {
    id: `ap-${call.harness}-${call.toolUseId ?? randomUUID()}`,
    chat: call.chat ?? "",
    harness: call.harness,
    tool: answer.normalizedTool,
    summary: approvalSummary(answer, call.toolInput),
    timeoutSeconds: answer.askTimeoutSeconds,
  };
  let outcome: ApproveOutcome;
  try {
    outcome = await approveFn(request);
  } catch (error) {
    outcome = { unreachable: `approve failed: ${(error as Error)?.message ?? String(error)}` };
  }
  if ("unreachable" in outcome) {
    return harnessCanAsk
      ? { kind: "harness", reason: `${answer.reason} (tk daemon: ${outcome.unreachable})` }
      : { kind: "deny", reason: `${answer.reason}; ask needs an approver and the tk daemon is not reachable (${outcome.unreachable})` };
  }
  if (outcome.answer === "yes") return { kind: "allow", reason: `${answer.reason}; approved in tk` };
  if (outcome.by === "none") {
    return harnessCanAsk
      ? { kind: "harness", reason: `${answer.reason} (no tk approver connected)` }
      : { kind: "deny", reason: `${answer.reason}; ask needs an approver and none is connected` };
  }
  if (outcome.by === "timeout") return { kind: "deny", reason: `${answer.reason}; ask timed out` };
  return { kind: "deny", reason: `${answer.reason}; declined in tk` };
}
