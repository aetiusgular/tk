// OSV lookups on an entry's packages (toolkit plan T4.1; lane H).
//
// POST https://api.osv.dev/v1/querybatch with one query per package version;
// the batch answer lists vulnerability ids per query (OSV API docs,
// google.github.io/osv.dev/api; the research for this item is in the status
// doc). Off unless TK_TRUST_ONLINE=1: it sends package names and versions,
// public catalog data, to Google's OSV service (rules section 5, item 8).
// Only packages with an exact version are looked up; an unpinned package is
// shown on the card as not looked up, because a query without a version asks
// about every version ever published.

import type { SarifLog } from "./sarif.ts";
import { isoSeconds } from "./scanners.ts";
import { isRecord } from "../validate/types.ts";

export const OSV_BATCH_URL = "https://api.osv.dev/v1/querybatch";
/** The API's limit per batch request (OSV docs). */
export const OSV_BATCH_LIMIT = 1000;

/** registryType (upstream server.json) to OSV ecosystem. OCI images have no
 *  OSV ecosystem; they are not looked up here. */
const ECOSYSTEM: Record<string, string> = { npm: "npm", pypi: "PyPI", nuget: "NuGet", cargo: "crates.io" };

export type OsvQuery = { package: { name: string; ecosystem: string }; version: string };
export type OsvEntryResult = { id: string; version: string; date: string; queries: OsvQuery[]; vulns: { query: number; id: string; modified?: string }[]; skipped: string[] };

export function queriesFor(data: Record<string, unknown>): { queries: OsvQuery[]; skipped: string[] } {
  const queries: OsvQuery[] = [];
  const skipped: string[] = [];
  if (!Array.isArray(data["packages"])) return { queries, skipped };
  for (const p of data["packages"]) {
    if (!isRecord(p) || typeof p["identifier"] !== "string") continue;
    const rt = typeof p["registryType"] === "string" ? p["registryType"] : "";
    const eco = ECOSYSTEM[rt];
    const v = typeof p["version"] === "string" ? p["version"] : undefined;
    if (!eco) {
      skipped.push(`${rt}:${p["identifier"]} (no OSV ecosystem)`);
      continue;
    }
    if (!v || !/^[0-9A-Za-z][0-9A-Za-z.+_-]*$/.test(v) || v === "latest") {
      skipped.push(`${rt}:${p["identifier"]} (no exact version)`);
      continue;
    }
    queries.push({ package: { name: p["identifier"], ecosystem: eco }, version: v });
  }
  return { queries, skipped };
}

export type Fetch = (url: string, init: { method: "POST"; headers: Record<string, string>; body: string; signal: AbortSignal }) => Promise<{ ok: boolean; status: number; json: () => Promise<unknown> }>;

/** Looks up every entry's packages in batches. Results are ids only, as the batch endpoint returns them. */
export async function lookupEntries(entries: { id: string; version: string; data: Record<string, unknown> }[], opts: { fetch?: Fetch; now?: () => Date; timeoutMs?: number } = {}): Promise<OsvEntryResult[]> {
  const f = opts.fetch ?? (globalThis.fetch as unknown as Fetch);
  const date = isoSeconds((opts.now ?? (() => new Date()))());
  const out: OsvEntryResult[] = entries.map((e) => ({ id: e.id, version: e.version, date, ...queriesFor(e.data), vulns: [] }));
  const flat: { entry: number; query: number; q: OsvQuery }[] = [];
  out.forEach((r, entry) => r.queries.forEach((q, query) => flat.push({ entry, query, q })));
  for (let i = 0; i < flat.length; i += OSV_BATCH_LIMIT) {
    const chunk = flat.slice(i, i + OSV_BATCH_LIMIT);
    const res = await f(OSV_BATCH_URL, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ queries: chunk.map((c) => c.q) }), signal: AbortSignal.timeout(opts.timeoutMs ?? 30_000) });
    if (!res.ok) throw new Error(`OSV answered ${res.status}`);
    const body = await res.json();
    if (!isRecord(body) || !Array.isArray(body["results"]) || body["results"].length !== chunk.length) throw new Error("OSV's answer does not match the batch");
    body["results"].forEach((r, k) => {
      const c = chunk[k]!;
      const vulns = isRecord(r) && Array.isArray(r["vulns"]) ? r["vulns"] : [];
      for (const v of vulns) if (isRecord(v) && typeof v["id"] === "string") out[c.entry]!.vulns.push({ query: c.query, id: v["id"], ...(typeof v["modified"] === "string" ? { modified: v["modified"] } : {}) });
    });
  }
  return out;
}

/** One SARIF result per (package, vulnerability id), so OSV's answer is stored like a scanner's. */
export function osvToSarif(r: OsvEntryResult): SarifLog {
  return {
    $schema: "https://json.schemastore.org/sarif-2.1.0.json",
    version: "2.1.0",
    runs: [
      {
        tool: { driver: { name: "osv", version: "api-v1", informationUri: "https://osv.dev", rules: [] } },
        results: r.vulns.map((v) => {
          const q = r.queries[v.query]!;
          return { ruleId: v.id, level: "warning", message: { text: `${q.package.ecosystem} ${q.package.name}@${q.version}: ${v.id} (https://osv.dev/vulnerability/${v.id})` }, properties: { ecosystem: q.package.ecosystem, package: q.package.name, version: q.version, ...(v.modified ? { modified: v.modified } : {}) } };
        }),
        invocations: [{ executionSuccessful: true, toolExecutionNotifications: r.skipped.map((s) => ({ level: "note", message: { text: `not looked up: ${s}` } })) }],
        properties: { source: "OSV data, CC-BY 4.0" },
      },
    ],
  };
}
