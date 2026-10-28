// The part of SARIF 2.1.0 tk reads (OASIS SARIF v2.1.0; lane H, T4.1).
//
// tk stores each scanner's log byte for byte and records one number from it,
// `findings`, for the card. The count is of results that report a problem and
// are not suppressed. `kind` defaults to `fail`; only `fail` is a problem
// (`pass`, `notApplicable`, `informational`, `review` and `open` are not).
// A result is suppressed when it carries a suppression whose `status` is
// absent or `accepted` (`underReview` and `rejected` are not suppressions in
// force). Levels are counted apart so a card can say "2 errors, 5 notes".
// Section numbers and the defaults: see the status doc's Research.

import { isRecord } from "../validate/types.ts";

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
  return Array.isArray(r.suppressions) && r.suppressions.some((s) => s.status === undefined || s.status === "accepted");
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
