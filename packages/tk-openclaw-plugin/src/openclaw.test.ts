import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startFakeDaemon, type FakeDaemon } from "../../tk-claude-plugin/src/testing/fakeDaemon.ts";
import entry from "../index.ts";
import { REDACTED } from "./core/redact.ts";
import { TK_PRIORITY, afterToolCall, beforeToolCall, defaultDeps, toolForGate } from "./openclaw.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = mkdtempSync(path.join(tmpdir(), "tk-openclaw-"));
const policyFile = path.join(root, "policy.json");
writeFileSync(
  policyFile,
  JSON.stringify({
    version: 1,
    askTimeoutSeconds: 1,
    servers: ["exa", "my-srv"],
    rules: [
      { tool: "bash", action: "allow" },
      { tool: "read", action: "allow" },
      { tool: "exa/*", action: "allow" },
      { tool: "my_srv/*", action: "ask" },
      { tool: "web_fetch", action: "deny" },
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

const deps = (extra: Record<string, string> = {}) => defaultDeps({ TK_POLICY: policyFile, ...daemon.env(), ...extra });
const ctx = { sessionId: "ses-1", sessionKey: "agent:main" };

describe("before_tool_call", () => {
  it("an allow hands back the params tk checked, so they are the ones that run", async () => {
    const params = { command: "ls -la" };
    expect(await beforeToolCall({ toolName: "exec", params, toolCallId: "t1" }, ctx, deps())).toEqual({ params: { command: "ls -la" } });
  });

  it("a deny is a final block with tk's reason", async () => {
    expect(await beforeToolCall({ toolName: "web_fetch", params: { url: "https://x" } }, ctx, deps())).toEqual({ block: true, blockReason: "tk: rule web_fetch" });
    expect(await beforeToolCall({ toolName: "exec", params: { command: "git -c core.fsmonitor=x status" } }, ctx, deps())).toEqual({ block: true, blockReason: "tk: reject list: core.fsmonitor" });
  });

  it("a shell exec is gated as bash and gets the shell-text check (case 82); Code Mode's exec keeps its name", async () => {
    expect(toolForGate({ toolName: "exec" })).toBe("bash");
    expect(toolForGate({ toolName: "exec", toolKind: "code_mode_exec" })).toBe("exec");
    daemon.setApprove("none");
    const r = await beforeToolCall({ toolName: "exec", params: { command: "cat $HOME/x" } }, ctx, deps());
    expect(r).toMatchObject({ requireApproval: { title: "tk: allow bash?" } });
    // Code Mode has no rule: tk asks.
    expect(await beforeToolCall({ toolName: "exec", toolKind: "code_mode_exec", params: { code: "1" } }, ctx, deps())).toMatchObject({ requireApproval: {} });
  });

  it("MCP names <server>__<tool> with - in the server split right", async () => {
    daemon.setApprove("none");
    expect(await beforeToolCall({ toolName: "my-srv__do-it", params: {} }, ctx, deps())).toMatchObject({ requireApproval: { title: "tk: allow my_srv/do_it?" } });
    expect(await beforeToolCall({ toolName: "exa__search", params: { q: "x" } }, ctx, deps())).toEqual({ params: { q: "x" } });
  });

  it("an ask: the tk approver's yes allows; no approver asks OpenClaw; a no blocks", async () => {
    daemon.setApprove("yes");
    expect(await beforeToolCall({ toolName: "my-srv__x", params: { a: 1 } }, ctx, deps())).toEqual({ params: { a: 1 } });
    daemon.setApprove("none");
    expect(await beforeToolCall({ toolName: "my-srv__x", params: {} }, ctx, deps())).toMatchObject({ requireApproval: { severity: "warning", timeoutMs: 1000, allowedDecisions: ["allow-once", "deny"] } });
    daemon.setApprove("no");
    expect(await beforeToolCall({ toolName: "my-srv__x", params: {} }, ctx, deps())).toMatchObject({ block: true });
    daemon.setApprove("none");
    expect(daemon.approvals().at(-1)).toMatchObject({ harness: "openclaw", tool: "my_srv/x", chat: "ses-1" });
  });

  it("a secret in an MCP tool's input comes back redacted in the params", async () => {
    const r = await beforeToolCall({ toolName: "exa__search", params: { q: "x", api_key: "k3y" } }, ctx, deps());
    expect(r).toEqual({ params: { q: "x", api_key: REDACTED } });
  });

  it("OpenClaw's control files ask", async () => {
    daemon.setApprove("none");
    for (const command of ["cat /home/u/.openclaw/openclaw.json", "export OPENCLAW_CONFIG_PATH=/tmp/c.json", "echo x > openclaw.json"]) {
      expect(await beforeToolCall({ toolName: "exec", params: { command } }, ctx, deps()), command).toMatchObject({ requireApproval: { description: expect.stringContaining("control file") } });
    }
  });

  it("bad input and a broken policy load block", async () => {
    expect(await beforeToolCall({}, ctx, deps())).toMatchObject({ block: true });
    const broken = { ...deps(), loadPolicy: () => { throw new Error("boom"); } };
    expect(await beforeToolCall({ toolName: "read", params: {} }, ctx, broken)).toEqual({ block: true, blockReason: "tk: hook error: boom" });
  });

  it("records the decision under the tool call id", async () => {
    const n = daemon.events().length;
    await beforeToolCall({ toolName: "read", params: { path: "a" }, toolCallId: "t-42" }, ctx, deps());
    expect(daemon.events().slice(n)[0]).toMatchObject({ id: "openclaw-t-42-decision", harness: "openclaw", kind: "approval", chatId: "ses-1" });
  });
});

describe("after_tool_call", () => {
  it("records the call with a hash and never throws", async () => {
    const n = daemon.events().length;
    await afterToolCall({ toolName: "exec", result: { stdout: "ok" }, toolCallId: "t-43" }, ctx, deps());
    expect(daemon.events().slice(n)[0]).toMatchObject({ id: "openclaw-t-43", kind: "tool.call" });
    await expect(afterToolCall(undefined as never, undefined, deps())).resolves.toBeUndefined();
  });
});

describe("plugin files", () => {
  it("the entry registers before_tool_call at the lowest priority with a budget above the ask cap, and after_tool_call", () => {
    const seen: { hook: string; opts?: Record<string, unknown> }[] = [];
    entry.register({ on: (hook, _h, opts) => void seen.push({ hook, ...(opts ? { opts } : {}) }) });
    expect(seen.map((s) => s.hook)).toEqual(["before_tool_call", "after_tool_call"]);
    expect(seen[0]?.opts).toEqual({ priority: TK_PRIORITY, timeoutMs: 120_000 });
    expect(TK_PRIORITY).toBeLessThan(-1000);
  });

  it("the manifest and package.json have OpenClaw's required fields", () => {
    const m = JSON.parse(readFileSync(path.join(here, "..", "openclaw.plugin.json"), "utf8"));
    expect(m.id).toBe("tk");
    expect(m.configSchema).toMatchObject({ type: "object" });
    const pkg = JSON.parse(readFileSync(path.join(here, "..", "package.json"), "utf8"));
    expect(pkg.openclaw.extensions).toEqual(["./index.ts"]);
  });

  it("the core copy is byte-identical to the canonical one (sync-core.mjs does not list this package yet)", () => {
    const canonical = path.join(here, "..", "..", "tk-claude-plugin", "src", "core");
    const files = readdirSync(canonical).filter((f) => f.endsWith(".ts") && !f.endsWith(".test.ts")).sort();
    expect(readdirSync(path.join(here, "core")).filter((f) => f.endsWith(".ts")).sort()).toEqual(files);
    for (const f of files) expect(readFileSync(path.join(here, "core", f)).equals(readFileSync(path.join(canonical, f))), f).toBe(true);
  });
});
