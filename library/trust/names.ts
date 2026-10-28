// Lookalike, reserved and confusable names (toolkit plan T4.3; lane H).
//
// UpGuard (2 Jul 2026, note AP section 5) counted 3 to 15 unverified
// lookalikes per brand across four registries; the official registry has "no
// formal verified property". This module is the check `tk publish` (lane G,
// T3.3) and the library validator run on a name. It returns findings in the
// validator's own shape (library/validate/types.ts) and decides nothing else.
//
// Every comparison is on `skeleton()` (./fold.ts): the gate's own fold with
// the full Unicode 17.0.0 confusables table, then separators and a few
// multi-character look-alikes. An entry is exempt from a brand when its
// namespace owns the brand or it points only at the brand's own hosts, images
// or packages (lane E's rule, corner case 59).

import { domainToUnicode } from "node:url";
import type { Finding } from "../validate/types.ts";
import { finding } from "../validate/types.ts";
import { fold, hasNonAscii, skeleton } from "./fold.ts";

/** A brand whose name is held for its owner. `source` says where the record
 *  came from (a catalog commit, a directory page, lane E's seed). */
export type Brand = {
  name: string;
  namespaces: string[];
  hosts?: string[];
  images?: string[];
  packages?: string[];
  source?: string;
};

/** A name no entry may take unless it is its owner's (same rule as a brand).
 *  `prefix` also warns on ids that start with the name: vendor marks only,
 *  never generic words, so a mirror of 30,000 entries is not buried in notes. */
export type Reserved = Brand & { reason: string; prefix?: boolean };

/** What an entry points at, as the validator derives it (library/validate/index.ts). */
export type Identity = { namespace: string; hosts: string[]; images: string[]; packages: string[] };

export type NameInput = {
  /** The short id: the catalog folder, the last part of the manifest name. */
  id: string;
  /** The entry's `title`, when it has one. */
  title?: string;
  /** The entry's `description`, when it has one. */
  description?: string;
  identity: Identity;
};

/** Suffixes a squatter adds to a brand (lane E's list, plus `official` forms). */
const SQUAT_SUFFIX = /^(mcp|server|mcpserver|official|2|v2|remote|cloud|ai|app|api|tools|plugin)?$/;
const SQUAT_PREFIX = /^(official|real|the|mcp)?$/;

function hostMatches(host: string, brandHosts: string[]): boolean {
  const h = host.toLowerCase();
  return brandHosts.some((b) => h === b.toLowerCase() || h.endsWith("." + b.toLowerCase()));
}

function imageMatches(image: string, brandImages: string[]): boolean {
  return brandImages.some((b) => image === b || image.startsWith(b + ":") || image.startsWith(b + "@") || image.startsWith(b + "/"));
}

function packageMatches(pkg: string, brandPackages: string[]): boolean {
  // A package prefix must end at a scope or name boundary: `@shopify/` owns
  // `@shopify/dev-mcp`, `@shopify` does not own `@shopifyx/...`.
  return brandPackages.some((b) => pkg === b || (b.endsWith("/") && pkg.startsWith(b)) || pkg.startsWith(b + "@"));
}

/** The entry is the brand's own, or a pointer at nothing but the brand's services. */
export function ownedBy(identity: Identity, brand: Brand): boolean {
  if (brand.namespaces.some((n) => n.toLowerCase() === identity.namespace.toLowerCase())) return true;
  const targets = identity.hosts.length + identity.images.length + identity.packages.length;
  if (targets === 0) return false;
  return (
    identity.hosts.every((h) => hostMatches(h, brand.hosts ?? [])) &&
    identity.images.every((i) => imageMatches(i, brand.images ?? [])) &&
    identity.packages.every((p) => packageMatches(p, brand.packages ?? []))
  );
}

/** Levenshtein distance with an early exit above `max`. */
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

export type Likeness = { kind: "equal" | "distance" | "affix"; distance: number };

/** How a name's skeleton relates to a brand's. Distance 1 only for brands of
 *  five or more characters (lane E's floor: short names collide by chance). */
export function likeness(name: string, brand: string): Likeness | undefined {
  const s = skeleton(name);
  const b = skeleton(brand);
  if (s.length === 0 || b.length === 0) return undefined;
  if (s === b) return { kind: "equal", distance: 0 };
  if (b.length >= 5 && editDistance(s, b, 1) <= 1) return { kind: "distance", distance: 1 };
  const at = s.indexOf(b);
  if (at >= 0 && SQUAT_PREFIX.test(s.slice(0, at)) && SQUAT_SUFFIX.test(s.slice(at + b.length))) {
    return { kind: "affix", distance: s.length - b.length };
  }
  return undefined;
}

