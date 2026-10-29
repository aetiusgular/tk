// The OTLP emitter (lane A, T0.3). Contract: contracts/events.md section 2.
//
// Pi events used (Pi 1.4.0 dist/core/extensions/types.d.ts and
// docs/extensions.md:103-121):
//   before_agent_start (types.d.ts:720-740)  -> user_prompt, and a new prompt.id
//   tool_execution_start (:853-860)          -> start time and arguments, kept
//   the gate's decision on pi.events         -> tool_decision
//   tool_execution_end (:872-883)            -> tool_result (durationMs is Pi's own)
//   message_end, assistant role (:848-851;   -> api_request, assistant_response
//     pi-ai AssistantMessage :368-393)
//   session_start / session_shutdown         -> sequence reset / final flush
//
// Content is off unless TK_LOG_CONTENT=1: hashes, sizes and names only. With
// it on, prompt, response and tool input are sent redacted and cut to 4 KiB.
// Nothing is sent without TK_CHAT_TOKEN and a loopback TK_OTLP_ENDPOINT.
//
// Corner case 46: the factory registers handlers and one bus listener. The
// environment is read, and the sender's timer started, on the first record.

import { createHash, randomUUID } from "node:crypto";
import { arch, platform } from "node:os";
import { Sender, TOKEN_PATTERN, logsUrl, record, type AttrValue, type FetchLike, type SenderCounts } from "../src/otlp.ts";
import {
  DECISION_CHANNEL,
  type AgentMessageLike,
  type BeforeAgentStartEvent,
  type DecisionNotice,
  type MessageEndEvent,
  type PiApi,
  type PiContext,
  type ToolExecutionEndEvent,
  type ToolExecutionStartEvent,
} from "../src/pi-api.ts";
import { redactText } from "../src/redact.ts";
import { harnessId } from "../src/harness.ts";

const CONTENT_MAX = 4096;
const MAX_PENDING = 2000;

export interface EmitterOptions {
  env?: () => Record<string, string | undefined>;
  fetch?: FetchLike;
  now?: () => number;
  newId?: () => string;
  intervalMs?: number;
}

export interface EmitterStatus {
  /** Why nothing is sent, when nothing is. */
  refused?: string;
  counts: SenderCounts & { notSent: number };
}

export type EmitterFactory = ((pi: PiApi) => void) & { status(): EmitterStatus };

const DECISIONS = new Set(["accept", "reject", "ask_accept", "ask_reject", "ask_timeout"]);

function isNotice(d: unknown): d is DecisionNotice {
  if (typeof d !== "object" || d === null) return false;
  const n = d as Record<string, unknown>;
  return (
    typeof n.toolName === "string" &&
    typeof n.toolNameNormalized === "string" &&
    typeof n.toolCallId === "string" &&
    typeof n.reason === "string" &&
    typeof n.decision === "string" &&
    DECISIONS.has(n.decision)
  );
}

