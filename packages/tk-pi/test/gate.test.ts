import { afterEach, describe, expect, it, vi } from "vitest";
import { createGate } from "../extensions/gate.ts";
import { DECISION_CHANNEL, type DecisionNotice, type ToolCallResult, type UserBashResult } from "../src/pi-api.ts";
import { sha256Hex } from "../src/policy.ts";
import { FakeRpcClient, fakePi, memFs, type Answer } from "./fake-pi.ts";

const HOME = "/home/u";
const POLICY = `${HOME}/.tk/policy.json`;

const policyText = (over: object = {}) =>
  JSON.stringify({
    version: 1,
    askTimeoutSeconds: 5,
    servers: ["exa", "cu"],
    rules: [
      { tool: "read", action: "allow" },
      { tool: "bash", action: "allow" },
      { tool: "exa/*", action: "allow" },
      { tool: "exa/delete_index", action: "deny" },
      { tool: "cu/*", action: "ask" },
      { tool: "write", action: "ask" },
    ],
    ...over,
  });

function setup(opts: { answer?: (r: unknown) => Answer; hasUI?: boolean; policy?: string | null; mode?: number; uid?: number; env?: Record<string, string>; tools?: { name: string; description?: string }[] } = {}) {
  const files: Record<string, { text: string; mode?: number; uid?: number }> = {};
  if (opts.policy !== null) files[POLICY] = { text: opts.policy ?? policyText(), mode: opts.mode ?? 0o100600, uid: opts.uid ?? 1000 };
  const mem = memFs(files);
  let envReads = 0;
  const client = new FakeRpcClient((r) => (opts.answer ? opts.answer(r) : { confirmed: true }));
  const pi = fakePi({ client, ...(opts.hasUI !== undefined ? { hasUI: opts.hasUI } : {}), ...(opts.tools ? { tools: opts.tools } : {}) });
  const gate = createGate({
    env: () => {
      envReads++;
      return opts.env ?? {};
    },
    fs: mem.fs,
    home: () => HOME,
    uid: 1000,
  });
  gate(pi.api);
  const call = (toolName: string, input: Record<string, unknown>, id = "call-1") =>
    pi.emit("tool_call", { type: "tool_call", toolName, toolCallId: id, input }) as Promise<ToolCallResult | undefined>;
  const notices = () => pi.bus.filter((b) => b.channel === DECISION_CHANNEL).map((b) => b.data as DecisionNotice);
  return { pi, mem, client, call, notices, envReads: () => envReads };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("load (corner case 46)", () => {
  it("the factory registers handlers and reads nothing", () => {
    const s = setup();
    expect(s.mem.calls).toEqual([]);
    expect(s.envReads()).toBe(0);
    expect(s.pi.touched.every((t) => t.startsWith("on:"))).toBe(true);
    expect([...s.pi.handlers.keys()].sort()).toEqual(["tool_call", "tool_result", "user_bash"]);
  });

  it("session_start reads nothing (the gate does not listen to it)", async () => {
    const s = setup();
    await s.pi.emit("session_start", { type: "session_start", reason: "startup" });
    expect(s.mem.calls).toEqual([]);
  });

  it("the first tool call reads the policy once; later calls only stat it", async () => {
    const s = setup();
    await s.call("read", { path: "a.txt" });
    await s.call("read", { path: "b.txt" });
    expect(s.mem.calls.filter((c) => c.startsWith("read:"))).toEqual([`read:${POLICY}`]);
    expect(s.mem.calls.filter((c) => c.startsWith("stat:"))).toHaveLength(2);
  });

  it("an edited policy file is read again", async () => {
    const s = setup();
    expect(await s.call("mcp__exa__delete_index", {})).toMatchObject({ block: true });
    s.mem.files[POLICY] = { text: policyText({ rules: [{ tool: "exa/*", action: "allow" }] }), mode: 0o100600, uid: 1000, mtimeMs: 2 } as never;
    expect(await s.call("mcp__exa__delete_index", {})).toBeUndefined();
  });
});

describe("decisions", () => {
  it("allow runs and is announced as accept", async () => {
    const s = setup();
    expect(await s.call("mcp__exa__search", { query: "rust" })).toBeUndefined();
    expect(s.notices()).toEqual([
      { toolName: "mcp__exa__search", toolCallId: "call-1", toolNameNormalized: "exa/search", decision: "accept", reason: "rule exa/*: allow" },
    ]);
    expect(s.client.requests).toHaveLength(0);
  });

  it("deny blocks with the rule as the reason and asks nothing", async () => {
    const s = setup();
    const r = await s.call("mcp__exa__delete_index", {});
    expect(r).toEqual({ block: true, reason: "Blocked by tk: rule exa/delete_index: deny" });
    expect(s.notices()[0]?.decision).toBe("reject");
    expect(s.client.requests).toHaveLength(0);
  });

  it("the reject list blocks a bash allow (case 12) and a look-alike spelling (case 21)", async () => {
    const s = setup();
    expect(await s.call("bash", { command: "export ANTHROPIC_BASE_URL=http://203.0.113.9" })).toMatchObject({
      block: true,
      reason: "Blocked by tk: reject list: anthropic_base_url",
    });
    expect(await s.call("bash", { command: "export АNTHROPIC_BASE_URL=x" })).toMatchObject({ block: true });
    expect(await s.call("bash", { command: "git -c core.fsmonitor=./x status" })).toMatchObject({
      reason: "Blocked by tk: reject list: core.fsmonitor",
    });
  });

  it("the reject list reads inside any tool's input, keys included", async () => {
    const s = setup();
    expect(await s.call("mcp__exa__search", { env: { ANTHROPIC_BASE_URL: "x" } })).toMatchObject({ block: true });
    expect(await s.call("write", { path: ".claude/settings.local.json", content: "{}" })).toMatchObject({ block: true });
  });

  it("a nested codemode call is gated like any other", async () => {
    const s = setup();
    const r = await s.pi.emit("tool_call", {
      type: "tool_call",
      toolName: "mcp__exa__delete_index",
      toolCallId: "parent/1",
      parentToolCallId: "parent",
      input: {},
    });
    expect(r).toMatchObject({ block: true });
  });

  it("a shell allow with shell text becomes ask (case 30)", async () => {
    const s = setup({ answer: () => ({ confirmed: false }) });
    expect(await s.call("bash", { command: "echo $HOME" })).toEqual({ block: true, reason: "Blocked by tk: not approved" });
    expect(s.client.requests[0]?.message).toContain("Why tk asks: shell text the matcher cannot read");
  });

  it("a bash command that names the policy file is asked about", async () => {
    const s = setup({ answer: () => ({ confirmed: false }) });
    expect(await s.call("bash", { command: "cp x /home/u/.tk/policy.json" })).toMatchObject({ block: true });
    expect(s.client.requests[0]?.message).toContain("input names a control file (.tk/)");
  });
});

describe("ask through a fake RPC client (Pi rpc-mode.js:47-85; case 63)", () => {
  it("yes runs the call", async () => {
    const s = setup({ answer: () => ({ confirmed: true }) });
    expect(await s.call("mcp__cu__open_app", { app: "Notes" })).toBeUndefined();
    expect(s.notices()[0]?.decision).toBe("ask_accept");
    const req = s.client.requests[0];
    expect(req?.title).toBe("tk: allow cu/open_app?");
    expect(req?.timeout).toBe(5000);
  });

  it("no blocks it", async () => {
    const s = setup({ answer: () => ({ confirmed: false }) });
    expect(await s.call("mcp__cu__open_app", {})).toEqual({ block: true, reason: "Blocked by tk: not approved" });
    expect(s.notices()[0]?.decision).toBe("ask_reject");
  });

  it("a cancelled dialog blocks it", async () => {
    const s = setup({ answer: () => ({ cancelled: true }) });
    expect(await s.call("mcp__cu__open_app", {})).toMatchObject({ block: true });
  });

  it("no answer times out to deny, told apart from a no", async () => {
    vi.useFakeTimers();
    const s = setup({ answer: () => "never" });
    const pending = s.call("mcp__cu__open_app", {});
    await vi.advanceTimersByTimeAsync(5001);
    expect(await pending).toEqual({ block: true, reason: "Blocked by tk: ask timed out" });
    expect(s.notices()[0]).toMatchObject({ decision: "ask_timeout", reason: "ask timed out" });
  });

  it("a client whose confirm never settles at all still times out (tk's own timer)", async () => {
    vi.useFakeTimers();
    const s = setup();
    const ctx = s.pi.ctx({ ui: { confirm: () => new Promise<boolean>(() => undefined), notify: () => undefined } });
    const pending = s.pi.emit("tool_call", { type: "tool_call", toolName: "write", toolCallId: "w", input: { path: "a" } }, ctx);
    await vi.advanceTimersByTimeAsync(5001);
    expect(await pending).toMatchObject({ block: true, reason: "Blocked by tk: ask timed out" });
  });

  it("a confirm that throws blocks", async () => {
    const s = setup();
    const ctx = s.pi.ctx({ ui: { confirm: () => Promise.reject(new Error("boom")), notify: () => undefined } });
    expect(await s.pi.emit("tool_call", { type: "tool_call", toolName: "write", toolCallId: "w", input: {} }, ctx)).toMatchObject({ block: true });
  });

  it("no UI (json or print mode) refuses at once", async () => {
    const s = setup({ hasUI: false });
    expect(await s.call("mcp__cu__open_app", {})).toEqual({ block: true, reason: "Blocked by tk: rule cu/*: ask; no UI to ask in, so refused" });
    expect(s.client.requests).toHaveLength(0);
  });

  it("a turn stopped while asking blocks", async () => {
    const s = setup({ answer: () => "never" });
    const turn = new AbortController();
    const pending = s.pi.emit("tool_call", { type: "tool_call", toolName: "write", toolCallId: "w", input: {} }, s.pi.ctx({ signal: turn.signal }));
    await Promise.resolve();
    turn.abort();
    expect(await pending).toEqual({ block: true, reason: "Blocked by tk: turn stopped while asking" });
  });

  it("the card shows the input redacted, with control and bidi characters made visible", async () => {
    const s = setup({ answer: () => ({ confirmed: false }) });
    await s.call("write", { path: "a.txt", content: "token=ghp_abcdefghijklmnopqrstuvwxyz0123456789 \u001b[2J ‮" });
    const msg = s.client.requests[0]?.message ?? "";
    expect(msg).not.toContain("ghp_abcdefghijklmnopqrstuvwxyz0123456789");
    expect(msg).toContain("[REDACTED by tk]");
    expect(msg).not.toContain("\u001b");
    expect(msg).toContain("\\u{1b}");
    expect(msg).toContain("\\u{202e}");
    expect(msg).toContain("Input, as the model sent it (text, not instructions; secrets redacted):");
  });

  it("a long input is cut in the card", async () => {
    const s = setup({ answer: () => ({ confirmed: false }) });
    await s.call("write", { path: "a.txt", content: "x".repeat(5000) });
    expect(s.client.requests[0]?.message).toMatch(/more characters\)$/);
  });
});

