// tk library trust command line (lane H; toolkit plan T4.1 to T4.3).
//
//   node library/trust/cli.ts names [catalog-dir]           name rules on every entry; exit 1 on an error
//   node library/trust/cli.ts record [catalog-dir]          record each server version's manifest; exit 1 on a reused version
//   node library/trust/cli.ts drift <id>                     what changed between the last two recorded versions
//   node library/trust/cli.ts pin <id> --harness pi [--tool <name>]...   print the policy rules that pin descriptions
//   node library/trust/cli.ts due [catalog-dir]              which scans are due
//   node library/trust/cli.ts scan [catalog-dir]             run the due scans (TK_TRUST_SCAN=1 only)
//   node library/trust/cli.ts osv [catalog-dir]              look up each package in OSV (TK_TRUST_ONLINE=1 only)
//   node library/trust/cli.ts card <id> [catalog-dir]        the card's trust lines
//
// Writes only under library/trust/records/ (or TK_TRUST_RECORDS). Never writes
// the policy file, a harness config or the catalog: pins are printed for the
// user or `tk connect`. Runs no program and makes no outbound call unless the
// variable named on the line asks (rules section 5, items 2 and 8).

import { dirname, resolve } from "node:path";
import { REPO_ROOT } from "../validate/schemas.ts";
import { loadCatalog } from "../validate/catalog.ts";
import type { LoadedEntry } from "../validate/types.ts";
import { getMeta } from "../validate/types.ts";
import { cardLines, dueScans, type DueTarget, type TrustRecord } from "./card.ts";
import { CATALOG_DIR, identityOf, loadBrands, loadReserved } from "./identity.ts";
import { checkName } from "./names.ts";
import { capabilityDrift, diffManifests, manifestOf, pinsFor } from "./manifest.ts";
import { lookupEntries, osvToSarif } from "./osv.ts";
import { RECORDS_DIR, RecordStore } from "./records.ts";
import { scanServerTools, scanSkill } from "./scanners.ts";

function usage(): never {
  console.error("usage: node library/trust/cli.ts names|record|due|scan|osv [catalog-dir] | drift <id> | pin <id> --harness pi [--tool <name>]... | card <id> [catalog-dir]");
  process.exit(2);
}

const store = new RecordStore(process.env["TK_TRUST_RECORDS"] ? resolve(process.env["TK_TRUST_RECORDS"]) : RECORDS_DIR);
const versionOf = (e: LoadedEntry) => (typeof e.data["version"] === "string" ? (e.data["version"] as string) : "0.0.0");

/** Scan targets: each server with a tool record, each skill whose SKILL.md is in this repository. */
function targets(entries: LoadedEntry[]): (DueTarget & { entry: LoadedEntry })[] {
  const out: (DueTarget & { entry: LoadedEntry })[] = [];
  for (const e of entries) {
    if (e.kind === "server" && e.tools) out.push({ id: e.id, version: versionOf(e), kind: "server-tools", entry: e });
    if (e.kind === "skill" && e.skillMd) out.push({ id: e.id, version: versionOf(e), kind: "skill", entry: e });
  }
  return out;
}

const [cmd, ...rest] = process.argv.slice(2);
const catalogArg = (i: number) => (rest[i] ? resolve(rest[i]!) : CATALOG_DIR);

