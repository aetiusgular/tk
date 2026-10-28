import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { FakeRegistry, remoteServer } from "./fake-registry.ts";

// The command line against a real HTTP server on 127.0.0.1. Spawned
// asynchronously: a spawnSync would freeze the server in this process
// (integration w1, Look out for).
function run(args: string[]): Promise<{ code: number | null; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const p = spawn(process.execPath, ["--no-warnings", join(import.meta.dirname, "cli.ts"), ...args], { stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    p.stdout.on("data", (c) => (stdout += c));
    p.stderr.on("data", (c) => (stderr += c));
    p.on("close", (code) => resolve({ code, stdout, stderr }));
  });
}

describe("mirror command line", () => {
  it("pulls a fake registry into a folder and reports what it did", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.tt/one", "1.0.0", "https://one.example.com/mcp"));
    reg.publish(remoteServer("io.github.tt/two", "2.0.0", "https://two.example.com/mcp"));
    const { server, baseUrl } = await reg.listen();
    try {
      const out = mkdtempSync(join(tmpdir(), "tk-mirror-cli-"));
      const r = await run(["pull", "--out", out, "--base-url", baseUrl, "--no-probe"]);
      expect(r.code, r.stderr).toBe(0);
      const report = JSON.parse(r.stdout);
      expect(report).toMatchObject({ mode: "full", received: 2, new: 2, servers: 2, probed: 0 });
      expect(existsSync(join(out, "entries", "io.github.tt%2Fone.json"))).toBe(true);
      expect(JSON.parse(readFileSync(join(out, "state.json"), "utf8")).source).toBe(baseUrl);
    } finally {
      server.close();
    }
  }, 30_000);

  it("exits 2 on a bad argument and 1 on an unreachable registry without moving state", async () => {
    expect((await run(["pull"])).code).toBe(2);
    expect((await run(["pull", "--out", "x", "--cap", "-3"])).code).toBe(2);
    const out = mkdtempSync(join(tmpdir(), "tk-mirror-cli-"));
    const r = await run(["pull", "--out", out, "--base-url", "http://127.0.0.1:9", "--no-probe", "--max-pages", "1", "--attempts", "1"]);
    expect(r.code).toBe(1);
    expect(existsSync(join(out, "state.json"))).toBe(false);
  }, 120_000);
});
