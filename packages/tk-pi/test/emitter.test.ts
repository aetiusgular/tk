import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createEmitter } from "../extensions/emitter.ts";
import { logsUrl, record, Sender, type FetchLike } from "../src/otlp.ts";
import { DECISION_CHANNEL } from "../src/pi-api.ts";
import { fakePi } from "./fake-pi.ts";

const here = dirname(fileURLToPath(import.meta.url));
const fixture = JSON.parse(readFileSync(join(here, "..", "..", "..", "tests", "fixtures", "otlp", "claude-2.1.302-logs.json"), "utf8"));

const TOKEN = "a".repeat(64);
const ENV = { TK_CHAT_TOKEN: TOKEN, TK_OTLP_ENDPOINT: "http://127.0.0.1:4318" };

interface Posted {
  url: string;
  headers: Record<string, string>;
  body: any;
}

function setup(env: Record<string, string> = ENV, fetchImpl?: FetchLike) {
  const posts: Posted[] = [];
  let envReads = 0;
  const fetch: FetchLike =
    fetchImpl ??
    (async (url, init) => {
      posts.push({ url, headers: init.headers, body: JSON.parse(init.body) });
      return { ok: true, status: 200 };
    });
  let t = 1_792_204_985_000;
  const factory = createEmitter({
    env: () => {
      envReads++;
      return env;
    },
    fetch,
    now: () => t,
    newId: () => "prompt-1",
    intervalMs: 1000,
  });
  const pi = fakePi({ sessionId: "sess-9" });
  factory(pi.api);
  return {
    pi,
    factory,
    posts,
    envReads: () => envReads,
    tick: (ms: number) => {
      t += ms;
    },
  };
}

const attrs = (r: any): Record<string, any> =>
  Object.fromEntries(r.attributes.map((a: any) => [a.key, Object.values(a.value)[0]]));

const allRecords = (posts: Posted[]) => posts.flatMap((p) => p.body.resourceLogs[0].scopeLogs[0].logRecords);

afterEach(() => {
  vi.useRealTimers();
});

