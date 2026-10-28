import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { startFakeDaemon, type FakeDaemon } from "../../tk-claude-plugin/src/testing/fakeDaemon.ts";
import { REDACTED } from "./core/redact.ts";
import { TkBlocked, after, before, defaultDeps } from "./kilo.ts";
import * as entry from "./plugin.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = mkdtempSync(path.join(tmpdir(), "tk-kilo-"));
const policyFile = path.join(root, "policy.json");
writeFileSync(
  policyFile,
  JSON.stringify({
    version: 1,
    askTimeoutSeconds: 1,
    servers: ["exa", "github", "my-server"],
    rules: [
      { tool: "bash", action: "allow" },
      { tool: "read", action: "allow" },
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

const deps = (extra: Record<string, string> = {}) => defaultDeps({ TK_POLICY: policyFile, ...daemon.env(), ...extra });
const call = (tool: string) => ({ tool, sessionID: "ses_1", callID: `call_${Math.random().toString(36).slice(2)}` });

describe("tool.execute.before", () => {
  it("lets an allowed call through and records it", async () => {
    const before0 = daemon.events().length;
    await expect(before(call("bash"), { args: { command: "ls" } }, deps())).resolves.toBeUndefined();
    expect(daemon.events().slice(before0)[0]).toMatchObject({ harness: "kilo", kind: "approval", detail: { decision: "allow", tool: "bash" } });
  });
  it("throws to block a denied call, and on the reject list", async () => {
    await expect(before(call("webfetch"), { args: { url: "https://x" } }, deps())).rejects.toBeInstanceOf(TkBlocked);
    await expect(before(call("bash"), { args: { command: "export CHATGPT_BASE_URL=http://x" } }, deps())).rejects.toThrow("tk: reject list: chatgpt_base_url");
  });
  it("an ask is the approver's yes, else a throw (contract section 5)", async () => {
    daemon.setApprove("yes");
    await expect(before(call("github_create_issue"), { args: {} }, deps())).resolves.toBeUndefined();
    expect(daemon.approvals().at(-1)).toMatchObject({ harness: "kilo", tool: "github/create_issue", chat: "ses_1" });
    for (const b of ["no", "none", "timeout", "close"] as const) {
      daemon.setApprove(b);
      await expect(before(call("github_create_issue"), { args: {} }, deps()), b).rejects.toBeInstanceOf(TkBlocked);
    }
    await expect(before(call("github_create_issue"), { args: {} }, defaultDeps({ TK_POLICY: policyFile, TK_DAEMON_SOCKET: path.join(root, "absent.sock") }))).rejects.toThrow("not reachable");
  });
  it("redacts in place, so the tool runs with the redacted object", async () => {
    const args = { query: "q", api_key: "abc", nested: { url: "https://h/?token=zz9" } };
    const output = { args };
    await before(call("exa_search"), output, deps());
    expect(output.args).toBe(args);
    expect(args).toEqual({ query: "q", api_key: REDACTED, nested: { url: `https://h/?token=${REDACTED}` } });
  });
  it("fails closed on bad input", async () => {
    await expect(before({} as never, { args: {} }, deps())).rejects.toBeInstanceOf(TkBlocked);
    // A shell call with no args serializes to "{}", which the matcher cannot read: ask, then deny.
    daemon.setApprove("none");
    await expect(before(call("bash"), undefined as never, deps())).rejects.toBeInstanceOf(TkBlocked);
    const broken = { ...deps(), loadPolicy: () => { throw new Error("boom"); } };
    await expect(before(call("bash"), { args: {} }, broken)).rejects.toThrow("tk: hook error: boom");
  });
});

describe("Kilo's control files", () => {
  it("an allowed call that names Kilo's config or plugin folders asks, so it is denied with no approver", async () => {
    daemon.setApprove("none");
    for (const command of ["cp evil.ts /home/u/.config/kilo/plugin/", "echo x > kilo.json", "mkdir -p .kilo/plugin", "export KILO_PURE=1", "cat .kilocode/plugin/x.ts"]) {
      await expect(before(call("bash"), { args: { command } }, deps()), command).rejects.toThrow("control file");
    }
    daemon.setApprove("yes");
    await expect(before(call("bash"), { args: { command: "cat kilo.json" } }, deps())).resolves.toBeUndefined();
    daemon.setApprove("none");
  });
});

describe("tool.execute.after", () => {
  it("redacts a built-in tool's output and metadata in place", async () => {
    const out = { title: "bash", output: "TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789", metadata: { output: "password=hunter2", exit: 0 } };
    await after({ ...call("bash"), args: {} }, out, deps());
    expect(out.output).toBe(`TOKEN=${REDACTED}`);
    expect(out.metadata).toEqual({ output: `password=${REDACTED}`, exit: 0 });
  });
  it("redacts an MCP result's text content in place", async () => {
    const content = [{ type: "text", text: "secret: s3cr3t" }];
    const out = { content };
    await after({ ...call("exa_search"), args: {} }, out, deps());
    expect(out.content).toBe(content);
    expect(content[0]?.text).toBe(`secret: ${REDACTED}`);
  });
  it("never throws", async () => {
    await expect(after(undefined as never, null, deps())).resolves.toBeUndefined();
  });
});

describe("plugin files", () => {
  it("the entry exports only the v1 default {id, server}, and server returns the two hooks", async () => {
    expect(Object.keys(entry)).toEqual(["default"]);
    expect(entry.default.id).toBe("tk");
    const hooks = await entry.default.server();
    expect(Object.keys(hooks).sort()).toEqual(["tool.execute.after", "tool.execute.before"]);
  });
  it("MCP names keep `-` in Kilo (sanitize keeps [a-zA-Z0-9_-]); the server still splits", async () => {
    const policy = path.join(root, "dash.json");
    writeFileSync(policy, JSON.stringify({ version: 1, servers: ["my-server"], rules: [{ tool: "my_server/*", action: "deny" }] }));
    await expect(before(call("my-server_do-it"), { args: {} }, defaultDeps({ TK_POLICY: policy, TK_DAEMON_SOCKET: path.join(root, "absent.sock") }))).rejects.toThrow("tk: rule my_server/*");
  });
  it("the package has no engines.opencode and imports no plugin SDK (Kilo would skip or install it)", () => {
    const pkg = JSON.parse(readFileSync(path.join(here, "..", "package.json"), "utf8"));
    expect(pkg.engines?.opencode).toBeUndefined();
    for (const f of ["plugin.ts", "kilo.ts", "control.ts"]) expect(readFileSync(path.join(here, f), "utf8")).not.toMatch(/from "@(opencode-ai|kilocode)\//);
  });
  it("the core copy is byte-identical to the canonical one", () => {
    const script = path.join(here, "..", "..", "tk-claude-plugin", "scripts", "sync-core.mjs");
    expect(() => execFileSync(process.execPath, [script, "--check"], { encoding: "utf8" })).not.toThrow();
  });
});
