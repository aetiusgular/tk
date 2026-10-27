// Lookalike names against a seed brand list (lane E, T3.1; plan T4.3 builds
// the full list and the confusable table in wave 2, lane H).
//
// UpGuard (2 Jul 2026, note AP section 5) counted 3 to 15 unverified
// lookalikes per brand across four registries. The library's check at this
// step: an entry's short id that equals a brand after folding, or sits within
// edit distance 1 of a brand of at least 5 characters, is refused unless the
// entry's namespace is listed as that brand's owner in brands.json.
import { fold } from "./rejects.ts";

/** A brand: who owns the name (publisher namespaces), which hosts serve its
 *  remote endpoint, and which image prefixes are its builds. An entry that
 *  points only at the brand's own hosts or images is a pointer to the brand's
 *  service, not a lookalike, whoever published the pointer (corner case 59:
 *  the reverse-DNS key says which library the pointer came from). */
export type Brand = { name: string; namespaces: string[]; hosts?: string[]; images?: string[]; packages?: string[] };

export type EntryIdentity = { namespace: string; hosts: string[]; images: string[]; packages: string[] };

function ownedByBrand(entry: EntryIdentity, brand: Brand): boolean {
  if (brand.namespaces.includes(entry.namespace)) return true;
  const hosts = brand.hosts ?? [];
  const images = brand.images ?? [];
  const packages = brand.packages ?? [];
  const targets = [...entry.hosts, ...entry.images, ...entry.packages];
  if (targets.length === 0) return false;
  return (
    entry.hosts.every((h) => hosts.some((b) => h === b || h.endsWith("." + b))) &&
    entry.images.every((i) => images.some((b) => i === b || i.startsWith(b + ":") || i.startsWith(b + "@") || i.startsWith(b + "/"))) &&
    entry.packages.every((p) => packages.some((b) => p === b || p.startsWith(b)))
  );
}

/** Levenshtein distance, early-exit above `max`. */
export function editDistance(a: string, b: string, max = Infinity): number {
  if (a === b) return 0;
  if (Math.abs(a.length - b.length) > max) return max + 1;
  let prev = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    const cur = [i];
    let rowMin = i;
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      const v = Math.min((prev[j] ?? 0) + 1, (cur[j - 1] ?? 0) + 1, (prev[j - 1] ?? 0) + cost);
      cur.push(v);
      if (v < rowMin) rowMin = v;
    }
    if (rowMin > max) return max + 1;
    prev = cur;
  }
  return prev[b.length] ?? 0;
}

/** Strip the separators and digit look-alikes a squatter varies. */
function skeleton(name: string): string {
  return fold(name).replace(/[-_.\s]/g, "").replace(/0/g, "o").replace(/1/g, "l").replace(/rn/g, "m").replace(/vv/g, "w");
}

export type LookalikeHit = { brand: string; distance: number };

/** Returns the brand an id looks like, or undefined. `namespace` is the
 *  entry's namespace (`io.github.<owner>` or a domain); a brand's own
 *  namespaces are exempt. */
export function lookalike(id: string, entry: EntryIdentity, brands: Brand[]): LookalikeHit | undefined {
  const s = skeleton(id);
  for (const brand of brands) {
    if (ownedByBrand(entry, brand)) continue;
    const b = skeleton(brand.name);
    if (s === b) return { brand: brand.name, distance: 0 };
    if (b.length >= 5 && editDistance(s, b, 1) <= 1) return { brand: brand.name, distance: 1 };
    // "<brand>-mcp", "<brand>-server", "<brand>2": the brand with a suffix the
    // namespace does not own.
    if (s.startsWith(b) && /^(mcp|server|official|2|v2|remote|cloud|ai)?$/.test(s.slice(b.length)) && s !== b) {
      return { brand: brand.name, distance: s.length - b.length };
    }
  }
  return undefined;
}
