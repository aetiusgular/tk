// OpenClaw plugin entry (T2.4). The shape definePluginEntry returns (it only
// returns its argument, dist/plugin-entry-8p_0SEiv.mjs:13-27), written out so
// the plugin imports nothing from openclaw. register() only registers: no
// file is read until the first tool call.
import { TK_PRIORITY, TK_TIMEOUT_MS, afterToolCall, beforeToolCall, defaultDeps, type AfterEvent, type BeforeCtx, type BeforeEvent } from "./src/openclaw.ts";

type Api = { on: (hook: string, handler: (event: never, ctx: never) => unknown, opts?: Record<string, unknown>) => void };

export default {
  id: "tk",
  name: "tk",
  description: "tk policy gate: checks every tool call against ~/.tk/policy.json before it runs.",
  register(api: Api): void {
    const deps = defaultDeps(process.env);
    api.on("before_tool_call", ((event: BeforeEvent, ctx: BeforeCtx) => beforeToolCall(event, ctx, deps)) as never, { priority: TK_PRIORITY, timeoutMs: TK_TIMEOUT_MS });
    api.on("after_tool_call", ((event: AfterEvent, ctx: BeforeCtx) => afterToolCall(event, ctx, deps)) as never);
  },
};
