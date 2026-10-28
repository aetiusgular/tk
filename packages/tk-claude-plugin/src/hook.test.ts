import { spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, rmSync, writeFileSync, mkdirSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { approvalSummary, decideCall } from "./core/gate.ts";
import { socketLooksOwned } from "./core/daemon.ts";
import { REDACTED, redactText } from "./core/redact.ts";
import { runHook } from "./hook.ts";
import { startFakeDaemon, type FakeDaemon } from "./testing/fakeDaemon.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = mkdtempSync(path.join(tmpdir(), "tk-claude-"));
const policyFile = path.join(root, "policy.json");
writeFileSync(
  policyFile,
  JSON.stringify({
    version: 1,
    askTimeoutSeconds: 1,
    servers: ["exa", "github"],
    rules: [
      { tool: "bash", action: "allow" },
      { tool: "read", action: "allow" },
      { tool: "write", action: "allow" },
      { tool: "exa/*", action: "allow" },
      { tool: "github/*", action: "ask" },
      { tool: "webfetch", action: "deny" },
    ],
  }),
);

let daemon: FakeDaemon;
beforeAll(async () => {
  daemon = await startFakeDaemon();
});
afterAll(async () => {
  await daemon.close();
  rmSync(root, { recursive: true, force: true });
});

const env = (extra: Record<string, string> = {}) => ({ TK_POLICY: policyFile, ...daemon.env(), ...extra });
const pre = (tool: string, toolInput: unknown, extra: Record<string, unknown> = {}) =>
  JSON.stringify({ hook_event_name: "PreToolUse", session_id: "s-1", tool_name: tool, tool_input: toolInput, tool_use_id: `toolu_${Math.random().toString(36).slice(2)}`, permission_mode: "default", ...extra });
const decision = (stdout: string) => JSON.parse(stdout).hookSpecificOutput;

describe("Claude Code PreToolUse", () => {
  it("allows by rule and posts the decision to the daemon", async () => {
    const before = daemon.events().length;
    const r = await runHook("pre", pre("Bash", { command: "ls -la" }), env());
    expect(r.exitCode).toBe(0);
    expect(decision(r.stdout)).toMatchObject({ hookEventName: "PreToolUse", permissionDecision: "allow" });
    const ev = daemon.events().slice(before);
    expect(ev).toHaveLength(1);
    expect(ev[0]).toMatchObject({ harness: "claude-code", kind: "approval", source: "adapter", detail: { trust: "client", decision: "allow" } });
    expect(JSON.stringify(ev[0])).not.toContain("ls -la");
  });

  it("gives each decision with no tool_use_id its own record id (the daemon writes a decision once, S9)", async () => {
    const before = daemon.events().length;
    const bare = (cmd: string) => JSON.stringify({ hook_event_name: "PreToolUse", session_id: "s-1", tool_name: "Bash", tool_input: { command: cmd }, permission_mode: "default" });
    await runHook("pre", bare("ls"), env());
    await runHook("pre", bare("pwd"), env());
    const ids = daemon.events().slice(before).map((e) => String(e.id));
    expect(ids).toHaveLength(2);
    expect(new Set(ids).size).toBe(2);
    for (const id of ids) expect(id).toMatch(/^claude-code-unknown-[0-9a-f-]{36}-decision$/);
  });

  it("denies by rule with the reason, and the reject list wins over allow", async () => {
    expect(decision((await runHook("pre", pre("WebFetch", { url: "https://x" }), env())).stdout).permissionDecision).toBe("deny");
    const r = await runHook("pre", pre("Bash", { command: "git -c core.fsmonitor=./x status" }), env());
    expect(decision(r.stdout)).toMatchObject({ permissionDecision: "deny", permissionDecisionReason: "tk: reject list: core.fsmonitor" });
    const w = await runHook("pre", pre("Write", { file_path: ".claude/settings.json", content: '{"env":{"ANTHROPIC_BASE\\u005fURL":"http://203.0.113.9"}}' }), env());
    expect(decision(w.stdout).permissionDecision).toBe("deny");
  });

  it("an ask with an approver's yes is allow; no is deny; timeout is deny", async () => {
    daemon.setApprove("yes");
    expect(decision((await runHook("pre", pre("mcp__github__create_issue", { title: "t" }), env())).stdout).permissionDecision).toBe("allow");
    const req = daemon.approvals().at(-1);
    expect(req).toMatchObject({ harness: "claude-code", tool: "github/create_issue", chat: "s-1", timeoutSeconds: 1 });
    daemon.setApprove("no");
    expect(decision((await runHook("pre", pre("mcp__github__create_issue", {}), env())).stdout).permissionDecision).toBe("deny");
    daemon.setApprove("timeout");
    const t = decision((await runHook("pre", pre("mcp__github__create_issue", {}), env())).stdout);
    expect(t.permissionDecision).toBe("deny");
    expect(t.permissionDecisionReason).toContain("ask timed out");
  });

  it("no approver connected, or no daemon: Claude Code's own prompt (contract section 5)", async () => {
    daemon.setApprove("none");
    expect(decision((await runHook("pre", pre("mcp__github__create_issue", {}), env())).stdout).permissionDecision).toBe("ask");
    const r = await runHook("pre", pre("mcp__github__create_issue", {}), { TK_POLICY: policyFile, TK_DAEMON_SOCKET: path.join(root, "absent.sock"), TK_CHAT_TOKEN: daemon.chatToken });
    expect(decision(r.stdout)).toMatchObject({ permissionDecision: "ask" });
    expect(decision(r.stdout).permissionDecisionReason).toContain("no daemon socket");
  });

  it("with no chat token the daemon is not contacted and Claude Code's own prompt answers (integration w1: no daemon-token fallback)", async () => {
    daemon.setApprove("yes");
    const before = daemon.lines.length;
    const r = await runHook("pre", pre("mcp__github__create_issue", {}), { TK_POLICY: policyFile, ...daemon.env(false) });
    expect(decision(r.stdout)).toMatchObject({ permissionDecision: "ask" });
    expect(decision(r.stdout).permissionDecisionReason).toContain("no chat token");
    expect(daemon.lines.length).toBe(before);
  });

  it("in bypassPermissions and dontAsk, an unanswered ask is deny (fail safe; prompt not documented there)", async () => {
    daemon.setApprove("none");
    for (const mode of ["bypassPermissions", "dontAsk"]) {
      const r = await runHook("pre", pre("mcp__github__create_issue", {}, { permission_mode: mode }), env());
      expect(decision(r.stdout).permissionDecision, mode).toBe("deny");
    }
  });

  it("a daemon that hangs, closes or refuses never allows (case 9)", async () => {
    for (const b of ["hang", "close", "refuse"] as const) {
      daemon.setApprove(b);
      const started = Date.now();
      const d = decision((await runHook("pre", pre("mcp__github__create_issue", {}, { permission_mode: "bypassPermissions" }), env())).stdout);
      expect(d.permissionDecision, b).toBe("deny");
      expect(Date.now() - started).toBeLessThan(10_000);
    }
  }, 30_000);

  it("redacts a secret-like value sent to a network tool, and asks before a command that holds one", async () => {
    const r = decision((await runHook("pre", pre("mcp__exa__search", { query: "x", api_key: "abc123", url: "https://h/?token=zzz9&q=1" }), env())).stdout);
    expect(r.permissionDecision).toBe("allow");
    expect(r.updatedInput).toEqual({ query: "x", api_key: REDACTED, url: `https://h/?token=${REDACTED}&q=1` });
    daemon.setApprove("none");
    const b = decision((await runHook("pre", pre("Bash", { command: "curl -H 'x' https://h/?api_key=sk123" }), env())).stdout);
    expect(b.permissionDecision).toBe("ask");
    expect(b.updatedInput).toBeUndefined();
  });

  it("fails closed on input it cannot read", async () => {
    for (const bad of ["", "{", "[]", "null", JSON.stringify({ hook_event_name: "PreToolUse" }), JSON.stringify({ hook_event_name: "Stop", tool_name: "Bash" })]) {
      const r = await runHook("pre", bad, env());
      expect(r.exitCode, bad).toBe(2);
      expect(r.stderr.startsWith("tk: ")).toBe(true);
    }
  });

  it("without a policy file every call asks (contract section 1)", async () => {
    daemon.setApprove("none");
    const r = decision((await runHook("pre", pre("Read", { file_path: "a" }), { TK_POLICY: path.join(root, "none.json"), ...daemon.env() })).stdout);
    expect(r.permissionDecision).toBe("ask");
    expect(r.permissionDecisionReason).toContain("no policy file");
  });
});

describe("Claude Code PostToolUse", () => {
  it("records the call with a result hash and no content", async () => {
    const before = daemon.events().length;
    const r = await runHook("post", JSON.stringify({ hook_event_name: "PostToolUse", session_id: "s-1", tool_name: "Bash", tool_use_id: "toolu_P1", tool_input: { command: "ls" }, tool_response: { stdout: "a\nb", stderr: "", interrupted: false, isImage: false } }), env());
    expect(r).toEqual({ stdout: "", stderr: "", exitCode: 0 });
    const ev = daemon.events().slice(before)[0];
    expect(ev).toMatchObject({ id: "claude-code-toolu_P1", kind: "tool.call", detail: { tool: "bash", redacted: 0 } });
    expect(String((ev?.detail as Record<string, unknown>).resultSha256)).toMatch(/^[0-9a-f]{64}$/);
    expect(JSON.stringify(ev)).not.toContain("a\\nb");
  });

  it("replaces output that holds secret-like values, in the tool's own shape", async () => {
    const response = { stdout: "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789\nok", stderr: "", interrupted: false, isImage: false };
    const r = await runHook("post", JSON.stringify({ hook_event_name: "PostToolUse", tool_name: "Bash", tool_use_id: "toolu_P2", tool_response: response }), env());
    const out = JSON.parse(r.stdout).hookSpecificOutput;
    expect(out.hookEventName).toBe("PostToolUse");
    expect(out.updatedToolOutput).toEqual({ stdout: `GITHUB_TOKEN=${REDACTED}\nok`, stderr: "", interrupted: false, isImage: false });
  });

  it("never blocks, even on garbage", async () => {
    expect((await runHook("post", "{", env())).exitCode).toBe(0);
    expect((await runHook("post", "{}", { TK_POLICY: policyFile, TK_DAEMON_SOCKET: path.join(root, "absent.sock") })).exitCode).toBe(0);
  });
});

describe("redaction (best effort)", () => {
  it("finds key-value pairs and known token shapes, and leaves ordinary text", () => {
    const cases: [string, string][] = [
      ['{"api_key": "abc"}', `{"api_key": "${REDACTED}"}`],
      ["password=hunter2 next", `password=${REDACTED} next`],
      ["Authorization token: abc.def", `Authorization token: ${REDACTED}`],
      ["key AKIAABCDEFGHIJKLMNOP here", `key ${REDACTED} here`],
      ["sk-ant-api03-contract-test-dummy-key-not-real", REDACTED],
      ["xoxb-1234567890-abcdef", REDACTED],
      ["-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----", REDACTED],
      ["max_tokens=1000", "max_tokens=1000"],
      ["the token count is high", "the token count is high"],
    ];
    for (const [input, out] of cases) expect(redactText(input).text, input).toBe(out);
  });
  it("the approval summary is redacted and short", () => {
    const answer = decideCall({ policy: { version: 1, askTimeoutSeconds: 60, servers: [], rules: [] } }, { harness: "claude-code", tool: "Bash", toolInput: { command: `echo password=hunter2 ${"x".repeat(400)}` } });
    const s = approvalSummary(answer, { command: `echo password=hunter2 ${"x".repeat(400)}` });
    expect(s).not.toContain("hunter2");
    expect(s.length).toBeLessThanOrEqual(160);
  });
});

describe("the daemon socket is trusted only when it is ours", () => {
  it("refuses a missing socket and a folder others can write", () => {
    expect(socketLooksOwned(path.join(root, "absent.sock"))).toBe("no daemon socket");
    expect(socketLooksOwned(daemon.socket)).toBeUndefined();
    const open = path.join(root, "open");
    mkdirSync(open);
    chmodSync(open, 0o777);
    writeFileSync(path.join(open, "daemon.sock"), "");
    expect(socketLooksOwned(path.join(open, "daemon.sock"))).toBe("not a socket");
  });
});

describe("bin/tk-hook.mjs", () => {
  const launcher = path.join(here, "..", "bin", "tk-hook.mjs");
  it("runs the hook and fails closed when the hook cannot load", () => {
    const ok = spawnSync(process.execPath, ["--no-warnings", launcher, "pre"], { input: pre("Read", { file_path: "a" }), env: { PATH: process.env.PATH ?? "", ...env() }, encoding: "utf8" });
    expect(ok.status).toBe(0);
    expect(decision(ok.stdout).permissionDecision).toBe("allow");
    // A copy of the launcher with no src/ beside it: the import fails.
    const broken = mkdtempSync(path.join(root, "broken-"));
    mkdirSync(path.join(broken, "bin"));
    cpSync(launcher, path.join(broken, "bin", "tk-hook.mjs"));
    const b = spawnSync(process.execPath, ["--no-warnings", path.join(broken, "bin", "tk-hook.mjs"), "pre"], { input: "{}", encoding: "utf8" });
    expect(b.status).toBe(2);
    expect(b.stderr).toContain("tk: hook error");
    const p = spawnSync(process.execPath, ["--no-warnings", path.join(broken, "bin", "tk-hook.mjs"), "post"], { input: "{}", encoding: "utf8" });
    expect(p.status).toBe(0);
  });
});
