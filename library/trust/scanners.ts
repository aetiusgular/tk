// Running the scanners (toolkit plan T4.1; lane H).
//
// Two scanners, pinned in scanners.json, each on the target it reads:
//
// - NVIDIA SkillSpector 2.13.2 on a skill folder:
//   `skillspector scan <dir> --no-llm --format sarif --output <file>`
//   (src/skillspector/cli.py:442-459 at abd8293). Exit 0 or 1 is a finished
//   scan (1 means its risk score passed 50), 2 an error (README "Exit codes").
//   `--no-llm` keeps file contents from its LLM provider; its OSV lookup to
//   api.osv.dev still runs and has no off switch (nodes/analyzers/osv_client.py:42-45).
// - Cisco AI Defense MCP Scanner 4.8.7 on a server's recorded tool list,
//   without connecting: `mcp-scanner --analyzers yara --format raw static
//   --tools <file>` (README, docs/static-scanning.md, mcpscanner/cli.py:1545-1653
//   at b27af3b). It writes no SARIF and exits 0 whatever it finds; tk turns its
//   raw JSON into SARIF (`ciscoToSarif`). YARA alone makes no outbound call.
//
// Neither is trusted to decide anything. Trail of Bits built skills that passed
// every scanner it tried (3 Jun 2026, "The sorry state of skill
// distribution"), and marketplace scanners flag up to 46.8% of skills against
// 0.52% that stay suspicious (arXiv 2603.16572, note AP section 5). A scan is a
// dated record: "scanned on <date> by <scanner> <version>: N findings".
//
// Every run is an argument list, never a shell string (rules section 5, item
// 3), with a fixed environment: the caller's environment is not passed, so no
// key reaches a scanner and Cisco's `api`, `llm` and `virustotal` analyzers
// cannot switch themselves on (its config.py:141-145 enables VirusTotal when a
// key is present).

import { execFile } from "node:child_process";
import { lstatSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ToolsRecord } from "../validate/types.ts";
import { isRecord } from "../validate/types.ts";
import { TRUST_DIR } from "./identity.ts";
import { MAX_SARIF_BYTES } from "./records.ts";
import type { SarifLog } from "./sarif.ts";
import { countFindings, parseSarif } from "./sarif.ts";

export type ScannerId = "skillspector" | "cisco-mcp-scanner";
export type TargetKind = "skill" | "server-tools";

export type PinnedScanner = { version: string; install: string; targets: TargetKind[]; outbound: string[] };

export function loadPinned(path = join(TRUST_DIR, "scanners.json")): Record<ScannerId, PinnedScanner> {
  const raw: unknown = JSON.parse(readFileSync(path, "utf8"));
  if (!isRecord(raw) || !isRecord(raw["scanners"])) throw new Error(`${path}: no 'scanners' object`);
  return raw["scanners"] as Record<ScannerId, PinnedScanner>;
}

export type ExecResult = { code: number | null; stdout: string; stderr: string; timedOut: boolean };
export type Exec = (file: string, args: string[], opts: { env: Record<string, string>; cwd: string; timeoutMs: number }) => Promise<ExecResult>;

const MAX_OUTPUT = MAX_SARIF_BYTES;

/** Runs a program by argument list with a fixed environment and a timeout. */
export const realExec: Exec = (file, args, opts) =>
  new Promise((resolve) => {
    execFile(file, args, { env: opts.env, cwd: opts.cwd, timeout: opts.timeoutMs, maxBuffer: MAX_OUTPUT, killSignal: "SIGKILL", windowsHide: true }, (err, stdout, stderr) => {
      const e = err as (NodeJS.ErrnoException & { code?: number | string; killed?: boolean }) | null;
      const code = e === null ? 0 : typeof e.code === "number" ? e.code : null;
      resolve({ code, stdout: String(stdout), stderr: String(stderr), timedOut: e?.killed === true });
    });
  });

/** The only variables a scanner sees. `PATH` and `HOME` come from the caller
 *  (the scanner's own install needs them); nothing else does. */
export function scannerEnv(scanner: ScannerId, from: NodeJS.ProcessEnv = process.env): Record<string, string> {
  const env: Record<string, string> = { LANG: "C.UTF-8", LC_ALL: "C.UTF-8", NO_COLOR: "1" };
  if (from["PATH"]) env["PATH"] = from["PATH"];
  if (from["HOME"]) env["HOME"] = from["HOME"];
  if (scanner === "skillspector") {
    // LangChain sends traces only when one of these is true and a key is set
    // (SkillSpector's .env.example:80-81 sets the first false); both false, no key.
    env["LANGCHAIN_TRACING_V2"] = "false";
    env["LANGSMITH_TRACING"] = "false";
    // Its OSV lookup cannot be turned off; a short timeout bounds it, and its
    // bundled list stands in when the host is unreachable (README).
    env["SKILLSPECTOR_OSV_TIMEOUT"] = "10";
  }
  if (scanner === "cisco-mcp-scanner") env["MCP_SCANNER_VIRUSTOTAL_ENABLED"] = "false";
  return env;
}

