import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { TRUST_DIR } from "./identity.ts";
import type { Fetch } from "./osv.ts";
import { OSV_BATCH_URL, lookupEntries, osvToSarif, queriesFor } from "./osv.ts";
import { countFindings } from "./sarif.ts";

const data = { packages: [
  { registryType: "npm", identifier: "@shopify/dev-mcp", version: "1.17.0" },
  { registryType: "pypi", identifier: "mcp-server-fetch", version: "2025.4.7" },
  { registryType: "npm", identifier: "loose", version: "latest" },
  { registryType: "oci", identifier: "mcp/exa", version: "1" },
] };

describe("OSV lookups", () => {
  it("queries exact versions only and says what it skipped", () => {
    const { queries, skipped } = queriesFor(data);
    expect(queries).toEqual([
      { package: { name: "@shopify/dev-mcp", ecosystem: "npm" }, version: "1.17.0" },
      { package: { name: "mcp-server-fetch", ecosystem: "PyPI" }, version: "2025.4.7" },
    ]);
    expect(skipped).toEqual(["npm:loose (no exact version)", "oci:mcp/exa (no OSV ecosystem)"]);
  });

  it("posts one batch and maps ids back to each entry", async () => {
    const posts: { url: string; body: unknown }[] = [];
    const fetch: Fetch = async (url, init) => {
      posts.push({ url, body: JSON.parse(init.body) });
      return { ok: true, status: 200, json: async () => ({ results: [{}, { vulns: [{ id: "GHSA-xxxx-yyyy-zzzz", modified: "2026-01-01T00:00:00Z" }] }] }) };
    };
    const [r] = await lookupEntries([{ id: "fetch", version: "1.0.0", data }], { fetch, now: () => new Date("2026-10-28T18:30:00Z") });
    expect(posts).toHaveLength(1);
    expect(posts[0]!.url).toBe(OSV_BATCH_URL);
    expect(r!.vulns).toEqual([{ query: 1, id: "GHSA-xxxx-yyyy-zzzz", modified: "2026-01-01T00:00:00Z" }]);
    const sarif = osvToSarif(r!);
    expect(countFindings(sarif)).toBe(1);
    expect(sarif.runs[0]!.invocations![0]!.toolExecutionNotifications).toHaveLength(2);
  });

  it("fails loudly on an answer that does not match the batch", async () => {
    const fetch: Fetch = async () => ({ ok: true, status: 200, json: async () => ({ results: [] }) });
    await expect(lookupEntries([{ id: "x", version: "1", data }], { fetch })).rejects.toThrow(/does not match/);
    const down: Fetch = async () => ({ ok: false, status: 503, json: async () => ({}) });
    await expect(lookupEntries([{ id: "x", version: "1", data }], { fetch: down })).rejects.toThrow(/503/);
  });
});

describe("the command line", () => {
  const cli = join(TRUST_DIR, "cli.ts");
  const run = (args: string[], env: Record<string, string> = {}) => {
    try {
      return { code: 0, out: execFileSync(process.execPath, ["--no-warnings", cli, ...args], { encoding: "utf8", env: { PATH: process.env["PATH"] ?? "", ...env }, stdio: ["ignore", "pipe", "pipe"] }) };
    } catch (e) {
      const err = e as { status: number; stdout: string; stderr: string };
      return { code: err.status, out: err.stdout + err.stderr };
    }
  };

  it("finds no name errors in the seed catalog", () => {
    const r = run(["names"]);
    expect(r.code, r.out).toBe(0);
    expect(r.out).toMatch(/14 entries, 0 errors/);
  });

  it("records manifests, then the same again, into a scratch store", () => {
    const dir = mkdtempSync(join(tmpdir(), "tk-cli-"));
    try {
      const first = run(["record"], { TK_TRUST_RECORDS: dir });
      expect(first.code, first.out).toBe(0);
      expect(first.out.match(/^new /gm)).toHaveLength(8);
      const second = run(["record"], { TK_TRUST_RECORDS: dir });
      expect(second.out.match(/^same /gm)).toHaveLength(8);
      const pin = run(["pin", "exa", "--harness", "claude-code"], { TK_TRUST_RECORDS: dir });
      expect(pin.code).toBe(1);
      expect(pin.out).toMatch(/does not see a tool's description/);
      expect(run(["due"], { TK_TRUST_RECORDS: dir }).out).toMatch(/scan\(s\) due/);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("runs no scanner and makes no call without the switch", () => {
    expect(run(["scan"]).code).toBe(2);
    expect(run(["osv"]).code).toBe(2);
  });

  it("prints a card with nothing called safe", () => {
    const r = run(["card", "exa"]);
    expect(r.code, r.out).toBe(0);
    expect(r.out).toMatch(/not scanned/);
    expect(r.out).not.toMatch(/\bsafe\b/i);
  });
});