function sha256(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

function clipRedacted(text: string): string {
  const r = redactText(text).text;
  return r.length > CONTENT_MAX ? `${r.slice(0, CONTENT_MAX)}…` : r;
}

function safeJson(v: unknown): string {
  try {
    return JSON.stringify(v) ?? "null";
  } catch {
    return "\"[unserializable]\"";
  }
}

function textOf(message: AgentMessageLike): string {
  if (typeof message.content === "string") return message.content;
  if (!Array.isArray(message.content)) return "";
  return message.content
    .filter((c): c is { type: "text"; text: string } => typeof c === "object" && c !== null && (c as { type?: unknown }).type === "text" && typeof (c as { text?: unknown }).text === "string")
    .map((c) => c.text)
    .join("");
}

export function createEmitter(options: EmitterOptions = {}): EmitterFactory {
  const now = options.now ?? Date.now;
  const newId = options.newId ?? randomUUID;
  let sender: Sender | undefined;
  let refused: string | undefined;
  let configured = false;
  let content = false;
  let notSent = 0;

  const configure = () => {
    if (configured) return;
    configured = true;
    const env = options.env ? options.env() : process.env;
    content = env.TK_LOG_CONTENT === "1";
    const token = env.TK_CHAT_TOKEN;
    if (token === undefined || token === "") {
      refused = "no TK_CHAT_TOKEN";
      return;
    }
    if (!TOKEN_PATTERN.test(token)) {
      refused = "TK_CHAT_TOKEN is not a token";
      return;
    }
    const target = logsUrl(env.TK_OTLP_ENDPOINT);
    if ("refused" in target) {
      refused = target.refused;
      return;
    }
    const fetchImpl = options.fetch ?? (globalThis.fetch as unknown as FetchLike | undefined);
    if (!fetchImpl) {
      refused = "no fetch in this runtime";
      return;
    }
    sender = new Sender({
      url: target.url,
      token,
      fetch: fetchImpl,
      resource: { arch: arch(), os: platform(), service: harnessId() },
      ...(options.intervalMs !== undefined ? { intervalMs: options.intervalMs } : {}),
    });
  };

  const contentOn = () => {
    configure();
    return content;
  };

  const factory = function emitter(pi: PiApi): void {
    let sessionId: string | undefined;
    let promptId: string | undefined;
    let sequence = 0;
    const pending = new Map<string, { startMs: number; args: unknown }>();

    const send = (shortName: string, attrs: Record<string, AttrValue | undefined>, ctx?: PiContext) => {
      configure();
      if (!sender) {
        notSent++;
        return;
      }
      if (ctx) {
        try {
          sessionId = ctx.sessionManager.getSessionId();
        } catch {
          // keep the last one
        }
      }
      sequence++;
      const t = now();
      sender.enqueue(
        record(
          shortName,
          {
            "session.id": sessionId,
            "prompt.id": promptId,
            "event.sequence": sequence,
            "event.timestamp": new Date(t).toISOString(),
            ...attrs,
          },
          t,
        ),
      );
    };

    pi.on("session_start", () => {
      sequence = 0;
      promptId = undefined;
      pending.clear();
    });

    pi.on("session_shutdown", async () => {
      pending.clear();
      if (sender) await sender.close(2000);
    });

    pi.on("before_agent_start", (event: BeforeAgentStartEvent, ctx: PiContext) => {
      promptId = newId();
      if (event.messageType !== "user") return;
      const prompt = typeof event.prompt === "string" ? event.prompt : "";
      send("user_prompt", { prompt_length: prompt.length, prompt: contentOn() ? clipRedacted(prompt) : undefined }, ctx);
    });

    pi.on("tool_execution_start", (event: ToolExecutionStartEvent) => {
      if (pending.size >= MAX_PENDING) {
        const oldest = pending.keys().next().value;
        if (oldest !== undefined) pending.delete(oldest);
      }
      pending.set(event.toolCallId, { startMs: now(), args: event.args });
    });

    pi.events.on(DECISION_CHANNEL, (data: unknown) => {
      if (!isNotice(data)) return;
      send("tool_decision", {
        tool_name: data.toolName,
        tool_name_normalized: data.toolNameNormalized,
        tool_use_id: data.toolCallId,
        decision: data.decision,
        source: "tk-policy",
        reason: data.reason,
      });
    });

    pi.on("tool_execution_end", (event: ToolExecutionEndEvent, ctx: PiContext) => {
      const started = pending.get(event.toolCallId);
      pending.delete(event.toolCallId);
      const body = safeJson((event.result as { content?: unknown } | null | undefined)?.content ?? event.result ?? null);
      const duration = event.durationMs ?? (started ? now() - started.startMs : undefined);
      send(
        "tool_result",
        {
          tool_name: event.toolName,
          tool_use_id: event.toolCallId,
          parent_tool_use_id: event.parentToolCallId,
          success: !event.isError,
          duration_ms: duration === undefined ? undefined : Math.max(0, Math.round(duration)),
          tool_result_size_bytes: Buffer.byteLength(body, "utf8"),
          tool_result_sha256: sha256(body),
          tool_input: contentOn() && started ? clipRedacted(safeJson(started.args)) : undefined,
        },
        ctx,
      );
    });

    pi.on("message_end", (event: MessageEndEvent, ctx: PiContext) => {
      const m = event.message;
      if (!m || m.role !== "assistant") return;
      const model = typeof m.model === "string" ? m.model : undefined;
      const duration = typeof m.timestamp === "number" ? now() - m.timestamp : undefined;
      send(
        "api_request",
        {
          model,
          provider: typeof m.provider === "string" ? m.provider : undefined,
          input_tokens: typeof m.usage?.input === "number" ? m.usage.input : undefined,
          output_tokens: typeof m.usage?.output === "number" ? m.usage.output : undefined,
          duration_ms: duration === undefined ? undefined : Math.max(0, Math.round(duration)),
          stop_reason: typeof m.stopReason === "string" ? m.stopReason : undefined,
        },
        ctx,
      );
      const text = textOf(m);
      send("assistant_response", { model, response_length: text.length, response: contentOn() ? clipRedacted(text) : undefined }, ctx);
    });
  } as EmitterFactory;

  factory.status = () => ({
    ...(refused !== undefined ? { refused } : {}),
    counts: { ...(sender?.counts ?? { queued: 0, sent: 0, failed: 0, dropped: 0 }), notSent },
  });
  return factory;
}

export default createEmitter();