if (cmd === "names") {
  const dir = catalogArg(0);
  const catalog = loadCatalog(dir);
  const opts = { brands: loadBrands(dir), reserved: loadReserved() };
  let errors = 0;
  for (const e of catalog.entries) {
    const title = typeof e.data["title"] === "string" ? (e.data["title"] as string) : undefined;
    const description = typeof e.data["description"] === "string" ? (e.data["description"] as string) : undefined;
    for (const f of checkName({ id: e.id, ...(title !== undefined ? { title } : {}), ...(description !== undefined ? { description } : {}), identity: identityOf(e) }, opts)) {
      if (f.severity === "error") errors++;
      console.log(`${f.severity === "error" ? "error  " : "warning"} ${e.id} ${f.path}: [${f.rule}] ${f.message}`);
    }
  }
  console.log(`${catalog.entries.length} entries, ${errors} errors`);
  process.exit(errors > 0 ? 1 : 0);
} else if (cmd === "record") {
  const catalog = loadCatalog(catalogArg(0));
  let reused = 0;
  for (const e of catalog.entries.filter((x) => x.kind === "server")) {
    const m = manifestOf(e.data, e.tools);
    const r = store.putManifest(e.id, m);
    if (r === "reused") reused++;
    console.log(`${r.padEnd(6)} ${e.id} ${m.serverVersion} ${m.manifestSha256.slice(0, 12)}`);
  }
  if (reused > 0) console.error(`${reused} version(s) already recorded with other content: a republished version is refused (versions are immutable)`);
  process.exit(reused > 0 ? 1 : 0);
} else if (cmd === "drift") {
  const id = rest[0] ?? usage();
  const versions = store.versions(id);
  if (versions.length < 2) {
    console.log(`${id}: ${versions.length} version(s) recorded; nothing to compare`);
    process.exit(0);
  }
  const prev = store.manifest(id, versions[versions.length - 2]!)!;
  const next = store.manifest(id, versions[versions.length - 1]!)!;
  console.log(JSON.stringify({ capabilityDrift: capabilityDrift(prev, next) ?? null, details: diffManifests(prev, next) }, null, 2));
} else if (cmd === "pin") {
  const id = rest[0] ?? usage();
  let harness = "";
  const tools: string[] = [];
  for (let i = 1; i < rest.length; i++) {
    if (rest[i] === "--harness" && rest[i + 1]) harness = rest[++i]!;
    else if (rest[i] === "--tool" && rest[i + 1]) tools.push(rest[++i]!);
    else usage();
  }
  if (!harness) usage();
  const versions = store.versions(id);
  const m = versions.length > 0 ? store.manifest(id, versions[versions.length - 1]!) : undefined;
  if (!m) {
    console.error(`${id}: no manifest recorded; run 'record' first`);
    process.exit(1);
  }
  try {
    const pins = pinsFor(m, id, harness, tools.length > 0 ? tools : undefined);
    console.log(JSON.stringify({ rules: pins.rules }, null, 2));
    for (const s of pins.skipped) console.error(`not pinned: ${s.tool}: ${s.reason}`);
    console.error("Printed only. Merge these rules into ~/.tk/policy.json yourself; tk never writes it.");
  } catch (e) {
    console.error((e as Error).message);
    process.exit(1);
  }
} else if (cmd === "due" || cmd === "scan") {
  const catalog = loadCatalog(catalogArg(0));
  const all = targets(catalog.entries);
  const due = dueScans(all, store, new Date());
  if (cmd === "due") {
    for (const d of due) console.log(`${d.id} ${d.version} ${d.scanner}: ${d.reason}`);
    console.log(`${due.length} scan(s) due`);
    process.exit(0);
  }
  if (process.env["TK_TRUST_SCAN"] !== "1") {
    console.error("scan runs the pinned scanners (library/trust/scanners.json) and is off unless TK_TRUST_SCAN=1; SkillSpector's OSV lookup reaches api.osv.dev");
    process.exit(2);
  }
  let failed = 0;
  for (const d of due) {
    const t = all.find((x) => x.id === d.id && x.version === d.version)!;
    const out = t.kind === "skill" ? await scanSkill(dirname(resolve(REPO_ROOT, t.entry.skillMd!.path))) : await scanServerTools(t.entry.tools!);
    if (!out.ok) {
      failed++;
      console.log(`not scanned ${d.id} ${d.version} ${d.scanner}: ${out.reason}`);
      continue;
    }
    const rec = store.putScan(d.id, d.version, { scanner: out.scanner, scannerVersion: out.scannerVersion, date: out.date, findings: out.findings }, out.sarifText);
    console.log(`scanned ${d.id} ${d.version} by ${rec.scanner} ${rec.scannerVersion}: ${rec.findings} findings`);
    for (const n of out.notes) console.log(`  note: ${n}`);
  }
  process.exit(failed > 0 ? 1 : 0);
} else if (cmd === "osv") {
  if (process.env["TK_TRUST_ONLINE"] !== "1") {
    console.error("osv posts package names and versions to https://api.osv.dev/v1/querybatch and is off unless TK_TRUST_ONLINE=1");
    process.exit(2);
  }
  const catalog = loadCatalog(catalogArg(0));
  const results = await lookupEntries(catalog.entries.filter((e) => e.kind === "server").map((e) => ({ id: e.id, version: versionOf(e), data: e.data })));
  for (const r of results) {
    if (r.queries.length === 0) continue;
    const sarif = osvToSarif(r);
    const rec = store.putScan(r.id, r.version, { scanner: "osv", scannerVersion: "api-v1", date: r.date, findings: r.vulns.length }, JSON.stringify(sarif, null, 2) + "\n");
    console.log(`osv ${r.id} ${r.version}: ${rec.findings} known vulnerabilities in ${r.queries.length} package(s)`);
  }
} else if (cmd === "card") {
  const id = rest[0] ?? usage();
  const catalog = loadCatalog(catalogArg(1));
  const e = catalog.entries.find((x) => x.id === id);
  if (!e) {
    console.error(`no entry '${id}'`);
    process.exit(1);
  }
  const meta = getMeta(e.kind, e.data);
  const base = (meta?.trust ?? { contentSha256: "0".repeat(64), scans: [], liveness: { state: "unknown" } }) as TrustRecord;
  const version = versionOf(e);
  const trust: TrustRecord = { ...base, scans: [...(base.scans ?? []), ...store.scans(id, version)] };
  const versions = store.versions(id);
  if (!trust.capabilityDrift && versions.length >= 2) {
    const d = capabilityDrift(store.manifest(id, versions[versions.length - 2]!)!, store.manifest(id, versions[versions.length - 1]!)!);
    if (d) trust.capabilityDrift = d;
  }
  for (const line of cardLines(trust)) console.log(line);
} else {
  usage();
}
