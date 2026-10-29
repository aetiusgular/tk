// One mirror run (lane G, T3.2): pull, store, recompute latest, probe,
// flag, write the entries and the index.
import { mirroredEntry, remoteUrls } from "./entry.ts";
import { computeFlags, type Flag } from "./flags.ts";
import { canResolve, combine, machineLooksOffline, pool, probe, type ProbeDetail, type ProbeOptions } from "./liveness.ts";
import { refuseUrl } from "./netguard.ts";
import { OFFICIAL_KEY, OFFICIAL_REGISTRY, RegistryClient, type ClientOptions, type ServerRecord } from "./registry.ts";
import { Store } from "./store.ts";

export type PullOptions = {
  out: string;
  /** Pull everything, deleted servers included, and flag what upstream no longer returns. */
  full?: boolean;
  client?: ClientOptions;
  /** Probe remotes (default true). The probes are outbound calls to hosts strangers named. */
  probe?: boolean;
  probeOptions?: ProbeOptions;
  /** A URL probed more recently than this is not probed again (default 24 h). */
  probeMaxAgeMs?: number;
  /** At most this many URLs per run (default 2,000), oldest first. */
  probeBudget?: number;
  probeConcurrency?: number;
  /** Per publisher per UTC day (default 20). */
  cap?: number;
  /** Re-read this much before the watermark (default 10 min). */
  overlapMs?: number;
  maxPages?: number;
  now?: () => Date;
  log?: (line: string) => void;
};

export type PullReport = {
  mode: "full" | "incremental";
  updatedSince?: string;
  pages: number;
  requests: number;
  received: number;
  new: number;
  changed: number;
  skipped: number;
  latestCorrected: number;
  missingUpstream: number;
  probed: number;
  /** URLs a probe would refuse (a single-label host, a templated URL),
   * recorded as not probed, outside the budget (box test w2, BW9). */
  refused: number;
  /** Why the probes did not run or were not kept, when they were not. */
  probeSkipped?: string;
  servers: number;
  watermark?: string;
};

export type IndexLine = {
  latest: string | null;
  versions: number;
  status: "active" | "deprecated" | "deleted";
  liveness: "alive" | "dead" | "unknown" | "not-applicable";
  /** A bulk-publisher flag here holds no count: the count is kept once per
   * publisher and day in index.json's `bulkPublishers` (box test w2, BW7). */
  flags: RowFlag[];
  missingUpstream?: true;
};

export type RowFlag = Exclude<Flag, { flag: "bulk-publisher" }> | { flag: "bulk-publisher"; publisher: string; day: string; cap: number };

/** A row's flags, and the bulk counts they carried, apart. */
function rowFlags(flags: Flag[], counts: Record<string, number>): RowFlag[] {
  return flags.map((f) => {
    if (f.flag !== "bulk-publisher") return f;
    counts[`${f.publisher} ${f.day}`] = f.count;
    return { flag: f.flag, publisher: f.publisher, day: f.day, cap: f.cap };
  });
}

function iso(d: Date): string {
  return d.toISOString().replace(/\.\d{3}Z$/, "Z");
}

/** The latest version of one server from what the store holds. The registry
 *  clears `is_latest` on the old version without moving its `updated_at`
 *  (`internal/database/postgres.go:996, 1020` at 9c4a96c), so an incremental
 *  pull can leave two versions marked latest; the one published last wins. */
export function pickLatest(versions: ServerRecord[]): ServerRecord | undefined {
  const marked = versions.filter((v) => v._meta[OFFICIAL_KEY].isLatest === true);
  const pool = marked.length ? marked : [];
  const key = (v: ServerRecord) => v._meta[OFFICIAL_KEY].publishedAt ?? v._meta[OFFICIAL_KEY].updatedAt;
  return pool.sort((a, b) => (key(a) < key(b) ? 1 : key(a) > key(b) ? -1 : a.server.version < b.server.version ? 1 : -1))[0];
}

