// Flags the mirror raises over the whole set (lane G, T3.2). Flags are
// records for the card and for review; the mirror deletes nothing for them.
//
//   duplicate-remote       two servers name the same remote host and path
//   duplicate-description  two servers from different publishers share a
//                          description once folded (note AP section 6: one
//                          account published 1,505 servers, one gateway 1,286
//                          endpoints)
//   bulk-publisher         a publisher first published more than `cap`
//                          servers on one UTC day
//   tk-key-removed         the publisher put an object under tk's own _meta key
//   reject-list            the server.json names a key of the reject list
//                          (contracts/policy.md section 3.1) or install-time
//                          execution, as lane E's validator reads them
import { fold, scanRejects } from "../validate/rejects.ts";
import { canonicalJson } from "./canonical.ts";
import { remoteUrls, stripTkKey } from "./entry.ts";
import type { ServerRecord } from "./registry.ts";
import { OFFICIAL_KEY } from "./registry.ts";

export type Flag =
  | { flag: "duplicate-remote"; of: string[]; key: string }
  | { flag: "duplicate-description"; of: string[] }
  | { flag: "bulk-publisher"; publisher: string; day: string; count: number; cap: number }
  | { flag: "tk-key-removed" }
  | { flag: "reject-list"; keys: string[] };

export const DEFAULT_CAP = 20;

export function publisherOf(name: string): string {
  return name.slice(0, name.indexOf("/")).toLowerCase();
}

/** A remote's identity for dedupe: lower-case host and path, no query, no
 *  trailing slash, the default port dropped. */
export function remoteKey(url: string): string | undefined {
  try {
    const u = new URL(url);
    const port = u.port && u.port !== "443" ? `:${u.port}` : "";
    return `${u.hostname.toLowerCase()}${port}${u.pathname.replace(/\/+$/, "").toLowerCase() || "/"}`;
  } catch {
    return undefined;
  }
}

/** A description folded the way the reject list folds text, punctuation and
 *  runs of space removed. Under 24 characters it is too short to mean a copy. */
export function descriptionKey(text: unknown): string | undefined {
  if (typeof text !== "string") return undefined;
  const k = fold(text)
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .trim();
  return k.length >= 24 ? k : undefined;
}

/** Flags for every latest version, by server name. */
/** `all` holds every stored version, for the per-day count (a publisher can
 *  flood by versions as well as by names); it defaults to `latest`. */
export function computeFlags(latest: ServerRecord[], opts: { cap?: number; all?: ServerRecord[] } = {}): Map<string, Flag[]> {
  const cap = opts.cap ?? DEFAULT_CAP;
  const out = new Map<string, Flag[]>();
  const add = (name: string, f: Flag) => out.set(name, [...(out.get(name) ?? []), f]);

  const byRemote = new Map<string, string[]>();
  const byDesc = new Map<string, string[]>();
  const byDay = new Map<string, string[]>();
  for (const r of latest) {
    const name = r.server.name;
    const { server, hadTkKey } = stripTkKey(r.server);
    if (hadTkKey) add(name, { flag: "tk-key-removed" });
    const hits = scanRejects(canonicalJson(server));
    if (hits.length) add(name, { flag: "reject-list", keys: [...new Set(hits.map((h) => h.key))].sort() });
    for (const url of remoteUrls(server)) {
      const k = remoteKey(url);
      if (k) byRemote.set(k, [...new Set([...(byRemote.get(k) ?? []), name])]);
    }
    const d = descriptionKey(server["description"]);
    if (d) byDesc.set(d, [...(byDesc.get(d) ?? []), name]);
  }
  const latestNames = new Set(latest.map((r) => r.server.name));
  for (const r of opts.all ?? latest) {
    const published = r._meta[OFFICIAL_KEY].publishedAt;
    if (typeof published !== "string" || !/^\d{4}-\d{2}-\d{2}/.test(published)) continue;
    const key = `${publisherOf(r.server.name)} ${published.slice(0, 10)}`;
    byDay.set(key, [...(byDay.get(key) ?? []), `${r.server.name}@${r.server.version}`]);
  }
  for (const [key, names] of byRemote) {
    if (names.length < 2) continue;
    for (const n of names) add(n, { flag: "duplicate-remote", key, of: names.filter((x) => x !== n).sort() });
  }
  for (const names of byDesc.values()) {
    // The same publisher describing two of its own servers alike is not a copy.
    if (new Set(names.map(publisherOf)).size < 2) continue;
    for (const n of names) add(n, { flag: "duplicate-description", of: names.filter((x) => publisherOf(x) !== publisherOf(n)).sort() });
  }
  for (const [key, versions] of byDay) {
    if (versions.length <= cap) continue;
    const [publisher, day] = key.split(" ") as [string, string];
    // Names never hold "@"; versions may.
    const names = new Set(versions.map((v) => v.slice(0, v.indexOf("@"))));
    for (const n of names) if (latestNames.has(n)) add(n, { flag: "bulk-publisher", publisher, day, count: versions.length, cap });
  }
  return out;
}
