// Checks that every contract fixture validates against its schema, so a lane
// that builds against a fixture builds against a valid one. Session 0.
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { Ajv2020 } from "ajv/dist/2020.js";

const here = dirname(fileURLToPath(import.meta.url));
const read = (p: string): unknown => JSON.parse(readFileSync(join(here, p), "utf8"));

function validator() {
  const ajv = new Ajv2020({ allErrors: true, strict: true, validateFormats: false });
  for (const p of [
    "entry.schema.json",
    "policy.schema.json",
    "shapes/server.min.schema.json",
    "shapes/skill.schema.json",
    "shapes/plugin.schema.json",
    "shapes/marketplace.schema.json",
  ]) {
    ajv.addSchema(read(p) as object);
  }
  return ajv;
}

const base = "https://github.com/aetiusgular/tk/contracts/";

function check(schemaId: string, data: unknown): string[] {
  const ajv = validator();
  const validate = ajv.getSchema(base + schemaId);
  if (!validate) throw new Error(`no schema ${schemaId}`);
  return validate(data) ? [] : (validate.errors ?? []).map((e: { instancePath: string; message?: string }) => `${e.instancePath} ${e.message ?? ""}`);
}

describe("policy contract", () => {
  it("the fixture policy validates", () => {
    expect(check("policy.schema.json", read("fixtures/policy/policy.json"))).toEqual([]);
  });

  it("the pinned description hash is sha256 of the text case 16 sends", () => {
    const policy = read("fixtures/policy/policy.json") as { rules: { descriptionSha256?: string }[] };
    const pin = policy.rules.find((r) => r.descriptionSha256)?.descriptionSha256;
    expect(pin).toBe(createHash("sha256").update("Write a file.").digest("hex"));
  });

  it("has at least twenty cases with unique ids and a known action", () => {
    const { cases } = read("fixtures/policy/cases.json") as { cases: { id: number; expect: { action: string } }[] };
    expect(cases.length).toBeGreaterThanOrEqual(20);
    expect(new Set(cases.map((c) => c.id)).size).toBe(cases.length);
    for (const c of cases) expect(["allow", "ask", "deny"]).toContain(c.expect.action);
  });

  it("refuses a rule with an unknown field and an unnormalized tool", () => {
    expect(check("policy.schema.json", { version: 1, rules: [{ tool: "exa/x", action: "allow", widen: true }] })).not.toEqual([]);
    expect(check("policy.schema.json", { version: 1, rules: [{ tool: "mcp__Exa__X", action: "allow" }] })).not.toEqual([]);
  });
});

describe("entry contract", () => {
  it("the server fixture validates as server.json with tk's _meta", () => {
    expect(check("shapes/server.min.schema.json", read("fixtures/entry/server.json"))).toEqual([]);
  });

  it("the skill fixture validates", () => {
    expect(check("shapes/skill.schema.json", read("fixtures/entry/skill/skill.json"))).toEqual([]);
  });

  it("the bundle fixture validates as plugin.json", () => {
    expect(check("shapes/plugin.schema.json", read("fixtures/entry/bundle/.claude-plugin/plugin.json"))).toEqual([]);
  });

  it("the marketplace fixture validates and pins a full sha", () => {
    expect(check("shapes/marketplace.schema.json", read("fixtures/entry/marketplace/.claude-plugin/marketplace.json"))).toEqual([]);
  });

  it("refuses a host runner that does not say it runs as you", () => {
    const entry = { kind: "server", tier: "own", status: "active", runner: { kind: "host" }, exposure: { default: "direct" },
      trust: { contentSha256: "0".repeat(64), scans: [], liveness: { state: "unknown" } } };
    expect(check("entry.schema.json", entry)).not.toEqual([]);
  });

  it("refuses an unpinned github source in a marketplace export", () => {
    const m = { name: "x", owner: { name: "y" }, plugins: [{ name: "p", source: { source: "github", repo: "a/b" } }] };
    expect(check("shapes/marketplace.schema.json", m)).not.toEqual([]);
  });
});

describe("harness presets", () => {
  const presets = read("harness-presets.json") as { harnesses: { id: string; verified: string; sources: string[] }[] };

  it("lists the fourteen harnesses of note AP section 4 with unique ids", () => {
    const ids = presets.harnesses.map((h) => h.id);
    expect(ids).toHaveLength(14);
    expect(new Set(ids).size).toBe(14);
    for (const id of ["claude-code", "codex", "pi", "opencode", "gemini"]) expect(ids).toContain(id);
  });

  it("every harness id fits the policy file's harness pattern and names a source", () => {
    for (const h of presets.harnesses) {
      expect(h.id).toMatch(/^[a-z0-9][a-z0-9-]{0,63}$/);
      expect(h.sources.length).toBeGreaterThan(0);
    }
  });
});

describe("daemon exchange", () => {
  it("every non-comment line has a direction marker and parses as JSON", () => {
    const text = readFileSync(join(here, "fixtures/daemon/exchange.jsonl"), "utf8");
    const lines = text.split("\n").filter((l) => l && !l.startsWith("#"));
    expect(lines.length).toBeGreaterThan(10);
    for (const l of lines) {
      expect(["> ", "< "]).toContain(l.slice(0, 2));
      expect(() => JSON.parse(l.slice(2))).not.toThrow();
    }
  });
});

// Integration w1: the repository is its own marketplace for tk's plugins, so
// `claude plugin install tk@tk` and `codex plugin add tk@tk` work from a
// clone (run with Claude Code 2.1.311 and Codex 0.163.0 on 27 Oct; Claude
// Code's `plugin validate` passed). The id must stay tk@tk: the managed
// settings fragment and Codex's hook trust keys name it.
describe("tk's own marketplaces", () => {
  const root = join(here, "..");
  const claude = JSON.parse(readFileSync(join(root, ".claude-plugin", "marketplace.json"), "utf8")) as { name: string; plugins: { name: string; source: string }[] };
  const codex = JSON.parse(readFileSync(join(root, ".agents", "plugins", "marketplace.json"), "utf8")) as { name: string; plugins: { name: string; source: { source: string; path: string } }[] };
  it("Claude Code's names tk and points at the Claude Code plugin", () => {
    expect(check("shapes/marketplace.schema.json", claude)).toEqual([]);
    expect(claude.name).toBe("tk");
    expect(claude.plugins).toEqual([expect.objectContaining({ name: "tk", source: "./packages/tk-claude-plugin" })]);
    const manifest = JSON.parse(readFileSync(join(root, "packages", "tk-claude-plugin", ".claude-plugin", "plugin.json"), "utf8")) as { name: string };
    expect(manifest.name).toBe("tk");
  });
  it("Codex's names tk and points at the Codex plugin", () => {
    expect(codex.name).toBe("tk");
    expect(codex.plugins).toEqual([{ name: "tk", source: { source: "local", path: "./packages/tk-codex-plugin" } }]);
    const manifest = JSON.parse(readFileSync(join(root, "packages", "tk-codex-plugin", ".codex-plugin", "plugin.json"), "utf8")) as { name: string };
    expect(manifest.name).toBe("tk");
  });
  it("the managed settings fragment enables exactly tk@tk", () => {
    const frag = readFileSync(join(root, "packages", "tk-claude-plugin", "managed-settings.fragment.json"), "utf8");
    expect(frag).toContain('"tk@tk"');
  });
});