export async function pull(o: PullOptions): Promise<PullReport> {
  const now = o.now ?? (() => new Date());
  const log = o.log ?? (() => {});
  const client = new RegistryClient({ ...(o.client ?? {}), log });
  const source = (o.client?.baseUrl ?? OFFICIAL_REGISTRY).replace(/\/+$/, "");
  const store = new Store(o.out);
  const state = store.state(source);
  const full = o.full === true || !state.watermark;
  const overlap = o.overlapMs ?? 10 * 60_000;
  const updatedSince = full ? undefined : iso(new Date(Date.parse(state.watermark!) - overlap));
  const startedAt = iso(now());
  const report: PullReport = { mode: full ? "full" : "incremental", ...(updatedSince ? { updatedSince } : {}), pages: 0, requests: 0, received: 0, new: 0, changed: 0, skipped: 0, latestCorrected: 0, missingUpstream: 0, probed: 0, refused: 0, servers: 0 };

  // 1. Pull. The watermark moves only when every page arrived.
  const touched = new Set<string>();
  const seen = new Set<string>();
  const skipped: { name?: string; reason: string }[] = [];
  let maxUpdated = state.watermark ?? "";
  for await (const page of client.all({ ...(updatedSince ? { updatedSince } : { includeDeleted: true }), ...(o.maxPages !== undefined ? { maxPages: o.maxPages } : {}) })) {
    report.pages++;
    for (const s of page.skipped) skipped.push({ ...(s.name !== undefined ? { name: s.name } : {}), reason: s.reason });
    for (const r of page.records) {
      report.received++;
      seen.add(`${r.server.name}\u0000${r.server.version}`);
      let result: "new" | "changed" | "same";
      try {
        result = store.putVersion(r);
      } catch (e) {
        skipped.push({ name: r.server.name, reason: (e as Error).message });
        continue;
      }
      if (result !== "same") {
        report[result]++;
        touched.add(r.server.name);
      }
      const u = r._meta[OFFICIAL_KEY].updatedAt;
      if (Date.parse(u) > Date.parse(maxUpdated || "1970-01-01T00:00:00Z")) maxUpdated = u;
    }
  }
  report.requests = client.requests;
  report.skipped = skipped.length;
  store.write("skipped.json", { run: startedAt, skipped });

  // 2. Latest per server, and what a full pull no longer saw.
  const names = store.names();
  report.servers = names.length;
  const oldIndex = (store.read("index.json") as { servers?: Record<string, IndexLine> } | undefined)?.servers ?? {};
  const latest = new Map<string, ServerRecord>();
  const all: ServerRecord[] = [];
  const missing = new Set<string>();
  const versionCount = new Map<string, number>();
  for (const name of names) {
    const versions = store.versions(name);
    all.push(...versions);
    versionCount.set(name, versions.length);
    if (full && !versions.some((v) => seen.has(`${name}\u0000${v.server.version}`))) missing.add(name);
    const l = pickLatest(versions);
    if (l) latest.set(name, l);
    if (versions.filter((v) => v._meta[OFFICIAL_KEY].isLatest === true).length > 1) report.latestCorrected++;
  }
  report.missingUpstream = missing.size;

  // 3. Liveness, oldest first, within the budget. Only active latest remotes.
  const livenessFile = (store.read("liveness.json") as { urls?: Record<string, ProbeDetail> } | undefined)?.urls ?? {};
  const cannot = o.probe === false ? undefined : await canResolve(o.probeOptions?.lookup);
  if (cannot) report.probeSkipped = cannot;
  if (o.probe !== false && !cannot) {
    const maxAge = o.probeMaxAgeMs ?? 24 * 3600_000;
    const due = new Set<string>();
    const refused = new Set<string>();
    for (const r of latest.values()) {
      if (r._meta[OFFICIAL_KEY].status !== "active") continue;
      for (const url of remoteUrls(r.server)) {
        const last = livenessFile[url];
        if (!last || now().getTime() - Date.parse(last.checkedAt) >= maxAge) (refuseUrl(url) === undefined ? due : refused).add(url);
      }
    }
    // A URL the probe refuses costs no network call, so it takes no part of
    // the budget (box test w2, BW9: 182 of 200 went to them). `probe` itself
    // writes their detail, with no request.
    const refusedDetails = await Promise.all([...refused].sort().map((url) => probe(url, { ...(o.probeOptions ?? {}), now })));
    const order = [...due].sort((a, b) => (livenessFile[a]?.checkedAt ?? "").localeCompare(livenessFile[b]?.checkedAt ?? "") || a.localeCompare(b));
    const batch = order.slice(0, o.probeBudget ?? 2000);
    const results = await pool(batch, o.probeConcurrency ?? 8, (url) => probe(url, { ...(o.probeOptions ?? {}), now }));
    for (const d of refusedDetails) livenessFile[d.url] = d;
    report.refused = refusedDetails.length;
    if (machineLooksOffline(results)) {
      report.probeSkipped = `all ${results.length} probes failed at the network; not recorded`;
      if (refusedDetails.length) store.write("liveness.json", { urls: livenessFile });
    } else {
      for (const d of results) livenessFile[d.url] = d;
      report.probed = results.length;
      if (results.length || refusedDetails.length) store.write("liveness.json", { urls: livenessFile });
    }
  }

  // 4. Entries and flags.
  const flags = computeFlags([...latest.values()], { ...(o.cap !== undefined ? { cap: o.cap } : {}), all });
  const index: Record<string, IndexLine> = {};
  const bulkPublishers: Record<string, number> = {};
  for (const name of names) {
    const l = latest.get(name);
    const versions = versionCount.get(name) ?? 0;
    const urls = l ? remoteUrls(l.server) : [];
    const details = urls.map((u) => livenessFile[u]).filter((d): d is ProbeDetail => d !== undefined);
    const liveness: IndexLine["liveness"] = !l || urls.length === 0 ? "not-applicable" : combine(details);
    const checkedAt = details.map((d) => d.checkedAt).sort().at(-1);
    const status = l ? l._meta[OFFICIAL_KEY].status : "deleted";
    index[name] = {
      latest: l ? l.server.version : null,
      versions,
      status,
      liveness,
      flags: rowFlags(flags.get(name) ?? [], bulkPublishers),
      ...(missing.has(name) || (oldIndex[name]?.missingUpstream && !touched.has(name)) ? { missingUpstream: true as const } : {}),
    };
    if (l) store.putEntry(name, mirroredEntry(l, { state: liveness, ...(checkedAt && liveness !== "not-applicable" ? { checkedAt } : {}) }));
  }
  store.write("index.json", { source, servers: index, ...(Object.keys(bulkPublishers).length ? { bulkPublishers } : {}) });

  // 5. State.
  const counts = { servers: names.length, versions: all.length, latest: latest.size, deleted: 0, deprecated: 0 };
  for (const l of Object.values(index)) {
    if (l.status === "deleted") counts.deleted++;
    if (l.status === "deprecated") counts.deprecated++;
  }
  store.saveState({ version: 1, source, ...(maxUpdated ? { watermark: maxUpdated } : {}), lastRunAt: startedAt, ...(full ? { lastFullAt: startedAt } : state.lastFullAt ? { lastFullAt: state.lastFullAt } : {}), counts });
  if (maxUpdated) report.watermark = maxUpdated;
  return report;
}
