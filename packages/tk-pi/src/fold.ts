// Text folding for the reject list (contracts/policy.md, section 3,
// "Folding"; rules section 4, cases 30 and 33).
//
// Steps, per code point, after Unicode NFKD:
//   1. drop invisible and format characters (the contract's list);
//   2. drop non-spacing and enclosing combining marks (Mn, Me) [tk-pi
//      addition, see below];
//   3. map every Unicode space in the contract's list to one ASCII space;
//   4. map fullwidth ASCII (U+FF01 to U+FF5E) to ASCII;
//   5. map a confusable letter to its printable-ASCII look-alike (Unicode
//      TR39 confusables.txt 17.0.0, generated into confusables.ts);
//   6. lowercase.
//
// NFKD and step 2 go beyond the contract: without them `ANTHROPIC` with a
// combining mark after a letter, or a compatibility form such as a
// mathematical bold letter, would pass. Both only add matches, so a call can
// only move towards `deny`. Recorded as a request for integration (lane A
// status doc) so the Rust `fold_text` can do the same.
//
// Linear in the input: one pass, a Map lookup per code point (case 33 asks
// for 240,000 characters in linear time).

import { CONFUSABLES_TABLE } from "./confusables.ts";

let table: Map<number, string> | undefined;

function confusables(): Map<number, string> {
  if (table) return table;
  const map = new Map<number, string>();
  for (const line of CONFUSABLES_TABLE.split("\n")) {
    const tab = line.indexOf("\t");
    if (tab <= 0) continue;
    map.set(Number.parseInt(line.slice(0, tab), 16), line.slice(tab + 1));
  }
  table = map;
  return map;
}

function isInvisible(cp: number): boolean {
  return (
    cp === 0x00ad ||
    cp === 0x034f ||
    (cp >= 0x200b && cp <= 0x200f) ||
    (cp >= 0x2060 && cp <= 0x2064) ||
    cp === 0xfeff ||
    cp === 0x180e
  );
}

function isUnicodeSpace(cp: number): boolean {
  return (
    cp === 0x0085 ||
    cp === 0x00a0 ||
    cp === 0x1680 ||
    (cp >= 0x2000 && cp <= 0x200a) ||
    cp === 0x2028 ||
    cp === 0x2029 ||
    cp === 0x202f ||
    cp === 0x205f ||
    cp === 0x3000
  );
}

const COMBINING = /^[\p{Mn}\p{Me}]$/u;

/** Folds text before the reject list runs over it. */
export function foldText(text: string): string {
  const map = confusables();
  const out: string[] = [];
  for (const ch of text.normalize("NFKD")) {
    const cp = ch.codePointAt(0) ?? 0;
    if (cp < 0x80) {
      out.push(ch.toLowerCase());
      continue;
    }
    if (isInvisible(cp) || COMBINING.test(ch)) continue;
    if (isUnicodeSpace(cp)) {
      out.push(" ");
      continue;
    }
    if (cp >= 0xff01 && cp <= 0xff5e) {
      out.push(String.fromCharCode(cp - 0xff01 + 0x21).toLowerCase());
      continue;
    }
    let mapped = map.get(cp);
    if (mapped === undefined) {
      const lower = ch.toLowerCase();
      if (lower !== ch) {
        const lcp = lower.codePointAt(0) ?? 0;
        mapped = lower.length === String.fromCodePoint(lcp).length ? (map.get(lcp) ?? lower) : lower;
      }
    }
    out.push((mapped ?? ch).toLowerCase());
  }
  return out.join("");
}

/**
 * Decodes the escapes a config parser would decode, so a key written as
 * `\u0041NTHROPIC_BASE_URL` in a JSON file is seen as the key it becomes.
 * JSON and TOML read `\uXXXX`; TOML also reads `\UXXXXXXXX`. Applied before
 * folding. The same function as the hook core's (integration w1).
 */
export function decodeEscapes(text: string): string {
  return text
    .replace(/\\U([0-9a-fA-F]{8})/g, (m, hex: string) => {
      const cp = Number.parseInt(hex, 16);
      return cp <= 0x10ffff ? String.fromCodePoint(cp) : m;
    })
    .replace(/\\u([0-9a-fA-F]{4})/g, (_m, hex: string) => String.fromCharCode(Number.parseInt(hex, 16)));
}