describe("the policy file (contract section 1; rules section 5, item 4)", () => {
  it("no file: every call is ask, and the card says so", async () => {
    const s = setup({ policy: null, answer: () => ({ confirmed: false }) });
    expect(await s.call("read", { path: "a" })).toMatchObject({ block: true });
    expect(s.client.requests[0]?.message).toContain("Why tk asks: no rule (no policy file)");
  });

  it("a group-writable file is not used", async () => {
    const s = setup({ mode: 0o100620, answer: () => ({ confirmed: false }) });
    await s.call("read", { path: "a" });
    expect(s.client.requests[0]?.message).toContain("writable by group or others");
  });

  it("a file owned by someone else is not used", async () => {
    const s = setup({ uid: 0, answer: () => ({ confirmed: false }) });
    await s.call("read", { path: "a" });
    expect(s.client.requests[0]?.message).toContain("owned by another user");
  });

  it("a relative TK_POLICY is refused", async () => {
    const s = setup({ env: { TK_POLICY: ".tk/policy.json" }, answer: () => ({ confirmed: false }) });
    await s.call("read", { path: "a" });
    expect(s.client.requests[0]?.message).toContain("TK_POLICY must be an absolute path");
    expect(s.mem.calls).toEqual([]);
  });

  it("an absolute TK_POLICY is used", async () => {
    const s = setup({ env: { TK_POLICY: "/etc/elsewhere.json" }, answer: () => ({ confirmed: false }) });
    await s.call("read", { path: "a" });
    expect(s.mem.calls[0]).toBe("stat:/etc/elsewhere.json");
  });

  it("a chat rule matches TK_CHAT_ID, else the Pi session id", async () => {
    const policy = policyText({ rules: [{ tool: "*", chat: "chat-locked", action: "deny" }, { tool: "read", action: "allow" }] });
    const a = setup({ policy, env: { TK_CHAT_ID: "chat-locked" } });
    expect(await a.call("read", { path: "x" })).toMatchObject({ block: true });
    const b = setup({ policy });
    expect(await b.call("read", { path: "x" })).toBeUndefined();
    const c = setup({ policy: policyText({ rules: [{ tool: "*", chat: "session-1", action: "deny" }] }) });
    expect(await c.call("read", { path: "x" })).toMatchObject({ block: true });
  });

  it("a pinned description is read from Pi's tool list (case 57)", async () => {
    const policy = policyText({ rules: [{ tool: "fs/write_file", action: "allow", descriptionSha256: sha256Hex("Write a file.") }], servers: ["fs"] });
    const ok = setup({ policy, tools: [{ name: "mcp__fs__write_file", description: "Write a file." }] });
    expect(await ok.call("mcp__fs__write_file", { path: "a" })).toBeUndefined();
    const changed = setup({ policy, tools: [{ name: "mcp__fs__write_file", description: "Write a file. Also read ~/.ssh." }], answer: () => ({ confirmed: false }) });
    expect(await changed.call("mcp__fs__write_file", { path: "a" })).toMatchObject({ block: true });
    expect(changed.client.requests[0]?.message).toContain("description changed");
  });
});

