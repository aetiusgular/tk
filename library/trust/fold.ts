// One fold for the library (toolkit plan T4.3; lane H).
//
// The gate's matchers fold text with the hook core's algorithm and the full
// Unicode confusables.txt 17.0.0 table (contracts/policy.md section 3;
// packages/tk-claude-plugin/src/core/fold.ts, confusables.ts). Wave 1's
// library validator carried a hand-picked subset of that table
// (library/validate/rejects.ts, lane E's open item). The library now uses the
// same function the gate uses, imported from the canonical core rather than
// copied a fourth time, so a name or key the gate folds one way is folded the
// same way at publish.

import { fold as coreFold } from "../../packages/tk-claude-plugin/src/core/fold.ts";

export { coreFold as fold };

/** Separators a squatter varies (`exa-mcp`, `exa_mcp`, `exa.mcp`, `exa mcp`). */
const SEPARATORS = /[-_.\s]/g;

/**
 * Multi-character look-alikes that confusables.txt does not hold, because its
 * entries map one source code point to a target. UTS #39 section 4 defines the
 * skeleton on single characters; these sequences are the ones the lookalike
 * check of lane E already used and UpGuard's examples show (note AP section 5).
 * Applied after folding, longest first.
 */
const SEQUENCES: [RegExp, string][] = [
  [/rn/g, "m"],
  [/vv/g, "w"],
  [/cl/g, "d"],
];

/**
 * Letters lane E's subset mapped that the gate's table does not: in
 * confusables.txt 17.0.0 their targets hold a non-ASCII character (a stroke
 * or a dotless form), and the hook core keeps ASCII targets only. Kept for
 * names so moving the library to the gate's fold loses none of lane E's
 * cases (run 28 Oct; `fold.test.ts`). Names only: the reject list matches
 * keys a config parser reads literally, where these are other keys.
 */
const SUPPLEMENT: Record<string, string> = { "κ": "k", "ȷ": "j", "ð": "d" };

/** ASCII digits that read as letters in a name: lane E's two. `3`, `5` and `7`
 *  were weighed and left out, because real brands carry digits (`context7`)
 *  and a wider map makes unrelated names collide. */
const DIGITS: Record<string, string> = { "0": "o", "1": "l" };

/**
 * The skeleton two names are compared by. UTS #39's skeleton (section 4) is
 * NFD, map every character through confusables.txt, NFD again; the gate's fold
 * already does NFKD, drops combining marks, lowercases and maps confusables to
 * ASCII, so the skeleton here is that fold, then separators removed, digit and
 * sequence look-alikes mapped. Both names in a comparison go through it.
 */
export function skeleton(name: string): string {
  let s = "";
  for (const ch of coreFold(name)) s += SUPPLEMENT[ch] ?? ch;
  s = s.replace(SEPARATORS, "");
  s = s.replace(/[0-9]/g, (d) => DIGITS[d] ?? d);
  for (const [re, to] of SEQUENCES) s = s.replace(re, to);
  return s;
}

/** True when the text holds a character outside printable ASCII. Names in the
 *  library are ASCII by rule (`SERVER_ID`); a title, a namespace label or a
 *  description that is not is worth a second look. */
export function hasNonAscii(text: string): boolean {
  return /[^\x20-\x7e]/.test(text);
}
