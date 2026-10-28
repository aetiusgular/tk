// The policy file on a real file system (integration w1 security finding
// S11): the gate reads it through the production `realFs`, so these cases
// fail on the stat-then-read-by-path code they replace.
import { execFileSync } from "node:child_process";
import { chmodSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { createGate } from "../extensions/gate.ts";
import type { ToolCallResult } from "../src/pi-api.ts";
import { FakeRpcClient, fakePi } from "./fake-pi.ts";

const root = mkdtempSync(path.join(tmpdir(), "tk-pi-policy-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const allowRead = JSON.stringify({ version: 1, askTimeoutSeconds: 1, rules: [{ tool: "read", action: "allow" }] });

function gateWith(policyPath: string) {
  const client = new FakeRpcClient(() => ({ confirmed: false }));
  const pi = fakePi({ client });
  createGate({ env: () => ({ TK_POLICY: policyPath }), uid: process.getuid?.() ?? 0 })(pi.api);
  const call = () => pi.emit("tool_call", { type: "tool_call", toolName: "read", toolCallId: "c1", input: { path: "a.txt" } }) as Promise<ToolCallResult | undefined>;
  return { call, client };
}

describe("the policy file on disk", () => {
  it("a regular private file is read and applied", async () => {
    const file = path.join(root, "policy.json");
    writeFileSync(file, allowRead, { mode: 0o600 });
    chmodSync(file, 0o600);
    expect(await gateWith(file).call()).toBeUndefined();
  });

  it("a symbolic link is refused, even to a good file: the target's allow does not apply", async () => {
    const target = path.join(root, "target.json");
    writeFileSync(target, allowRead, { mode: 0o600 });
    chmodSync(target, 0o600);
    const link = path.join(root, "link.json");
    symlinkSync(target, link);
    const g = gateWith(link);
    expect(await g.call()).toMatchObject({ block: true });
    expect(g.client.requests[0]?.message).toContain("policy file is a symbolic link");
  });

  it("a FIFO at the path neither hangs the turn nor loads", async () => {
    const fifo = path.join(root, "policy.fifo");
    execFileSync("mkfifo", [fifo]);
    const g = gateWith(fifo);
    const started = Date.now();
    expect(await g.call()).toMatchObject({ block: true });
    expect(Date.now() - started).toBeLessThan(2000);
    expect(g.client.requests[0]?.message).toContain("policy path is not a file");
  });

  it("a file others can write is refused on the handle that would be read", async () => {
    const file = path.join(root, "open.json");
    writeFileSync(file, allowRead);
    chmodSync(file, 0o666);
    const g = gateWith(file);
    expect(await g.call()).toMatchObject({ block: true });
    expect(g.client.requests[0]?.message).toContain("writable by group or others");
  });
});
