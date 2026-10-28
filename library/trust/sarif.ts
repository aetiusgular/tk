// The part of SARIF 2.1.0 tk reads (OASIS SARIF v2.1.0; lane H, T4.1).
//
// tk stores each scanner's log byte for byte and records one number from it,
// `findings`, for the card. The count is of results that report a problem and
// are not suppressed. `kind` defaults to `fail`; only `fail` is a problem
// (`pass`, `notApplicable`, `informational`, `review` and `open` are not).
// A result is suppressed when its `suppressions` array is non-empty and every
// suppression's `status` is absent or `accepted`. SARIF 2.1.0 says a consumer
// "SHALL examine the status properties" (section 3.27.23) and defines
// `accepted`, `underReview` and `rejected` (section 3.35.3), but not what an
// absent status means; tk reads it as accepted (SkillSpector writes baseline
// suppressions with `kind: external` and no status), and a list holding any
// `underReview` or `rejected` as not suppressed, so a mixed list is counted:
// the side that fails safe (rules section 9, item 7). Levels are counted apart
// so a card can say "2 errors, 5 notes".
// `kind` defaults to `fail` (section 3.27.9); `level` defaults to `warning`
// for a failure with no rule configuration (section 3.27.10).
// Source: oasis-tcs/sarif-spec at 1742a25, sarif-2.1 committee specification.


import { isRecord } from "../validate/types.ts";

/** The schema URI of SARIF 2.1.0 with errata 01, as the schema's own `id` gives it. */
export const SARIF_SCHEMA = "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json";


export type SarifResult = {
  ruleId?: string;
  kind?: string;
  level?: string;
  message?: { text?: string };
  locations?: unknown[];
  suppressions?: { kind?: string; status?: string; justification?: string }[];
  properties?: Record<string, unknown>;
};

export type SarifRun = {
  tool: { driver: { name: string; version?: string; semanticVersion?: string; informationUri?: string; rules?: { id: string; shortDescription?: { text: string } }[] } };
  results?: SarifResult[];
  invocations?: { executionSuccessful?: boolean; toolExecutionNotifications?: { level?: string; message?: { text?: string } }[] }[];
  properties?: Record<string, unknown>;
};

export type SarifLog = { $schema?: string; version: "2.1.0"; runs: SarifRun[] };

/** Parses and shape-checks a log. Returns the reason when it is not one tk can count. */
export function parseSarif(text: string): SarifLog | string {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return "not JSON";
  }
  if (!isRecord(raw)) return "not an object";
  if (raw["version"] !== "2.1.0") return `version ${JSON.stringify(raw["version"])}, not 2.1.0`;
  if (!Array.isArray(raw["runs"])) return "no runs array";
  for (const [i, run] of raw["runs"].entries()) {
    if (!isRecord(run) || !isRecord(run["tool"]) || !isRecord(run["tool"]["driver"]) || typeof run["tool"]["driver"]["name"] !== "string") return `run ${i} has no tool.driver.name`;
    if (run["results"] !== undefined && !Array.isArray(run["results"])) return `run ${i}: results is not an array`;
  }
  return raw as unknown as SarifLog;
}

export function isSuppressed(r: SarifResult): boolean {
  return Array.isArray(r.suppressions) && r.suppressions.length > 0 && r.suppressions.every((s) => s.status === undefined || s.status === "accepted");
}

export function isProblem(r: SarifResult): boolean {
  return (r.kind ?? "fail") === "fail" && !isSuppressed(r);
}

/** The number the contract's `trust.scans[].findings` holds. */
export function countFindings(log: SarifLog): number {
  let n = 0;
  for (const run of log.runs) for (const r of run.results ?? []) if (isProblem(r)) n++;
  return n;
}

export type LevelCounts = { error: number; warning: number; note: number; none: number; suppressed: number };

/** Levels of the counted results (`level` defaults to `warning`), and how many were suppressed. */
export function levelCounts(log: SarifLog): LevelCounts {
  const c: LevelCounts = { error: 0, warning: 0, note: 0, none: 0, suppressed: 0 };
  for (const run of log.runs) {
    for (const r of run.results ?? []) {
      if (isSuppressed(r)) {
        c.suppressed++;
        continue;
      }
      if ((r.kind ?? "fail") !== "fail") continue;
      const level = r.level ?? "warning";
      if (level === "error" || level === "warning" || level === "note" || level === "none") c[level]++;
      else c.warning++;
    }
  }
  return c;
}
