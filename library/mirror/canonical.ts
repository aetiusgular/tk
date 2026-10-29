// Canonical JSON and content hashes for the mirror and the publish flow
// (lane G, T3.2 and T3.3).
//
// RFC 8785 (JCS): object keys sorted by UTF-16 code units, no whitespace,
// strings and numbers serialized as ECMAScript's JSON.stringify does
// (RFC 8785 sections 3.2.2.3 and 3.2.3). JavaScript's default sort compares
// UTF-16 code units, so `Object.keys(o).sort()` is JCS order.
import { createHash } from "node:crypto";

export function canonicalJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "string") return JSON.stringify(value);
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new Error("canonical JSON has no form for a non-finite number");
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map((v) => canonicalJson(v === undefined ? null : v)).join(",")}]`;
  if (typeof value === "object") {
    const o = value as Record<string, unknown>;
    const keys = Object.keys(o)
      .filter((k) => o[k] !== undefined)
      .sort();
    return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalJson(o[k])}`).join(",")}}`;
  }
  throw new Error(`canonical JSON has no form for a ${typeof value}`);
}

export function sha256Hex(data: string | Uint8Array): string {
  return createHash("sha256").update(data).digest("hex");
}

/** The length in bytes of `value` as Go's `encoding/json.Marshal` writes it:
 *  compact, with `<`, `>`, `&`, U+2028 and U+2029 escaped to six bytes each.
 *  The official registry measures its 4 KB publisher `_meta` cap this way
 *  (`internal/validators/validators.go:727-737` at 9c4a96c). Key order does
 *  not change the length. */
export function goJsonLength(value: unknown): number {
  const compact = JSON.stringify(value);
  let extra = 0;
  for (const ch of compact) {
    if (ch === "<" || ch === ">" || ch === "&") extra += 5; // 1 byte becomes < (6)
    else if (ch === " " || ch === " ") extra += 3; // 3 UTF-8 bytes become 6
  }
  // Control characters: both write \n, \r, \t and \u00XX. JavaScript writes
  // \b and \f; Go before 1.22 wrote \u0008 and \u000c. Counted at the longer
  // form, so the estimate errs over the cap, never under it.
  const walk = (v: unknown): void => {
    if (typeof v === "string") {
      for (const ch of v) if (ch === "\b" || ch === "\f") extra += 4;
    } else if (Array.isArray(v)) v.forEach(walk);
    else if (v !== null && typeof v === "object")
      for (const [k, x] of Object.entries(v)) {
        walk(k);
        walk(x);
      }
  };
  walk(value);
  return Buffer.byteLength(compact, "utf8") + extra;
}
