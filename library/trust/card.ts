// What a card says about an entry's trust records, and which scans are due
// (toolkit plan T4.1, T4.2; lane H).
//
// Records, never a verdict (plan R7): every line names what was recorded and
// when, or that nothing was. No line says "safe", "secure" or "verified";
// `card.test.ts` holds that over every input shape.
//
// Rescans: every version is scanned when it is first recorded, and again
// daily, because a scanner's rules change and a clean result goes stale
// (ClawHub: "All active skills are re-scanned daily to detect if a previously
// clean skill becomes malicious", openclaw.ai/blog/virustotal-partnership, 7 Feb 2026).

import type { DriftKind } from "./manifest.ts";
import type { RecordStore, ScanRecord } from "./records.ts";
import type { ScannerId, TargetKind } from "./scanners.ts";

export const PLACEHOLDER_SHA = "0".repeat(64);

export type TrustRecord = {
  namespaceProof?: string;
  provenance?: { kind?: string; signer_identity?: string };
  contentSha256: string;
  scans: ScanRecord[];
  liveness: { state: string; checkedAt?: string };
  capabilityDrift?: { fromVersion: string; toVersion: string; changed: DriftKind[] };
};

const day = (iso: string) => iso.slice(0, 10);

function ageDays(iso: string, now: Date): number {
  return Math.floor((now.getTime() - Date.parse(iso)) / 86_400_000);
}

/** The newest record per scanner, newest first. */
export function latestScans(scans: ScanRecord[]): ScanRecord[] {
  const by = new Map<string, ScanRecord>();
  for (const s of scans) {
    const prev = by.get(s.scanner);
    if (!prev || s.date > prev.date) by.set(s.scanner, s);
  }
  return [...by.values()].sort((a, b) => (a.date < b.date ? 1 : a.date > b.date ? -1 : a.scanner.localeCompare(b.scanner)));
}

export function cardLines(trust: TrustRecord, now: Date = new Date()): string[] {
  const lines: string[] = [];
  const scans = latestScans(trust.scans);
  if (scans.length === 0) lines.push("not scanned");
  for (const s of scans) {
    const age = ageDays(s.date, now);
    const when = age >= 2 ? ` (${age} days ago)` : "";
    lines.push(`scanned on ${day(s.date)}${when} by ${s.scanner} ${s.scannerVersion}: ${s.findings} ${s.findings === 1 ? "finding" : "findings"}`);
  }
  if (scans.length > 0) lines.push("a scan records what one tool's rules matched on that day; scanners miss things and flag things that are fine");
  if (trust.capabilityDrift) lines.push(`changed from ${trust.capabilityDrift.fromVersion} to ${trust.capabilityDrift.toVersion}: ${trust.capabilityDrift.changed.join(", ")}`);
  lines.push(trust.contentSha256 === PLACEHOLDER_SHA ? "content hash: not recorded" : `content hash: ${trust.contentSha256.slice(0, 12)}…`);
  const proof = trust.namespaceProof ?? "none";
  lines.push(proof === "none" ? "namespace: not proven" : `namespace proven by ${proof}`);
  const prov = trust.provenance?.kind ?? "none";
  lines.push(prov === "none" ? "provenance: none recorded" : `provenance: ${prov}${trust.provenance?.signer_identity ? ` (${trust.provenance.signer_identity})` : ""}`);
  const live = trust.liveness;
  lines.push(live.state === "unknown" || live.state === "not-applicable" ? `liveness: ${live.state}` : `liveness: ${live.state}${live.checkedAt ? ` on ${day(live.checkedAt)}` : ""}`);
  return lines;
}

export type DueTarget = { id: string; version: string; kind: TargetKind };
export type Due = { id: string; version: string; scanner: ScannerId; reason: string };

const SCANNER_FOR: Record<TargetKind, ScannerId> = { skill: "skillspector", "server-tools": "cisco-mcp-scanner" };

/** Scans to run now: a version never scanned by its scanner, or last scanned
 *  `maxAgeHours` ago or more. Sorted by entry and version. */
export function dueScans(targets: DueTarget[], store: Pick<RecordStore, "scans">, now: Date, maxAgeHours = 24): Due[] {
  const due: Due[] = [];
  for (const t of targets) {
    const scanner = SCANNER_FOR[t.kind];
    const mine = store.scans(t.id, t.version).filter((s) => s.scanner === scanner);
    const last = latestScans(mine)[0];
    if (!last) {
      due.push({ id: t.id, version: t.version, scanner, reason: "never scanned" });
      continue;
    }
    const hours = (now.getTime() - Date.parse(last.date)) / 3_600_000;
    if (hours >= maxAgeHours) due.push({ id: t.id, version: t.version, scanner, reason: `last scanned ${last.date}` });
  }
  return due.sort((a, b) => a.id.localeCompare(b.id) || a.version.localeCompare(b.version));
}
