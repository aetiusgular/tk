// Text folding before the reject list runs (contracts/policy.md, section 3;
// rules cases 30 and 33). Canonical copy: packages/tk-claude-plugin/src/core/.
// The Codex and OpenCode plugins carry byte-identical copies (scripts/sync-core.mjs).

import { CONFUSABLES } from "./confusables.ts";

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

function mapOne(ch: string): string {
  const cp = ch.codePointAt(0) ?? 0;
  if (cp < 0x80) return ch;
  if (isInvisible(cp)) return "";
  if (isUnicodeSpace(cp)) return " ";
  if (cp >= 0xff01 && cp <= 0xff5e) return String.fromCharCode(cp - 0xff01 + 0x21);
  const look = CONFUSABLES.get(cp);
  return look ?? ch;
}

const COMBINING_MARK = /[\p{Mn}\p{Me}]/gu;

/**
 * NFKD, then combining marks (Mn, Me) dropped, as lane A does; then per
 * character: lowercase; drop invisible and format characters; map Unicode
 * spaces to one ASCII space; fullwidth ASCII to ASCII; confusables to their
 * ASCII look-alike (UTS #39 confusables.txt 17.0.0, ASCII targets only). The
 * confusable map runs before and after lowercasing, so both an uppercase
 * Cyrillic letter and its lowercase form land on ASCII. Linear in the text.
 */
export function fold(text: string): string {
  let out = "";
  for (const ch of text.normalize("NFKD").replace(COMBINING_MARK, "")) {
    const first = mapOne(ch);
    if (first === "") continue;
    for (const lower of first.toLowerCase()) out += mapOne(lower);
  }
  return out;
}

/**
 * Decodes the escapes a config parser would decode, so a key written as
 * `ANTHROPIC_BASE_URL` in a JSON file is seen as the key it becomes.
 * JSON and TOML both read `\uXXXX`; TOML also reads `\UXXXXXXXX`. Applied
 * before fold. Stricter than the contract, which folds only; recorded in
 * plans/parallel/tk_lane_F.md, Decisions.
 */
export function decodeEscapes(text: string): string {
  return text
    .replace(/\\U([0-9a-fA-F]{8})/g, (m, hex: string) => {
      const cp = Number.parseInt(hex, 16);
      return cp <= 0x10ffff ? String.fromCodePoint(cp) : m;
    })
    .replace(/\\u([0-9a-fA-F]{4})/g, (_m, hex: string) => String.fromCharCode(Number.parseInt(hex, 16)));
}
