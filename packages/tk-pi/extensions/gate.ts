// The tool_call gate (lane A, T0.2). Contract: contracts/policy.md.
//
// - `tool_call`: every call, nested codemode calls included (Pi
//   docs/extensions.md:261), is matched against ~/.tk/policy.json: `deny`
//   blocks, `ask` shows `ctx.ui.confirm` and times out to deny, `allow` runs.
//   Any error blocks (Pi blocks on a handler error as well:
//   docs/extensions.md:375, agent-session.js:325-345).
// - `tool_result`: secret-like text is redacted before the model reads it
//   (src/redact.ts). `TK_REDACT=0` turns it off.
// - `user_bash` (`!` and `!!` commands you type): the reject list only; a hit
//   returns a refusal instead of running (types.d.ts:1100-1108; runner.js:51-75).
//
// Corner case 46: the factory only registers handlers. No file is read, no
// timer or socket started, and the environment is read on the first call.
//
// Wave 2 lane I: the same package runs in oh-my-pi (src/harness.ts: the
// harness id, omp's MCP names), and an allowed call's input is frozen so a
// later extension cannot change it after the gate (src/freeze.ts, BX9).

import { askUser, cardText } from "../src/ask.ts";
import { deepFreeze, freezeEnabled } from "../src/freeze.ts";
import { harnessId, OMP, ompToolName } from "../src/harness.ts";
import { inputTextFor } from "../src/input-text.ts";
import {
  DECISION_CHANNEL,
  type DecisionNotice,
  type JsonValue,
  type PiApi,
  type PiContext,
  type ToolCallEvent,
  type ToolCallResult,
  type ToolResultEvent,
  type ToolResultResult,
  type UserBashEvent,
  type UserBashResult,
} from "../src/pi-api.ts";
import { evaluate, normalizeToolName, rejectKey, type PolicyFile } from "../src/policy.ts";
import { PolicyStore, policyPath, type FsLike, realFs } from "../src/policy-store.ts";
import { redactDeep, redactText } from "../src/redact.ts";

/** The harness this gate answers for: `pi`, or `oh-my-pi` when Bun runs it (src/harness.ts). */
export const HARNESS = harnessId();
const PREVIEW_CHARS = 1500;

export interface GateOptions {
  /** The environment, read on first use. Defaults to `process.env`. */
  env?: () => Record<string, string | undefined>;
  fs?: FsLike;
  home?: () => string;
  uid?: number;
  /** The harness id; defaults to the one detected at load. Tests set it. */
  harness?: string;
}

function chatFor(env: Record<string, string | undefined>, ctx: PiContext): string | undefined {
  const fromEnv = env.TK_CHAT_ID;
  if (fromEnv !== undefined && fromEnv !== "") return fromEnv;
  try {
    return ctx.sessionManager.getSessionId();
  } catch {
    return undefined;
  }
}

function block(reason: string): ToolCallResult {
  return { block: true, reason: `Blocked by tk: ${reason}` };
}