/**
 * Words that mix ASCII letters with characters that fold to ASCII letters:
 * `Еxa` with a Cyrillic `Е`, `pаypal` with a Cyrillic `а`. A word written
 * wholly in another script folds too, but does not mix, so a description in
 * Greek or Japanese is not flagged. A simplified form of UTS #39's mixed-script
 * detection (section 5.1), on the gate's fold (I: not the full Script_Extensions
 * resolution).
 */
export function mixedWords(text: string): string[] {
  const out: string[] = [];
  for (const word of text.split(/[\s,;:()"'`/]+/)) {
    if (word.length === 0 || !hasNonAscii(word)) continue;
    const hasAsciiLetter = /[A-Za-z]/.test(word);
    if (!hasAsciiLetter) continue;
    let foldsToLetter = false;
    for (const ch of word) {
      if (/[\x00-\x7f]/.test(ch)) continue;
      // Accented Latin letters (é, ü) fold by dropping a mark; they are ordinary
      // spelling, not a look-alike. Only a character that is not Latin and
      // still folds to an ASCII letter counts.
      if (/\p{Script=Latin}/u.test(ch)) continue;
      if (/^[a-z]+$/.test(fold(ch))) {
        foldsToLetter = true;
        break;
      }
    }
    if (foldsToLetter) out.push(word);
  }
  return out;
}

/** Labels of a domain namespace written in punycode (`xn--`), decoded. The
 *  namespace of a domain-verified entry is the domain reversed
 *  (`com.example`); a GitHub one is `io.github.<owner>`, ASCII by GitHub's rule. */
export function punycodeLabels(namespace: string): { label: string; unicode: string }[] {
  const labels = namespace.toLowerCase().split(".");
  const out: { label: string; unicode: string }[] = [];
  for (const label of labels) {
    if (!label.startsWith("xn--")) continue;
    const unicode = domainToUnicode(label);
    out.push({ label, unicode: unicode === "" ? label : unicode });
  }
  return out;
}

export type NameOptions = { brands: Brand[]; reserved: Reserved[] };

/** Runs every name rule on one entry. Rule names are stable: `trust/lookalike`,
 *  `trust/reserved`, `trust/reserved-prefix`, `trust/mixed-script`,
 *  `trust/punycode-namespace`. */
export function checkName(input: NameInput, opts: NameOptions): Finding[] {
  const out: Finding[] = [];
  const ns = input.identity.namespace.toLowerCase();
  const named: { path: string; value: string }[] = [{ path: "/name", value: input.id }];
  if (input.title !== undefined) named.push({ path: "/title", value: input.title });

  for (const brand of opts.brands) {
    if (ownedBy(input.identity, brand)) continue;
    for (const { path, value } of named) {
      const like = likeness(value, brand.name);
      if (!like) continue;
      out.push(
        finding(
          "trust/lookalike",
          "error",
          path,
          `'${value}' reads as the brand '${brand.name}' (${like.kind}, distance ${like.distance}) and the entry neither comes from that brand's namespace nor points only at its hosts, images or packages (UpGuard, note AP section 5; plan T4.3)`,
        ),
      );
      break;
    }
  }

  for (const r of opts.reserved) {
    if (ownedBy(input.identity, r)) continue;
    for (const { path, value } of named) {
      const like = likeness(value, r.name);
      if (like && like.kind !== "distance") {
        out.push(finding("trust/reserved", "error", path, `'${value}' takes the reserved name '${r.name}' (${r.reason})`));
        break;
      }
      if (r.prefix === true && path === "/name" && skeleton(value).startsWith(skeleton(r.name)) && skeleton(r.name).length >= 2) {
        out.push(finding("trust/reserved-prefix", "warning", path, `'${value}' starts with the reserved name '${r.name}' (${r.reason}); the entry is not from its owner, and the card says so`));
        break;
      }
    }
  }

  for (const { path, value } of [...named, ...(input.description !== undefined ? [{ path: "/description", value: input.description }] : [])]) {
    const words = mixedWords(value);
    if (words.length === 0) continue;
    // In a name or a title a mixed word is impersonation; in free text it can
    // be a quotation, so it is shown, not refused.
    const severity = path === "/description" ? "warning" : "error";
    out.push(finding("trust/mixed-script", severity, path, `${words.length} word(s) mix Latin letters with look-alike letters from another script (UTS #39 section 5.1): ${words.slice(0, 3).map((w) => JSON.stringify(w)).join(", ")}`));
  }

  for (const { label, unicode } of punycodeLabels(ns)) {
    const hit = [...opts.brands.map((b) => b.name), ...opts.reserved.map((r) => r.name)].find((n) => likeness(unicode, n));
    if (hit !== undefined) {
      out.push(finding("trust/punycode-namespace", "error", "/name", `the namespace label '${label}' decodes to '${unicode}', which reads as '${hit}'`));
    } else {
      out.push(finding("trust/punycode-namespace", "warning", "/name", `the namespace label '${label}' is punycode for '${unicode}'; shown so a reader sees the real characters`));
    }
  }
  return out;
}