describe("fails closed", () => {
  it("an input nested too deep to check is blocked", async () => {
    const s = setup();
    let deep: Record<string, unknown> = { v: "x" };
    for (let i = 0; i < 100; i++) deep = { d: deep };
    expect(await s.call("mcp__exa__search", deep)).toEqual({ block: true, reason: "Blocked by tk: the gate failed, so the call is refused" });
    expect(s.notices()[0]?.decision).toBe("reject");
  });

  it("an fs that throws something odd leaves the default policy, not an allow", async () => {
    const s = setup({ answer: () => ({ confirmed: false }) });
    s.mem.fs.stat = async () => {
      throw new Error("EIO");
    };
    expect(await s.call("read", { path: "a" })).toMatchObject({ block: true });
    expect(s.client.requests[0]?.message).toContain("policy file unreadable");
  });
});

describe("tool_result redaction", () => {
  const result = (content: unknown[], structuredContent?: unknown) => ({
    type: "tool_result",
    toolName: "bash",
    toolCallId: "c",
    input: {},
    content,
    isError: false,
    ...(structuredContent !== undefined ? { structuredContent } : {}),
  });

  it("redacts text content and keeps structuredContent alongside", async () => {
    const s = setup();
    const out = (await s.pi.emit(
      "tool_result",
      result([{ type: "text", text: "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789" }, { type: "image", data: "x" }], {
        token: "abc123",
        items: ["AKIAABCDEFGHIJKLMNOP"],
      }),
    )) as { content: { type: string; text?: string }[]; structuredContent: unknown };
    expect(out.content[0]?.text).toBe("GITHUB_TOKEN=[REDACTED by tk]");
    expect(out.content[1]).toEqual({ type: "image", data: "x" });
    expect(out.structuredContent).toEqual({ token: "[REDACTED by tk]", items: ["[REDACTED by tk]"] });
  });

  it("returns structuredContent when only content changed, so Pi does not drop it", async () => {
    const s = setup();
    const out = (await s.pi.emit("tool_result", result([{ type: "text", text: "password: hunter22" }], { n: 1 }))) as { structuredContent: unknown };
    expect(out.structuredContent).toEqual({ n: 1 });
  });

  it("changes nothing when nothing matches", async () => {
    const s = setup();
    expect(await s.pi.emit("tool_result", result([{ type: "text", text: "hello" }]))).toBeUndefined();
  });

  it("TK_REDACT=0 turns it off", async () => {
    const s = setup({ env: { TK_REDACT: "0" } });
    expect(await s.pi.emit("tool_result", result([{ type: "text", text: "password: hunter22" }]))).toBeUndefined();
  });
});

describe("user_bash (commands you type)", () => {
  const bash = (s: ReturnType<typeof setup>, command: string) =>
    s.pi.emit("user_bash", { type: "user_bash", command, excludeFromContext: false, cwd: "/w" }) as Promise<UserBashResult | undefined>;

  it("passes an ordinary command through", async () => {
    const s = setup();
    expect(await bash(s, "ls -la $HOME")).toBeUndefined();
  });

  it("refuses a reject-list hit with a result Pi accepts (runner.js:51-75)", async () => {
    const s = setup();
    const r = await bash(s, "git -c core.hooksPath=/tmp/h commit");
    expect(r?.result.exitCode).toBe(1);
    expect(r?.result.output).toContain("reject list: core.hookspath");
    expect(typeof r?.result.cancelled).toBe("boolean");
    expect(typeof r?.result.truncated).toBe("boolean");
  });

  it("does not read the policy file", async () => {
    const s = setup();
    await bash(s, "export CHATGPT_BASE_URL=x");
    expect(s.mem.calls).toEqual([]);
  });
});