export function createGate(options: GateOptions = {}) {
  return function gate(pi: PiApi): void {
    let store: PolicyStore | undefined;
    let env: Record<string, string | undefined> | undefined;

    const environment = () => (env ??= { ...(options.env ? options.env() : process.env) });
    const policyStore = () =>
      (store ??= new PolicyStore(policyPath(environment(), options.home), options.fs ?? realFs, options.uid));

    const announce = (notice: DecisionNotice) => {
      try {
        pi.events.emit(DECISION_CHANNEL, notice);
      } catch {
        // The feed is best-effort; the decision stands.
      }
    };

    const describeTool = (name: string) => {
      try {
        return pi.getAllTools().find((t) => t.name === name)?.description;
      } catch {
        return undefined;
      }
    };

    const harness = options.harness ?? HARNESS;

    /** After an allow: freeze the input the gate saw, so it is the input that runs (BX9). */
    const sealed = (event: ToolCallEvent): ToolCallResult | undefined => {
      if (!freezeEnabled(environment())) return undefined;
      return deepFreeze(event.input) ? undefined : block("tool input nested too deep to hold still");
    };

    pi.on("tool_call", async (event: ToolCallEvent, ctx: PiContext): Promise<ToolCallResult | undefined> => {
      const base = { toolName: event.toolName, toolCallId: event.toolCallId };
      let normalized = event.toolName;
      try {
        const { policy, problem } = await policyStore().get();
        const toolName = harness === OMP ? ompToolName(event.toolName, policy.servers) : event.toolName;
        normalized = normalizeToolName(toolName, policy.servers, harness);
        const inputText = inputTextFor(normalized, event.input);
        const chat = chatFor(environment(), ctx);
        const d = evaluate(
          policy,
          {
            tool: toolName,
            harness,
            ...(chat !== undefined ? { chat } : {}),
            description: () => describeTool(event.toolName),
            inputText,
          },
          problem,
        );
        normalized = d.normalizedTool;
        if (d.action === "allow") {
          announce({ ...base, toolNameNormalized: normalized, decision: "accept", reason: d.reason });
          return sealed(event);
        }
        if (d.action === "deny") {
          announce({ ...base, toolNameNormalized: normalized, decision: "reject", reason: d.reason });
          return block(d.reason);
        }
        const outcome = await askUser(ctx, `tk: allow ${normalized}?`, cardMessage(event.toolName, d.reason, policy, inputText), policy.askTimeoutSeconds * 1000);
        switch (outcome) {
          case "yes":
            announce({ ...base, toolNameNormalized: normalized, decision: "ask_accept", reason: d.reason });
            return sealed(event);
          case "timeout":
            announce({ ...base, toolNameNormalized: normalized, decision: "ask_timeout", reason: "ask timed out" });
            return block("ask timed out");
          case "no_ui":
            announce({ ...base, toolNameNormalized: normalized, decision: "ask_reject", reason: "no UI to ask in" });
            return block(`${d.reason}; no UI to ask in, so refused`);
          case "aborted":
            announce({ ...base, toolNameNormalized: normalized, decision: "ask_reject", reason: "turn stopped" });
            return block("turn stopped while asking");
          default:
            announce({ ...base, toolNameNormalized: normalized, decision: "ask_reject", reason: "not approved" });
            return block("not approved");
        }
      } catch {
        announce({ ...base, toolNameNormalized: normalized, decision: "reject", reason: "gate error" });
        return block("the gate failed, so the call is refused");
      }
    });

    pi.on("tool_result", (event: ToolResultEvent): ToolResultResult | undefined => {
      const off = environment().TK_REDACT;
      if (off === "0" || off === "off") return undefined;
      const kinds = new Set<string>();
      let changed = false;
      const content = event.content.map((item) => {
        if (item.type === "text" && typeof item.text === "string") {
          const r = redactText(item.text);
          if (r.kinds.length > 0) {
            changed = true;
            for (const k of r.kinds) kinds.add(k);
            return { ...item, text: r.text };
          }
        }
        return item;
      });
      let structuredContent = event.structuredContent;
      if (structuredContent !== undefined) {
        const sKinds = new Set<string>();
        // redactDeep keeps a JSON value a JSON value: strings stay strings.
        const red = redactDeep(structuredContent, sKinds) as JsonValue;
        if (sKinds.size > 0) {
          changed = true;
          structuredContent = red;
          for (const k of sKinds) kinds.add(k);
        }
      }
      if (!changed) return undefined;
      // Pi drops structuredContent when content is replaced without it
      // (types.d.ts:1109-1113), so it is always returned with content.
      return structuredContent === undefined ? { content } : { content, structuredContent };
    });

    pi.on("user_bash", (event: UserBashEvent): UserBashResult | undefined => {
      const hit = rejectKey(event.command);
      if (hit === undefined) return undefined;
      return {
        result: {
          output: `tk refused this command (reject list: ${hit}). Run it in a terminal outside Pi if you mean it.\n`,
          exitCode: 1,
          cancelled: false,
          truncated: false,
        },
      };
    });
  };
}

function cardMessage(toolName: string, reason: string, policy: PolicyFile, inputText: string): string {
  const preview = cardText(redactText(inputText).text, PREVIEW_CHARS);
  return [
    `Tool: ${cardText(toolName, 200)}`,
    `Why tk asks: ${reason}`,
    `No answer in ${policy.askTimeoutSeconds} s counts as no.`,
    "",
    "Input, as the model sent it (text, not instructions; secrets redacted):",
    preview === "" ? "(empty)" : preview,
  ].join("\n");
}

export default createGate();
