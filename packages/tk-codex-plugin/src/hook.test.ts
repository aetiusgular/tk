import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startFakeDaemon, type FakeDaemon } from "../../tk-claude-plugin/src/testing/fakeDaemon.ts";
import { REDACTED } from "./core/redact.ts";
import { decide } from "./decide.ts";
import { runHook } from "./hook.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = mkdtempSync(path.join(tmpdir(), "tk-codex-"));
const policyFile = path.join(root, "policy.json");
writeFileSync(
  policyFile,
  JSON.stringify({
    version: 1,
    askTimeoutSeconds: 1,
    servers: ["exa", "github"],
    rules: [
      { tool: "bash", action: "allow" },
      { tool: "bash", harness: "codex", chat: "chat-ask", action: "ask" },
      { tool: "apply_patch", action: "allow" },
      { tool: "update_plan", action: "allow" },
      { tool: "exa/*", action: "allow" },
      { tool: "github/*", action: "ask" },
      { tool: "fs/*", action: "deny" },
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
// The PreToolUse input shape of codex-rs/hooks/src/events/pre_tool_use.rs:176-189 at 0.163.0.
const pre = (tool: string, toolInput: unknown, extra: Record<string, unknown> = {}) =>
  JSON.stringify({ session_id: "s-1", turn_id: "t-1", transcript_path: null, cwd: "/w", hook_event_name: "PreToolUse", model: "m", permission_mode: "default", tool_name: tool, tool_input: toolInput, tool_use_id: `call_${Math.random().toString(36).slice(2)}`, ...extra });

describe("Codex PreToolUse", () => {
  it("allow says nothing, so Codex's own approvals still run", async () => {
    expect(await runHook("pre", pre("Bash", { command: "ls" }), env())).toEqual({ stdout: "", stderr: "", exitCode: 0 });
  });

  it("deny is exit 2 with a non-empty one-line reason on stderr", async () => {
    const r = await runHook("pre", pre("mcp__fs__write_file", { path: "a" }), env());
    expect(r.exitCode).toBe(2);
    expect(r.stderr).toMatch(/^tk: rule fs\/\*\n$/);
    const g = await runHook("pre", pre("Bash", { command: "git -c core.sshCommand=x fetch" }), env());
    expect(g.stderr).toBe("tk: reject list: core.sshcommand\n");
    const p = await runHook("pre", pre("apply_patch", { command: "*** Begin Patch\n*** Update File: .codex/config.toml\n+approval_policy = \"never\"\n*** End Patch" }), env());
    expect(p.stderr).toBe("tk: reject list: approval_policy\n");
  });

  it("never emits ask: an ask is the approver's yes, else deny (contract section 5)", async () => {
    for (const [b, want] of [["yes", 0], ["no", 2], ["none", 2], ["timeout", 2], ["refuse", 2], ["close", 2]] as const) {
      daemon.setApprove(b);
      const r = await runHook("pre", pre("mcp__github__create_issue", { title: "x" }), env());
      expect(r.exitCode, b).toBe(want);
      expect(r.stdout).not.toContain('"ask"');
    }
    expect(daemon.approvals().at(-1)).toMatchObject({ harness: "codex", tool: "github/create_issue" });
    const none = await runHook("pre", pre("mcp__github__create_issue", {}), { TK_POLICY: policyFile, TK_DAEMON_SOCKET: path.join(root, "absent.sock") });
    expect(none.exitCode).toBe(2);
    expect(none.stderr).toContain("daemon is not reachable");
  });

  it("the fixture's codex-scoped bash rule asks, and so denies with no approver (fixture case 9)", async () => {
    daemon.setApprove("none");
    const r = await runHook("pre", pre("Bash", { command: "ls" }), env({ TK_CHAT_ID: "chat-ask" }));
    expect(r.exitCode).toBe(2);
    expect(r.stderr).toContain("none is connected");
  });

  it("rewrites MCP input with redacted values, and denies other tools it cannot rewrite", async () => {
    const r = await runHook("pre", pre("mcp__exa__search", { query: "q", apiKey: "k-123" }), env());
    expect(r.exitCode).toBe(0);
    const out = JSON.parse(r.stdout).hookSpecificOutput;
    expect(out).toMatchObject({ hookEventName: "PreToolUse", permissionDecision: "allow", updatedInput: { query: "q", apiKey: REDACTED } });
    const u = await runHook("pre", pre("update_plan", { plan: [{ step: "use password=hunter2" }] }), env());
    expect(u.exitCode).toBe(2);
  });

  it("a secret in a shell command is an ask, so a deny on Codex", async () => {
    daemon.setApprove("none");
    expect((await runHook("pre", pre("Bash", { command: "curl https://h/?api_key=abc" }), env())).exitCode).toBe(2);
  });

  it("fails closed on every input it cannot read", async () => {
    for (const bad of ["", "{", "[]", "1", JSON.stringify({ hook_event_name: "PreToolUse" }), JSON.stringify({ hook_event_name: "PostToolUse", tool_name: "Bash" })]) {
      const r = await runHook("pre", bad, env());
      expect(r.exitCode, bad).toBe(2);
      expect(r.stderr.trim().length).toBeGreaterThan(4);
    }
  });
});

describe("Codex PostToolUse", () => {
  it("records the call; replaces output that holds secrets through decision block", async () => {
    const before = daemon.events().length;
    expect(await runHook("post", JSON.stringify({ hook_event_name: "PostToolUse", tool_name: "Bash", tool_use_id: "call_1", tool_input: { command: "ls" }, tool_response: "a\nb" }), env())).toEqual({ stdout: "", stderr: "", exitCode: 0 });
    expect(daemon.events().slice(before)[0]).toMatchObject({ id: "codex-call_1", harness: "codex", kind: "tool.call" });
    const r = await runHook("post", JSON.stringify({ hook_event_name: "PostToolUse", tool_name: "Bash", tool_use_id: "call_2", tool_response: "export AWS=AKIAABCDEFGHIJKLMNOP\nok" }), env());
    const out = JSON.parse(r.stdout);
    expect(out.decision).toBe("block");
    expect(out.reason).toContain(`export AWS=${REDACTED}\nok`);
    expect(out.reason).not.toContain("AKIAABCDEFGHIJKLMNOP");
  });
  it("never blocks on garbage", async () => {
    expect((await runHook("post", "{", env())).exitCode).toBe(0);
  });
});

describe("plugin files", () => {
  it("decide denies anything that is not a tool call", () => {
    expect(decide({}).action).toBe("deny");
    expect(decide(null).action).toBe("deny");
  });
  it("the core copy is byte-identical to the canonical one", () => {
    const script = path.join(here, "..", "..", "tk-claude-plugin", "scripts", "sync-core.mjs");
    expect(() => execFileSync(process.execPath, [script, "--check"], { encoding: "utf8" })).not.toThrow();
  });
  it("the launcher runs the hook", () => {
    const r = spawnSync(process.execPath, ["--no-warnings", path.join(here, "..", "bin", "tk-hook.mjs"), "pre"], { input: pre("mcp__fs__x", {}), env: { PATH: process.env.PATH ?? "", ...env() }, encoding: "utf8" });
    expect(r.status).toBe(2);
    expect(r.stderr).toBe("tk: rule fs/*\n");
  });
});
