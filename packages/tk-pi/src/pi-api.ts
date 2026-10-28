// The part of Pi's extension API that tk-pi uses, typed here so the package
// has no dependency on Pi at install or type-check time (Pi supplies its own
// modules to extensions at run time; tk-pi imports none of them).
//
// Source: @earendil-works/pi-coding-agent 1.4.0, dist/core/extensions/types.d.ts
// (npm tarball read 27 Oct 2026): ExtensionUIDialogOptions :40-45,
// ExtensionUIContext.confirm :76, ExtensionContext :213-240, ToolExecutionStart
// and End :853-883, UserBashEvent :898-906, ToolCallEventBase :931-940,
// ToolResultEventBase :985-1003, ToolCallEventResult :1089-1098,
// UserBashEventResult :1100-1108, ToolResultEventResult :1114-1120,
// ExtensionAPI.on :1190-1227, getAllTools :1284, events :1406, ToolInfo
// :1577-1582; core/event-bus.d.ts :1-4; core/bash-executor.d.ts :15-26;
// pi-ai 1.4.0 dist/types.d.ts AssistantMessage :368-393, Usage :285-298.
//
// Every field is the subset tk-pi reads. Pi's real types are wider; a real
// ExtensionAPI is assignable to `PiApi` (checked once against the 1.4.0
// declarations: see the lane A status doc, "Research").

export interface DialogOptions {
  signal?: AbortSignal;
  /** Milliseconds. Pi resolves `confirm` with `false` when it expires. */
  timeout?: number;
}

export interface PiUi {
  confirm(title: string, message: string, opts?: DialogOptions): Promise<boolean>;
  notify(message: string, type?: "info" | "warning" | "error"): void;
}

export interface PiSessionManager {
  getSessionId(): string;
}

export interface PiContext {
  ui: PiUi;
  mode: "tui" | "rpc" | "json" | "print";
  hasUI: boolean;
  cwd: string;
  sessionManager: PiSessionManager;
  signal: AbortSignal | undefined;
}

export interface ToolCallEvent {
  type: "tool_call";
  toolName: string;
  toolCallId: string;
  parentToolCallId?: string;
  input: Record<string, unknown>;
}

export interface ToolCallResult {
  block?: boolean;
  reason?: string;
}

export interface TextContent {
  type: "text";
  text: string;
}

export interface ImageContent {
  type: "image";
  data: string;
  mimeType: string;
}

/** pi-ai 1.4.0 dist/types.d.ts:308-311. */
export type JsonValue = null | boolean | number | string | readonly JsonValue[] | { [key: string]: JsonValue };

export interface ToolResultEvent {
  type: "tool_result";
  toolName: string;
  toolCallId: string;
  parentToolCallId?: string;
  input: Record<string, unknown>;
  content: (TextContent | ImageContent)[];
  structuredContent?: JsonValue;
  isError: boolean;
}

export interface ToolResultResult {
  content?: (TextContent | ImageContent)[];
  structuredContent?: JsonValue;
}

export interface UserBashEvent {
  type: "user_bash";
  command: string;
  excludeFromContext: boolean;
  cwd: string;
}

export interface BashResult {
  output: string;
  exitCode: number | undefined;
  cancelled: boolean;
  truncated: boolean;
}

export type UserBashResult = { result: BashResult };

export interface ToolExecutionStartEvent {
  type: "tool_execution_start";
  toolCallId: string;
  toolName: string;
  args: unknown;
  parentToolCallId?: string;
}

export interface ToolExecutionEndEvent {
  type: "tool_execution_end";
  toolCallId: string;
  toolName: string;
  result: unknown;
  isError: boolean;
  durationMs?: number;
  parentToolCallId?: string;
}

export interface Usage {
  input: number;
  output: number;
}

export interface AgentMessageLike {
  role: string;
  content?: unknown;
  model?: string;
  provider?: string;
  usage?: Usage;
  stopReason?: string;
  timestamp?: number;
}

export interface MessageEndEvent {
  type: "message_end";
  message: AgentMessageLike;
}

export interface BeforeAgentStartEvent {
  type: "before_agent_start";
  messageType: "user" | "custom";
  prompt?: string;
}

export interface SessionStartEvent {
  type: "session_start";
  reason: string;
}

export interface SessionShutdownEvent {
  type: "session_shutdown";
}

export interface ToolInfoLike {
  name: string;
  description?: string;
}

export interface EventBus {
  emit(channel: string, data: unknown): void;
  on(channel: string, handler: (data: unknown) => void): () => void;
}

type Handler<E, R = undefined> = (event: E, ctx: PiContext) => Promise<R | void> | R | void;

export interface PiApi {
  on(event: "tool_call", handler: Handler<ToolCallEvent, ToolCallResult>): () => void;
  on(event: "tool_result", handler: Handler<ToolResultEvent, ToolResultResult>): () => void;
  on(event: "user_bash", handler: Handler<UserBashEvent, UserBashResult>): () => void;
  on(event: "tool_execution_start", handler: Handler<ToolExecutionStartEvent>): () => void;
  on(event: "tool_execution_end", handler: Handler<ToolExecutionEndEvent>): () => void;
  on(event: "message_end", handler: Handler<MessageEndEvent>): () => void;
  on(event: "before_agent_start", handler: Handler<BeforeAgentStartEvent>): () => void;
  on(event: "session_start", handler: Handler<SessionStartEvent>): () => void;
  on(event: "session_shutdown", handler: Handler<SessionShutdownEvent>): () => void;
  getAllTools(): ToolInfoLike[];
  events: EventBus;
}

/** The channel the gate announces its decisions on, for the emitter. */
export const DECISION_CHANNEL = "tk-pi:tool_decision";

/** What the gate announces per decided call (the emitter's `tool_decision`). */
export interface DecisionNotice {
  toolName: string;
  toolNameNormalized: string;
  toolCallId: string;
  decision: "accept" | "reject" | "ask_accept" | "ask_reject" | "ask_timeout";
  reason: string;
}