export type ScanOutcome =
  | { ok: true; scanner: ScannerId; scannerVersion: string; date: string; findings: number; sarifText: string; notes: string[] }
  | { ok: false; scanner: ScannerId; reason: string };

/** `YYYY-MM-DDTHH:MM:SSZ`, the contract's form (no milliseconds). */
export function isoSeconds(d: Date): string {
  return d.toISOString().replace(/\.\d{3}Z$/, "Z");
}

function readCapped(path: string): string {
  const st = lstatSync(path);
  if (!st.isFile()) throw new Error(`${path} is not a plain file`);
  if (st.size > MAX_OUTPUT) throw new Error(`${path} is ${st.size} bytes, over ${MAX_OUTPUT}`);
  return readFileSync(path, "utf8");
}

export type RunOptions = { exec?: Exec; now?: () => Date; timeoutMs?: number; env?: NodeJS.ProcessEnv; bin?: string; pinned?: Record<ScannerId, PinnedScanner> };

/** `skillspector --version` prints `SkillSpector v<version>` (cli.py:286-301). */
export function parseSkillSpectorVersion(stdout: string): string | undefined {
  return /SkillSpector v(\d+\.\d+\.\d+\S*)/.exec(stdout)?.[1];
}

/** Scans one skill folder with SkillSpector. */
export async function scanSkill(skillDir: string, opts: RunOptions = {}): Promise<ScanOutcome> {
  const exec = opts.exec ?? realExec;
  const pinned = (opts.pinned ?? loadPinned()).skillspector;
  const bin = opts.bin ?? "skillspector";
  const env = scannerEnv("skillspector", opts.env);
  const timeoutMs = opts.timeoutMs ?? 300_000;
  const st = lstatSync(skillDir);
  if (!st.isDirectory()) return { ok: false, scanner: "skillspector", reason: `${skillDir} is not a folder` };
  const work = mkdtempSync(join(tmpdir(), "tk-scan-"));
  try {
    const v = await exec(bin, ["--version"], { env, cwd: work, timeoutMs: 30_000 });
    const version = parseSkillSpectorVersion(v.stdout);
    if (v.code !== 0 || version === undefined) return { ok: false, scanner: "skillspector", reason: `version probe failed (exit ${v.code})` };
    if (version !== pinned.version) return { ok: false, scanner: "skillspector", reason: `installed ${version}, pinned ${pinned.version}; refusing to record a scan from an unpinned scanner` };
    const out = join(work, "report.sarif");
    const date = isoSeconds((opts.now ?? (() => new Date()))());
    const r = await exec(bin, ["scan", skillDir, "--no-llm", "--format", "sarif", "--output", out], { env, cwd: work, timeoutMs });
    if (r.timedOut) return { ok: false, scanner: "skillspector", reason: `timed out after ${timeoutMs} ms` };
    if (r.code !== 0 && r.code !== 1) return { ok: false, scanner: "skillspector", reason: `exit ${r.code}: ${r.stderr.slice(0, 300)}` };
    const text = readCapped(out);
    const log = parseSarif(text);
    if (typeof log === "string") return { ok: false, scanner: "skillspector", reason: `its SARIF did not parse: ${log}` };
    const notes = incompleteNotes(log);
    return { ok: true, scanner: "skillspector", scannerVersion: version, date, findings: countFindings(log), sarifText: text, notes };
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

/** SkillSpector records partial coverage (an OSV timeout, a file it could not
 *  read) as a failed invocation or notifications; the card shows them. */
function incompleteNotes(log: SarifLog): string[] {
  const notes: string[] = [];
  for (const run of log.runs) {
    for (const inv of run.invocations ?? []) {
      if (inv.executionSuccessful === false) notes.push("the scanner reports its run did not complete");
      for (const n of inv.toolExecutionNotifications ?? []) if (typeof n.message?.text === "string") notes.push(n.message.text.slice(0, 200));
    }
  }
  return notes;
}

/** Scans a server's recorded tool list with Cisco's static YARA scan. The
 *  tool list is the `tools/list` result shape (`{"tools": [...]}`,
 *  docs/static-scanning.md). A list with no descriptions is not scanned: YARA
 *  reads descriptions and schemas, and a scan of names alone would read as a
 *  clean result it is not. */
export async function scanServerTools(tools: ToolsRecord, opts: RunOptions = {}): Promise<ScanOutcome> {
  const exec = opts.exec ?? realExec;
  const pinned = (opts.pinned ?? loadPinned())["cisco-mcp-scanner"];
  const bin = opts.bin ?? "mcp-scanner";
  if (!tools.tools.some((t) => typeof t.description === "string" || t.inputSchema !== undefined)) {
    return { ok: false, scanner: "cisco-mcp-scanner", reason: "the tool record holds names only; nothing for YARA to read (lane G's mirror records tools/list)" };
  }
  const env = scannerEnv("cisco-mcp-scanner", opts.env);
  const timeoutMs = opts.timeoutMs ?? 300_000;
  const work = mkdtempSync(join(tmpdir(), "tk-scan-"));
  try {
    const input = join(work, "tools-list.json");
    writeFileSync(input, JSON.stringify({ tools: tools.tools }), { flag: "wx", mode: 0o600 });
    const date = isoSeconds((opts.now ?? (() => new Date()))());
    const r = await exec(bin, ["--analyzers", "yara", "--format", "raw", "static", "--tools", input], { env, cwd: work, timeoutMs });
    if (r.timedOut) return { ok: false, scanner: "cisco-mcp-scanner", reason: `timed out after ${timeoutMs} ms` };
    if (r.code !== 0) return { ok: false, scanner: "cisco-mcp-scanner", reason: `exit ${r.code}: ${r.stderr.slice(0, 300)}` };
    // It has no version flag (cli.py:1315, 1365 are package versions for
    // pypi-scan); the version recorded is the pinned install's.
    const sarif = ciscoToSarif(r.stdout, pinned.version);
    if (typeof sarif === "string") return { ok: false, scanner: "cisco-mcp-scanner", reason: sarif };
    const text = JSON.stringify(sarif, null, 2) + "\n";
    return { ok: true, scanner: "cisco-mcp-scanner", scannerVersion: pinned.version, date, findings: countFindings(sarif), sarifText: text, notes: incompleteNotes(sarif) };
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

const CISCO_LEVEL: Record<string, "error" | "warning" | "note" | "none"> = { HIGH: "error", MEDIUM: "warning", LOW: "note", INFO: "note", UNKNOWN: "note", SAFE: "none" };

/** The first JSON object in the output: the raw format is one JSON document,
 *  and a log line printed before it (I: not seen in its docs) is skipped. */
function firstJson(stdout: string): unknown {
  const t = stdout.trim();
  try {
    return JSON.parse(t);
  } catch {
    const at = t.indexOf("\n{");
    if (at < 0) throw new Error("no JSON object in the scanner's output");
    return JSON.parse(t.slice(at + 1));
  }
}

/**
 * Cisco's raw JSON (docs/output-formats.md at b27af3b: `scan_results[]` with
 * `status`, `tool_name`, `findings{<analyzer>: {severity, threat_names[],
 * threat_summary, total_findings}}`) as a SARIF 2.1.0 log, one result per
 * analyzer and threat with a non-SAFE severity. `is_safe` is not carried: tk
 * records findings, never a verdict. A tool whose scan did not complete
 * becomes a failed invocation's notification.
 */
export function ciscoToSarif(stdout: string, version: string): SarifLog | string {
  let raw: unknown;
  try {
    raw = firstJson(stdout);
  } catch (e) {
    return `its output was not JSON: ${(e as Error).message}`;
  }
  if (!isRecord(raw) || !Array.isArray(raw["scan_results"])) return "its output has no scan_results";
  const results: SarifLog["runs"][number]["results"] = [];
  const rules = new Map<string, string>();
  const notifications: { level: "warning"; message: { text: string } }[] = [];
  for (const item of raw["scan_results"]) {
    if (!isRecord(item)) continue;
    const tool = typeof item["tool_name"] === "string" ? item["tool_name"] : "(unnamed)";
    const status = item["status"];
    if (status !== "completed") notifications.push({ level: "warning", message: { text: `${tool}: scan ${typeof status === "string" ? status : "status missing"}` } });
    const findings = item["findings"];
    if (!isRecord(findings)) continue;
    for (const [analyzer, f] of Object.entries(findings)) {
      if (!isRecord(f)) continue;
      const severity = typeof f["severity"] === "string" ? f["severity"].toUpperCase() : "UNKNOWN";
      const level = CISCO_LEVEL[severity] ?? "note";
      const total = typeof f["total_findings"] === "number" ? f["total_findings"] : 0;
      if (level === "none" && total === 0) continue;
      const names = Array.isArray(f["threat_names"]) && f["threat_names"].length > 0 ? f["threat_names"].filter((n): n is string => typeof n === "string") : ["finding"];
      const summary = typeof f["threat_summary"] === "string" ? f["threat_summary"].slice(0, 1000) : "";
      for (const name of names) {
        const ruleId = `${analyzer}/${name.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`;
        rules.set(ruleId, name);
        results.push({
          ruleId,
          level: level === "none" ? "note" : level,
          message: { text: `${tool}: ${name}${summary ? `: ${summary}` : ""}` },
          locations: [{ logicalLocations: [{ name: tool, kind: "function" }] }],
          properties: { severity, analyzer },
        });
      }
    }
  }
  return {
    $schema: "https://json.schemastore.org/sarif-2.1.0.json",
    version: "2.1.0",
    runs: [
      {
        tool: { driver: { name: "cisco-mcp-scanner", version, informationUri: "https://github.com/cisco-ai-defense/mcp-scanner", rules: [...rules].map(([id, name]) => ({ id, shortDescription: { text: name } })) } },
        results,
        invocations: [{ executionSuccessful: notifications.length === 0, toolExecutionNotifications: notifications }],
        properties: { convertedBy: "tk library/trust/scanners.ts ciscoToSarif", analyzers: ["yara"] },
      },
    ],
  };
}