describe("load (corner case 46)", () => {
  it("registers handlers and one bus listener; reads no environment", () => {
    const s = setup();
    expect(s.envReads()).toBe(0);
    expect(s.pi.touched.filter((t) => !t.startsWith("on:"))).toEqual(["events"]);
    expect([...s.pi.handlers.keys()].sort()).toEqual([
      "before_agent_start",
      "message_end",
      "session_shutdown",
      "session_start",
      "tool_execution_end",
      "tool_execution_start",
    ]);
  });

  it("session_start reads nothing and starts no timer", async () => {
    vi.useFakeTimers();
    const s = setup();
    await s.pi.emit("session_start", { type: "session_start", reason: "startup" });
    expect(s.envReads()).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe("records (contracts/events.md section 2)", () => {
  it("a tool call: decision and result, short event.name, pi.* body, joined by tool_use_id", async () => {
    vi.useFakeTimers();
    const s = setup();
    await s.pi.emit("before_agent_start", { type: "before_agent_start", messageType: "user", prompt: "list files" });
    await s.pi.emit("tool_execution_start", { type: "tool_execution_start", toolCallId: "tc-1", toolName: "bash", args: { command: "ls" } });
    s.pi.api.events.emit(DECISION_CHANNEL, {
      toolName: "bash",
      toolNameNormalized: "bash",
      toolCallId: "tc-1",
      decision: "accept",
      reason: "rule bash: allow",
    });
    s.tick(90);
    await s.pi.emit("tool_execution_end", {
      type: "tool_execution_end",
      toolCallId: "tc-1",
      toolName: "bash",
      result: { content: [{ type: "text", text: "a.txt\n" }] },
      isError: false,
      durationMs: 87.4,
    });
    await vi.advanceTimersByTimeAsync(1000);
    expect(s.posts).toHaveLength(1);
    const post = s.posts[0]!;
    expect(post.url).toBe("http://127.0.0.1:4318/v1/logs");
    expect(post.headers["x-tk-token"]).toBe(TOKEN);
    expect(post.headers["content-type"]).toBe("application/json");
    const res = post.body.resourceLogs[0];
    expect(attrs(res.resource)["service.name"]).toBe("pi");
    const recs = allRecords(s.posts);
    expect(recs.map((r: any) => r.body.stringValue)).toEqual(["pi.user_prompt", "pi.tool_decision", "pi.tool_result"]);
    const [prompt, decision, result] = recs.map(attrs) as [Record<string, any>, Record<string, any>, Record<string, any>];
    expect(prompt).toMatchObject({ "event.name": "user_prompt", "session.id": "sess-9", "prompt.id": "prompt-1", "event.sequence": "1", prompt_length: "10" });
    expect(prompt.prompt).toBeUndefined();
    expect(decision).toMatchObject({ "event.name": "tool_decision", tool_name: "bash", tool_name_normalized: "bash", tool_use_id: "tc-1", decision: "accept", source: "tk-policy", reason: "rule bash: allow" });
    expect(result).toMatchObject({ "event.name": "tool_result", tool_name: "bash", tool_use_id: "tc-1", success: true, duration_ms: "87" });
    const body = JSON.stringify([{ type: "text", text: "a.txt\n" }]);
    expect(result.tool_result_size_bytes).toBe(String(Buffer.byteLength(body)));
    expect(result.tool_result_sha256).toMatch(/^[0-9a-f]{64}$/);
    expect(result.tool_input).toBeUndefined();
    expect(recs[0].timeUnixNano).toBe("1792204985000000000");
  });

  it("the shape matches the recorded Claude Code 2.1.302 export the receiver decodes", () => {
    const claude = fixture.resourceLogs[0];
    const claudeResult = claude.scopeLogs[0].logRecords.find((r: any) => attrs(r)["event.name"] === "tool_result");
    const mine = record("tool_result", { tool_name: "bash", tool_use_id: "x", success: true, duration_ms: 3, tool_result_size_bytes: 4 }, 1);
    expect(Object.keys(mine).sort()).toEqual(["attributes", "body", "observedTimeUnixNano", "timeUnixNano"]);
    for (const k of ["timeUnixNano", "observedTimeUnixNano", "body", "attributes"]) expect(claudeResult).toHaveProperty(k);
    expect(typeof mine.timeUnixNano).toBe(typeof claudeResult.timeUnixNano);
    expect(Object.keys(mine.body)).toEqual(Object.keys(claudeResult.body));
    const theirs = attrs(claudeResult);
    for (const k of ["event.name", "tool_name", "tool_use_id", "success", "duration_ms", "tool_result_size_bytes"]) {
      expect(theirs, k).toHaveProperty(k);
      expect(attrs(mine), k).toHaveProperty(k);
    }
    expect(theirs["event.name"]).toBe("tool_result");
    expect(claudeResult.body.stringValue).toBe("claude_code.tool_result");
    expect(mine.body.stringValue).toBe("pi.tool_result");
  });

  it("an assistant message gives api_request and assistant_response; other roles give nothing", async () => {
    vi.useFakeTimers();
    const s = setup();
    await s.pi.emit("message_end", { type: "message_end", message: { role: "user", content: "hi" } });
    await s.pi.emit("message_end", {
      type: "message_end",
      message: { role: "assistant", content: [{ type: "text", text: "Hello" }, { type: "thinking", thinking: "x" }], model: "qwen3.8-27b", provider: "llama", usage: { input: 120, output: 5 }, stopReason: "stop", timestamp: 1_792_204_984_000 },
    });
    await vi.advanceTimersByTimeAsync(1000);
    const recs = allRecords(s.posts).map(attrs);
    expect(recs.map((r) => r["event.name"])).toEqual(["api_request", "assistant_response"]);
    expect(recs[0]).toMatchObject({ model: "qwen3.8-27b", input_tokens: "120", output_tokens: "5", duration_ms: "1000" });
    expect(recs[1]).toMatchObject({ response_length: "5" });
    expect(recs[1]?.response).toBeUndefined();
  });

  it("content on (TK_LOG_CONTENT=1): prompt, response and input are sent, redacted", async () => {
    vi.useFakeTimers();
    const s = setup({ ...ENV, TK_LOG_CONTENT: "1" });
    await s.pi.emit("before_agent_start", { type: "before_agent_start", messageType: "user", prompt: "use key sk-ant-abcdefghijklmnopqrstuvwxyz" });
    await s.pi.emit("tool_execution_start", { type: "tool_execution_start", toolCallId: "t", toolName: "write", args: { path: "a", content: "password=hunter22" } });
    await s.pi.emit("tool_execution_end", { type: "tool_execution_end", toolCallId: "t", toolName: "write", result: { content: [] }, isError: true });
    await vi.advanceTimersByTimeAsync(1000);
    const [p, r] = allRecords(s.posts).map(attrs);
    expect(p?.prompt).toBe("use key [REDACTED by tk]");
    expect(r?.tool_input).toContain("[REDACTED by tk]");
    expect(r?.tool_input).not.toContain("hunter22");
    expect(r?.success).toBe(false);
  });

  it("a custom-message run gets a prompt id but no user_prompt", async () => {
    vi.useFakeTimers();
    const s = setup();
    await s.pi.emit("before_agent_start", { type: "before_agent_start", messageType: "custom" });
    await vi.advanceTimersByTimeAsync(1000);
    expect(s.posts).toHaveLength(0);
  });

  it("a malformed decision on the bus is ignored", async () => {
    vi.useFakeTimers();
    const s = setup();
    s.pi.api.events.emit(DECISION_CHANNEL, { toolName: "x", decision: "maybe" });
    s.pi.api.events.emit(DECISION_CHANNEL, "accept");
    await vi.advanceTimersByTimeAsync(1000);
    expect(s.posts).toHaveLength(0);
  });

  it("the sequence restarts at a new session", async () => {
    vi.useFakeTimers();
    const s = setup();
    const msg = { type: "message_end", message: { role: "assistant", content: [] } };
    await s.pi.emit("message_end", msg);
    await s.pi.emit("session_start", { type: "session_start", reason: "new" });
    await s.pi.emit("message_end", msg);
    await vi.advanceTimersByTimeAsync(1000);
    expect(allRecords(s.posts).map((r: any) => attrs(r)["event.sequence"])).toEqual(["1", "2", "1", "2"]);
  });
});

describe("never blocks, never sends anywhere else", () => {
  it("no token: nothing is sent and nothing fetches", async () => {
    vi.useFakeTimers();
    const fetch = vi.fn<FetchLike>();
    const s = setup({ TK_OTLP_ENDPOINT: "http://127.0.0.1:4318" }, fetch);
    await s.pi.emit("message_end", { type: "message_end", message: { role: "assistant", content: [] } });
    await vi.advanceTimersByTimeAsync(2000);
    expect(fetch).not.toHaveBeenCalled();
    expect(s.factory.status()).toMatchObject({ refused: "no TK_CHAT_TOKEN", counts: { notSent: 2 } });
  });

  it("a token with header characters is refused", () => {
    const s = setup({ ...ENV, TK_CHAT_TOKEN: `${TOKEN}\r\nx-evil: 1` });
    void s.pi.emit("message_end", { type: "message_end", message: { role: "assistant", content: [] } });
    expect(s.factory.status().refused).toBe("TK_CHAT_TOKEN is not a token");
  });

  it.each([
    ["http://203.0.113.9:4318", "endpoint is not loopback"],
    ["http://localhost.evil.example:4318", "endpoint is not loopback"],
    ["http://127.0.0.1.nip.io:4318", "endpoint is not loopback"],
    ["http://user:pw@127.0.0.1:4318", "endpoint carries credentials"],
    ["http://127.0.0.1:4318/?a=b", "endpoint carries a query"],
    ["file:///etc/passwd", "endpoint is not http"],
    ["not a url", "endpoint is not a URL"],
  ])("endpoint %s is refused", (endpoint, why) => {
    expect(logsUrl(endpoint)).toEqual({ refused: why });
  });

  it.each([
    ["http://127.0.0.1:4318", "http://127.0.0.1:4318/v1/logs"],
    ["http://127.0.0.1:4318/", "http://127.0.0.1:4318/v1/logs"],
    ["http://localhost:9/v1/logs", "http://localhost:9/v1/logs"],
    ["http://[::1]:4318", "http://[::1]:4318/v1/logs"],
  ])("endpoint %s posts to %s", (endpoint, url) => {
    expect(logsUrl(endpoint)).toEqual({ url });
  });

  it("a failing or hanging receiver is counted and never delays a handler", async () => {
    vi.useFakeTimers();
    const hang: FetchLike = () => new Promise(() => undefined);
    const s = setup(ENV, hang);
    const t0 = Date.now();
    for (let i = 0; i < 50; i++) await s.pi.emit("message_end", { type: "message_end", message: { role: "assistant", content: [] } });
    expect(Date.now() - t0).toBeLessThan(500);
    const failing = setup(ENV, async () => {
      throw new Error("ECONNREFUSED");
    });
    await failing.pi.emit("message_end", { type: "message_end", message: { role: "assistant", content: [] } });
    await vi.advanceTimersByTimeAsync(1000);
    expect(failing.factory.status().counts).toMatchObject({ failed: 2, sent: 0 });
  });

  it("a non-2xx answer is counted as failed", async () => {
    vi.useFakeTimers();
    const s = setup(ENV, async () => ({ ok: false, status: 401 }));
    await s.pi.emit("message_end", { type: "message_end", message: { role: "assistant", content: [] } });
    await vi.advanceTimersByTimeAsync(1000);
    expect(s.factory.status().counts).toMatchObject({ failed: 2 });
  });

  it("redirects are refused and the request has a timeout", async () => {
    vi.useFakeTimers();
    const seen: { redirect: string; signal: AbortSignal }[] = [];
    const s = setup(ENV, async (_u, init) => {
      seen.push({ redirect: init.redirect, signal: init.signal });
      return { ok: true, status: 200 };
    });
    await s.pi.emit("message_end", { type: "message_end", message: { role: "assistant", content: [] } });
    await vi.advanceTimersByTimeAsync(1000);
    expect(seen[0]?.redirect).toBe("error");
    expect(seen[0]?.signal).toBeInstanceOf(AbortSignal);
  });

  it("session_shutdown flushes what is queued", async () => {
    const s = setup();
    await s.pi.emit("message_end", { type: "message_end", message: { role: "assistant", content: [] } });
    expect(s.posts).toHaveLength(0);
    await s.pi.emit("session_shutdown", { type: "session_shutdown" });
    expect(allRecords(s.posts)).toHaveLength(2);
  });

  it("the queue is bounded; overflow is counted, not kept", () => {
    const sender = new Sender({ url: "http://127.0.0.1:1/v1/logs", token: TOKEN, fetch: async () => ({ ok: true, status: 200 }), resource: { arch: "x", os: "y" }, maxQueue: 3, intervalMs: 60_000 });
    for (let i = 0; i < 5; i++) sender.enqueue(record("x", {}, 1));
    expect(sender.counts).toMatchObject({ queued: 3, dropped: 2 });
    return sender.close(100);
  });

  it("batches of more than maxBatch records go in several posts", async () => {
    const bodies: number[] = [];
    const sender = new Sender({
      url: "http://127.0.0.1:1/v1/logs",
      token: TOKEN,
      fetch: async (_u, init) => {
        bodies.push(JSON.parse(init.body).resourceLogs[0].scopeLogs[0].logRecords.length);
        return { ok: true, status: 200 };
      },
      resource: { arch: "x", os: "y" },
      maxBatch: 2,
      intervalMs: 60_000,
    });
    for (let i = 0; i < 5; i++) sender.enqueue(record("x", {}, 1));
    await sender.flush();
    expect(bodies).toEqual([2, 2, 1]);
    await sender.close(10);
  });
});
