import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Ajv2020 } from "ajv/dist/2020.js";
import { afterAll, describe, expect, it } from "vitest";
import { LIBRARY_DIR } from "./identity.ts";
import { cardLines, dueScans, latestScans, PLACEHOLDER_SHA } from "./card.ts";
import type { TrustRecord } from "./card.ts";
import { RecordStore } from "./records.ts";
import type { Exec } from "./scanners.ts";
import { ciscoToSarif, isoSeconds, loadPinned, parseSkillSpectorVersion, scanServerTools, scanSkill, scannerEnv } from "./scanners.ts";
import { countFindings, levelCounts, parseSarif } from "./sarif.ts";

const entrySchema = JSON.parse(readFileSync(join(LIBRARY_DIR, "..", "contracts", "entry.schema.json"), "utf8"));
const ajv = new Ajv2020({ strict: true, allErrors: true });
ajv.addSchema(entrySchema);
const validTrust = ajv.compile({ $ref: `${entrySchema.$id}#/$defs/trust` });

const work = mkdtempSync(join(tmpdir(), "tk-scanners-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));
const skillDir = join(work, "web-search");
mkdirSync(skillDir);
writeFileSync(join(skillDir, "SKILL.md"), "---\nname: web-search\ndescription: x\n---\nbody\n");

// The structure SkillSpector 2.13.2 writes (nodes/report.py:381-439, 693-745,
// 953-956 at abd8293): driver name and version, rules, levels from severity,
// a baseline-suppressed result with an external suppression.
const skillspectorSarif = (version = "2.13.2") =>
  JSON.stringify({
    $schema: "https://schemastore.azurewebsites.net/schemas/json/sarif-2.1.0-rtm.4.json",
    version: "2.1.0",
    runs: [
      {
        tool: { driver: { name: "skillspector", version, rules: [{ id: "P1", shortDescription: { text: "prompt injection" } }, { id: "SC4", shortDescription: { text: "vulnerable dependency" } }] } },
        columnKind: "unicodeCodePoints",
        results: [
          { ruleId: "P1", level: "error", message: { text: "instruction override" }, locations: [{ physicalLocation: { artifactLocation: { uri: "SKILL.md" }, region: { startLine: 12 } } }], properties: { severity: "HIGH" } },
          { ruleId: "SC4", level: "note", message: { text: "lodash 4.17.20" }, properties: { severity: "LOW" } },
          { ruleId: "P1", level: "warning", message: { text: "baseline" }, suppressions: [{ kind: "external", justification: "shipped baseline" }] },
        ],
        invocations: [{ executionSuccessful: true, properties: { analysisCompleteness: {} } }],
      },
    ],
  });

type Call = { file: string; args: string[]; env: Record<string, string> };
function fakeExec(answers: Record<string, { code: number; stdout?: string; stderr?: string; write?: string }>, calls: Call[]): Exec {
  return async (file, args, opts) => {
    calls.push({ file, args, env: opts.env });
    const key = args[0] === "--version" ? "version" : "scan";
    const a = answers[key] ?? { code: 2 };
    const out = args.indexOf("--output");
    if (a.write !== undefined && out >= 0) writeFileSync(args[out + 1]!, a.write);
    return { code: a.code, stdout: a.stdout ?? "", stderr: a.stderr ?? "", timedOut: false };
  };
}
const now = () => new Date("2026-10-28T18:30:00.123Z");

describe("SARIF counting", () => {
  it("counts unsuppressed failures by level", () => {
    const log = parseSarif(skillspectorSarif());
    if (typeof log === "string") throw new Error(log);
    expect(countFindings(log)).toBe(2);
    expect(levelCounts(log)).toEqual({ error: 1, warning: 0, note: 1, none: 0, suppressed: 1 });
  });

  it("does not count passes, informational results, or suppressions in force", () => {
    const log = parseSarif(JSON.stringify({ version: "2.1.0", runs: [{ tool: { driver: { name: "x" } }, results: [{ kind: "pass" }, { kind: "informational" }, { kind: "review" }, {}, { suppressions: [{ status: "accepted" }] }, { suppressions: [{ status: "rejected" }] }, { suppressions: [{ status: "underReview" }] }] }] }));
    if (typeof log === "string") throw new Error(log);
    expect(countFindings(log)).toBe(3);
  });

  it("counts a result whose suppressions disagree (fails safe where SARIF is silent)", () => {
    const log = parseSarif(JSON.stringify({ version: "2.1.0", runs: [{ tool: { driver: { name: "x" } }, results: [{ suppressions: [{ kind: "external" }, { kind: "inSource", status: "rejected" }] }, { suppressions: [] }, { suppressions: [{ kind: "external" }] }] }] }));
    if (typeof log === "string") throw new Error(log);
    expect(countFindings(log)).toBe(2);
  });

  it("refuses what is not a 2.1.0 log", () => {
    expect(parseSarif("nope")).toBe("not JSON");
    expect(parseSarif('{"version":"2.0.0","runs":[]}')).toMatch(/not 2.1.0/);
    expect(parseSarif('{"version":"2.1.0","runs":[{"tool":{}}]}')).toMatch(/driver.name/);
  });
});

describe("SkillSpector on a skill folder", () => {
  it("runs the pinned version by argument list with no LLM and records a dated count", async () => {
    const calls: Call[] = [];
    const exec = fakeExec({ version: { code: 0, stdout: "SkillSpector v2.13.2\n" }, scan: { code: 1, write: skillspectorSarif() } }, calls);
    const out = await scanSkill(skillDir, { exec, now, env: { PATH: "/usr/bin", HOME: "/h", ANTHROPIC_API_KEY: "sk-ant-test-0000", NVIDIA_API_KEY: "nvapi-test" } });
    expect(out).toMatchObject({ ok: true, scanner: "skillspector", scannerVersion: "2.13.2", date: "2026-10-28T18:30:00Z", findings: 2 });
    expect(calls[1]!.args.slice(0, 5)).toEqual(["scan", skillDir, "--no-llm", "--format", "sarif"]);
    for (const c of calls) {
      expect(Object.keys(c.env).sort()).toEqual(["HOME", "LANG", "LANGCHAIN_TRACING_V2", "LANGSMITH_TRACING", "LC_ALL", "NO_COLOR", "PATH", "SKILLSPECTOR_OSV_TIMEOUT"]);
      expect(JSON.stringify(c.env)).not.toMatch(/sk-ant|nvapi/);
    }
  });

  it("refuses a scanner that is not the pinned version", async () => {
    const exec = fakeExec({ version: { code: 0, stdout: "SkillSpector v2.12.0" }, scan: { code: 0, write: skillspectorSarif("2.12.0") } }, []);
    expect(await scanSkill(skillDir, { exec, now })).toMatchObject({ ok: false, reason: expect.stringMatching(/pinned 2.13.2/) });
  });

  it("records an error exit and an unreadable log as no scan", async () => {
    expect(await scanSkill(skillDir, { exec: fakeExec({ version: { code: 0, stdout: "SkillSpector v2.13.2" }, scan: { code: 2, stderr: "boom" } }, []), now })).toMatchObject({ ok: false, reason: "exit 2: boom" });
    expect(await scanSkill(skillDir, { exec: fakeExec({ version: { code: 0, stdout: "SkillSpector v2.13.2" }, scan: { code: 0, write: "{}" } }, []), now })).toMatchObject({ ok: false, reason: expect.stringMatching(/did not parse/) });
  });

  it("reads the version line it prints", () => {
    expect(parseSkillSpectorVersion("SkillSpector v2.13.2")).toBe("2.13.2");
    expect(parseSkillSpectorVersion("skillspector 2.13.2")).toBeUndefined();
  });
});

// docs/output-formats.md at b27af3b, the raw format.
const ciscoRaw = JSON.stringify({
  server_url: "static",
  requested_analyzers: ["yara"],
  scan_results: [
    { status: "completed", is_safe: false, item_type: "tool", tool_name: "execute_command", tool_description: "Run a command", findings: { yara_analyzer: { severity: "HIGH", threat_names: ["CODE EXECUTION"], threat_summary: "runs shell commands", total_findings: 1 } } },
    { status: "completed", is_safe: true, item_type: "tool", tool_name: "search", findings: { yara_analyzer: { severity: "SAFE", threat_names: [], threat_summary: "", total_findings: 0 } } },
    { status: "failed", is_safe: null, item_type: "tool", tool_name: "broken", findings: {} },
  ],
});

describe("Cisco's static YARA scan on a tool record", () => {
  const record = { source: "tools/list", recordedAt: "2026-10-28", tools: [{ name: "execute_command", description: "Run a command" }, { name: "search", description: "Search" }] };

  it("runs offline on the recorded list and converts its output to SARIF", async () => {
    const calls: Call[] = [];
    const out = await scanServerTools(record, { exec: fakeExec({ scan: { code: 0, stdout: ciscoRaw } }, calls), now, env: { PATH: "/usr/bin", VIRUSTOTAL_API_KEY: "vt", MCP_SCANNER_API_KEY: "k" } });
    expect(out).toMatchObject({ ok: true, scanner: "cisco-mcp-scanner", scannerVersion: "4.8.7", findings: 1 });
    expect(calls[0]!.args.slice(0, 6)).toEqual(["--analyzers", "yara", "--format", "raw", "static", "--tools"]);
    expect(calls[0]!.env).not.toHaveProperty("VIRUSTOTAL_API_KEY");
    expect(calls[0]!.env).not.toHaveProperty("MCP_SCANNER_API_KEY");
    expect(calls[0]!.env["MCP_SCANNER_VIRUSTOTAL_ENABLED"]).toBe("false");
    if (!out.ok) throw new Error(out.reason);
    expect(out.notes).toEqual(["the scanner reports its run did not complete", "broken: scan failed"]);
    expect(parseSarif(out.sarifText)).not.toBeTypeOf("string");
  });

  it("never carries is_safe into the record", () => {
    const sarif = ciscoToSarif(ciscoRaw, "4.8.7");
    expect(JSON.stringify(sarif)).not.toMatch(/is_safe|"safe"/i);
  });

  it("does not scan a record of names only", async () => {
    const out = await scanServerTools({ source: "x", recordedAt: "2026-10-27", tools: [{ name: "web_search_exa" }] }, { exec: fakeExec({}, []), now });
    expect(out).toMatchObject({ ok: false, reason: expect.stringMatching(/names only/) });
  });

  it("reports output that is not its JSON", () => {
    expect(ciscoToSarif("Traceback (most recent call last)", "4.8.7")).toMatch(/not JSON/);
    expect(ciscoToSarif('{"x":1}', "4.8.7")).toMatch(/no scan_results/);
  });
});

describe("pins and environment", () => {
  it("pins both scanners and names their outbound hosts", () => {
    const p = loadPinned();
    expect(p.skillspector).toMatchObject({ version: "2.13.2", outbound: ["api.osv.dev"] });
    expect(p.skillspector.install).toMatch(/@abd82935b67f765a9541013c0866c6349251dcdf$/);
    expect(p["cisco-mcp-scanner"]).toMatchObject({ version: "4.8.7", outbound: [] });
  });

  it("passes no variable of the caller's but PATH and HOME", () => {
    const env = scannerEnv("cisco-mcp-scanner", { PATH: "/p", HOME: "/h", AWS_SECRET_ACCESS_KEY: "x", GITHUB_TOKEN: "y" });
    expect(Object.keys(env).sort()).toEqual(["HOME", "LANG", "LC_ALL", "MCP_SCANNER_VIRUSTOTAL_ENABLED", "NO_COLOR", "PATH"]);
  });

  it("writes dates in the contract's form", () => {
    expect(isoSeconds(new Date("2026-10-28T18:30:00.999Z"))).toBe("2026-10-28T18:30:00Z");
  });
});

describe("the card (records, never a verdict)", () => {
  const base: TrustRecord = { contentSha256: PLACEHOLDER_SHA, scans: [], liveness: { state: "unknown" } };
  const scan = (scanner: string, date: string, findings: number) => ({ scanner, scannerVersion: "1.0.0", date, findings });
  const at = new Date("2026-10-28T18:30:00Z");

  it("says what was not recorded", () => {
    expect(cardLines(base, at)).toEqual(["not scanned", "content hash: not recorded", "namespace: not proven", "provenance: none recorded", "liveness: unknown"]);
  });

  it("shows the newest scan per scanner with its date and age", () => {
    const t: TrustRecord = { ...base, scans: [scan("skillspector", "2026-10-20T00:00:00Z", 4), scan("skillspector", "2026-10-28T10:00:00Z", 1), scan("cisco-mcp-scanner", "2026-10-25T00:00:00Z", 0)] };
    expect(cardLines(t, at).slice(0, 3)).toEqual([
      "scanned on 2026-10-28 by skillspector 1.0.0: 1 finding",
      "scanned on 2026-10-25 (3 days ago) by cisco-mcp-scanner 1.0.0: 0 findings",
      "a scan records what one tool's rules matched on that day; scanners miss things and flag things that are fine",
    ]);
  });

  it("never calls anything safe, secure or verified", () => {
    const shapes: TrustRecord[] = [
      base,
      { ...base, scans: [scan("skillspector", "2026-10-28T10:00:00Z", 0)], contentSha256: "a".repeat(64), namespaceProof: "github-oidc", provenance: { kind: "sigstore", signer_identity: "x" }, liveness: { state: "alive", checkedAt: "2026-10-28T10:00:00Z" } },
      { ...base, capabilityDrift: { fromVersion: "1", toVersion: "2", changed: ["description", "endpoint-host"] }, liveness: { state: "dead" } },
    ];
    for (const s of shapes) {
      expect(validTrust(s), JSON.stringify(validTrust.errors)).toBe(true);
      for (const line of cardLines(s, at)) expect(line).not.toMatch(/\b(safe|secure|verified|trusted|clean)\b/i);
    }
  });
});

describe("rescans", () => {
  it("are due for a version never scanned and daily after", () => {
    const root = mkdtempSync(join(tmpdir(), "tk-due-"));
    try {
      const store = new RecordStore(join(root, "records"));
      store.putScan("web-search", "1.0.0", { scanner: "skillspector", scannerVersion: "2.13.2", date: "2026-10-28T10:00:00Z", findings: 0 }, undefined);
      store.putScan("exa", "2026.10.27", { scanner: "cisco-mcp-scanner", scannerVersion: "4.8.7", date: "2026-10-27T09:00:00Z", findings: 0 }, undefined);
      const targets = [
        { id: "web-search", version: "1.0.0", kind: "skill" as const },
        { id: "web-search", version: "1.1.0", kind: "skill" as const },
        { id: "exa", version: "2026.10.27", kind: "server-tools" as const },
      ];
      expect(dueScans(targets, store, new Date("2026-10-28T18:30:00Z"))).toEqual([
        { id: "exa", version: "2026.10.27", scanner: "cisco-mcp-scanner", reason: "last scanned 2026-10-27T09:00:00Z" },
        { id: "web-search", version: "1.1.0", scanner: "skillspector", reason: "never scanned" },
      ]);
      expect(latestScans(store.scans("web-search", "1.0.0"))).toHaveLength(1);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
