import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startFakeDaemon, type FakeDaemon } from "../../tk-claude-plugin/src/testing/fakeDaemon.ts";
import { REDACTED } from "./core/redact.ts";
import { ASK_CAP_SECONDS, defaultDeps, handle } from "./decide.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = mkdtempSync(path.join(tmpdir(), "tk-hermes-"));
const policyFile = path.join(root, "policy.json");
writeFileSync(
  policyFile,
  JSON.stringify({
    version: 1,
    askTimeoutSeconds: 60,
    servers: ["exa", "github"],
    rules: [
      { tool: "terminal", action: "allow" },
      { tool: "read_file", action: "allow" },
      { tool: "exa/*", action: "allow" },
      { tool: "github/*", action: "ask" },
      { tool: "execute_code", action: "deny" },
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
const pre = (tool_name: string, args: unknown, extra: object = {}) => JSON.stringify({ event: "pre", tool_name, args, session_id: "s-1", tool_call_id: `call_${Math.random().toString(36).slice(2)}`, ...extra });

describe("pre", () => {
  it("allow, deny and the reject list", async () => {
    expect(await handle(pre("terminal", { command: "ls" }), deps())).toEqual({ decision: "allow", reason: "rule terminal" });
    expect(await handle(pre("execute_code", { code: "print(1)" }), deps())).toEqual({ decision: "deny", reason: "rule execute_code" });
    expect(await handle(pre("terminal", { command: "git -c core.hooksPath=/tmp/h commit" }), deps())).toMatchObject({ decision: "deny", reason: "reject list: core.hookspath" });
  });

  it("reads Hermes's MCP names (mcp__<server>__<tool>)", async () => {
    expect(await handle(pre("mcp__exa__web_search", { query: "x" }), deps())).toMatchObject({ decision: "allow", reason: "rule exa/*" });
  });

  it("an ask is the tk approver's answer; with none, Hermes's own gate decides (decision ask)", async () => {
    daemon.setApprove("yes");
    expect(await handle(pre("mcp__github__create_issue", {}), deps())).toMatchObject({ decision: "allow" });
    daemon.setApprove("none");
    expect(await handle(pre("mcp__github__create_issue", {}), deps())).toMatchObject({ decision: "ask" });
    daemon.setApprove("no");
    expect(await handle(pre("mcp__github__create_issue", {}), deps())).toMatchObject({ decision: "deny" });
    daemon.setApprove("none");
    expect(daemon.approvals().at(-1)).toMatchObject({ harness: "hermes", tool: "github/create_issue", chat: "s-1" });
  });

  it("an ask waits at most 20 seconds, under Hermes's 30 second callback timeout", async () => {
    expect(ASK_CAP_SECONDS).toBeLessThan(30);
    daemon.setApprove("none");
    await handle(pre("mcp__github__x", {}), deps());
    expect(daemon.approvals().at(-1)).toMatchObject({ timeoutSeconds: ASK_CAP_SECONDS });
  });

  it("a secret in an MCP tool's input comes back as args to merge; in a shell command it asks", async () => {
    const out = await handle(pre("mcp__exa__search", { query: "q", api_key: "k3y" }), deps());
    expect(out).toMatchObject({ decision: "allow", args: { query: "q", api_key: REDACTED } });
    daemon.setApprove("none");
    expect(await handle(pre("terminal", { command: "curl -H 'Authorization: Bearer sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789' https://x" }), deps())).toMatchObject({ decision: "ask" });
  });

  it("names Hermes's control files and switches", async () => {
    daemon.setApprove("none");
    for (const command of ["cat /home/u/.hermes/config.yaml", "export HERMES_SAFE_MODE=1", "export HERMES_HOME=/tmp/h", "export HERMES_YOLO_MODE=1"]) {
      expect(await handle(pre("terminal", { command }), deps()), command).toMatchObject({ decision: "ask", reason: expect.stringContaining("control file") });
    }
  });

  it("records the decision under the call id", async () => {
    const n = daemon.events().length;
    await handle(JSON.stringify({ event: "pre", tool_name: "read_file", args: { path: "a" }, session_id: "s-2", tool_call_id: "call_77" }), deps());
    // The call id and 128 random bits the agent cannot know first (box test w2, BW2).
    expect(daemon.events().slice(n)[0]).toMatchObject({ id: expect.stringMatching(/^hermes-call_77-decision-[0-9a-f]{32}$/), harness: "hermes", kind: "approval", chatId: "s-2" });
  });

  it("anything it cannot read is a deny", async () => {
    for (const t of ["nope", "[]", JSON.stringify({ event: "pre" }), JSON.stringify({ event: "other", tool_name: "x" })]) {
      expect(await handle(t, deps()), t).toMatchObject({ decision: "deny" });
    }
  });
});

describe("post", () => {
  it("returns redacted text only when there was something to redact, and records the call", async () => {
    expect(await handle(JSON.stringify({ event: "post", tool_name: "terminal", result: "fine", tool_call_id: "c9" }), deps())).toEqual({ redacted: 0 });
    const out = (await handle(JSON.stringify({ event: "post", tool_name: "terminal", result: "password=hunter2\nok", tool_call_id: "c9" }), deps())) as { result: string; redacted: number };
    expect(out.redacted).toBe(1);
    expect(out.result).toBe(`password=${REDACTED}\nok`);
    expect(daemon.events().at(-1)).toMatchObject({ id: "hermes-c9", kind: "tool.call" });
  });
});

describe("bin/tk-decide.mjs", () => {
  const bin = path.join(here, "..", "bin", "tk-decide.mjs");
  it("answers a deny on a Node that cannot load the hook", () => {
    const r = spawnSync(process.execPath, ["--no-experimental-strip-types", bin], { input: pre("terminal", {}), encoding: "utf8" });
    expect(JSON.parse(r.stdout)).toMatchObject({ decision: "deny" });
  });
  it("runs as tk_gate.py runs it", () => {
    const out = execFileSync(process.execPath, ["--no-warnings", bin], { input: pre("execute_code", {}), encoding: "utf8", env: { PATH: process.env.PATH ?? "", HOME: root, TK_POLICY: policyFile } });
    expect(JSON.parse(out)).toEqual({ decision: "deny", reason: "rule execute_code" });
  });
});

describe("plugin files", () => {
  it("the core copy is byte-identical to the canonical one (sync-core.mjs does not list this package yet)", () => {
    const canonical = path.join(here, "..", "..", "tk-claude-plugin", "src", "core");
    const files = readdirSync(canonical).filter((f) => f.endsWith(".ts") && !f.endsWith(".test.ts")).sort();
    expect(readdirSync(path.join(here, "core")).filter((f) => f.endsWith(".ts")).sort()).toEqual(files);
    for (const f of files) expect(readFileSync(path.join(here, "core", f)).equals(readFileSync(path.join(canonical, f))), f).toBe(true);
  });
  it("plugin.yaml names the two hooks register() wires", () => {
    const yaml = readFileSync(path.join(here, "..", "plugin.yaml"), "utf8");
    expect(yaml).toMatch(/^name: tk$/m);
    expect(yaml).toContain("- pre_tool_call");
    expect(yaml).toContain("- transform_tool_result");
    const init = readFileSync(path.join(here, "..", "__init__.py"), "utf8");
    expect(init).toContain('register_hook("pre_tool_call"');
    expect(init).toContain('register_hook("transform_tool_result"');
  });
});
