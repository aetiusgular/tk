import { spawn } from "node:child_process";
import { generateKeyPairSync } from "node:crypto";
import { cpSync, mkdirSync, mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const CLI = join(import.meta.dirname, "cli.ts");

function run(args: string[], opts: { stdin?: string; env?: Record<string, string> } = {}): Promise<{ code: number | null; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const p = spawn(process.execPath, ["--no-warnings", CLI, ...args], { stdio: ["pipe", "pipe", "pipe"], env: { ...process.env, ...(opts.env ?? {}) } });
    let stdout = "";
    let stderr = "";
    p.stdout.on("data", (c) => (stdout += c));
    p.stderr.on("data", (c) => (stderr += c));
    p.on("close", (code) => resolve({ code, stdout, stderr }));
    p.stdin.end(opts.stdin ?? "");
  });
}

describe("publish command line", () => {
  it("prints a key's public record and nothing of the private key", async () => {
    const pem = generateKeyPairSync("ed25519").privateKey.export({ format: "pem", type: "pkcs8" }).toString();
    const r = await run(["record", "--key-stdin"], { stdin: pem });
    expect(r.code).toBe(0);
    expect(r.stdout).toMatch(/^v=MCPv1; k=ed25519; p=[A-Za-z0-9+/=]{44}\n$/);
    const body = pem.split("\n").filter((l) => l && !l.startsWith("-----")).join("");
    expect(r.stdout + r.stderr).not.toContain(body.slice(10, 30));
  });

  it("warns on a key file others can read", async () => {
    const dir = mkdtempSync(join(tmpdir(), "tk-pubcli-"));
    const file = join(dir, "key.pem");
    writeFileSync(file, generateKeyPairSync("ed25519").privateKey.export({ format: "pem", type: "pkcs8" }).toString());
    chmodSync(file, 0o644);
    const r = await run(["record", "--key-file", file]);
    expect(r.code).toBe(0);
    expect(r.stderr).toMatch(/chmod 600/);
  });

  it("refuses a key on argv and bad arguments with exit 2", async () => {
    expect((await run(["publish", "x", "--library", "y", "--proof", "dns", "--private-key", "abc"])).code).toBe(2);
    expect((await run(["publish", "x", "--library", "y", "--proof", "maybe"])).code).toBe(2);
    expect((await run(["verify", "--pr", "a", "--base", "b", "--author", "$(id)", "x"])).code).toBe(2);
  });

  it("stops with exit 1 and prints the steps of no pull request when gh is not there", async () => {
    const root = mkdtempSync(join(tmpdir(), "tk-pubcli-"));
    mkdirSync(join(root, "lib", "library"), { recursive: true });
    cpSync(join(import.meta.dirname, "..", "catalog"), join(root, "lib", "library", "catalog"), { recursive: true });
    const dir = join(root, "weather-tool");
    mkdirSync(dir);
    writeFileSync(
      join(dir, "server.json"),
      JSON.stringify({
        $schema: "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
        name: "io.github.alice/weather-tool",
        description: "Current weather and forecasts for a place, through a hosted endpoint.",
        version: "1.0.0",
        remotes: [{ type: "streamable-http", url: "https://weather.example.com/mcp" }],
        _meta: { "io.modelcontextprotocol.registry/publisher-provided": { "io.github.aetiusgular.tk": { kind: "server", tier: "curated", status: "active", runner: { kind: "remote" }, exposure: { default: "deferred" }, trust: { contentSha256: "0".repeat(64), scans: [], liveness: { state: "unknown" } } } } },
      }),
    );
    const r = await run(["publish", dir, "--library", join(root, "lib"), "--proof", "github"], { env: { PATH: "/nonexistent" } });
    expect(r.code).toBe(1);
    expect(r.stdout).toMatch(/\[publish\/namespace\] gh is not logged in/);
    expect(r.stdout).not.toMatch(/gh pr create/);
  }, 60_000);
});
