// The hook core's fold against the shared fixture (contracts/fixtures/policy/fold.json),
// which the Rust fold (crates/tk-core/tests/fold_parity.rs) and tk-pi also test against.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { fold } from "./fold.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const doc = JSON.parse(readFileSync(path.join(here, "..", "..", "..", "..", "contracts", "fixtures", "policy", "fold.json"), "utf8")) as { unicode: string; cases: [string, string][] };

// The fold follows the running Node's Unicode data (NFKD, Mn and Me,
// lowercase). The fixture is from one version; on another, the test says so
// and skips rather than report a difference that is only the runtime's.
const sameUnicode = process.versions.unicode === doc.unicode;

describe("fold parity", () => {
  it.skipIf(!sameUnicode)(`equals the shared fixture on every code point whose fold holds ASCII (Unicode ${doc.unicode}; this Node has ${process.versions.unicode})`, () => {
    expect(doc.cases.length).toBeGreaterThan(3000);
    const bad = doc.cases.filter(([cp, want]) => fold(String.fromCodePoint(parseInt(cp, 16))) !== want);
    expect(bad).toEqual([]);
  });
});
