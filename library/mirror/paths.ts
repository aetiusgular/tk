// File names for mirrored servers (lane G, T3.2).
//
// Upstream names are `<namespace>/<name>` with mixed case allowed
// (`internal/validators/validators.go:19-25` at 9c4a96c), and versions are
// free text up to 255 characters. One folder per name and one file per
// version, each segment percent-encoded so that:
//   - `/`, `.`, `..`, NUL and every other byte outside [a-z0-9._-] are encoded,
//     so no segment can leave the store;
//   - upper-case letters are encoded too (`A` is `%41`), so two names that
//     differ only in case never share a path on a case-insensitive disk
//     (APFS on the Mac by default);
//   - a segment never starts with `.`, so `..` and hidden files cannot occur.
// The mapping is one to one; it is reversible except for the hashed form.
import { createHash } from "node:crypto";

const PLAIN = /^[a-z0-9_-]$/;

export function encodeSegment(text: string): string {
  if (text.length === 0) throw new Error("empty path segment");
  let out = "";
  const bytes = Buffer.from(text, "utf8");
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i]!;
    const ch = String.fromCharCode(b);
    // A dot is kept inside a segment, never first (so `.`, `..` and `.git` are encoded).
    if (b < 0x80 && (PLAIN.test(ch) || (ch === "." && i > 0))) out += ch;
    else out += "%" + b.toString(16).toUpperCase().padStart(2, "0");
  }
  // Most file systems cap a name at 255 bytes. A longer encoding becomes `~`
  // and the SHA-256 of the text: `~` is never in an encoded segment, so the
  // two forms cannot collide; the file itself holds the real name.
  if (out.length > 240) return "~" + createHash("sha256").update(text, "utf8").digest("hex");
  return out;
}

export function decodeSegment(segment: string): string | undefined {
  if (segment.startsWith("~")) return undefined; // hashed: read the name from the file
  if (!/^(?:[a-z0-9._-]|%[0-9A-F]{2})+$/.test(segment)) throw new Error(`not an encoded segment: ${JSON.stringify(segment)}`);
  return Buffer.from(segment.replace(/%([0-9A-F]{2})/g, (_, h: string) => String.fromCharCode(parseInt(h, 16))), "latin1").toString("utf8");
}

/** `servers/<encoded name>/<encoded version>.json`, relative to the store root. */
export function versionPath(name: string, version: string): string {
  return `servers/${encodeSegment(name)}/${encodeSegment(version)}.json`;
}

/** `entries/<encoded name>.json`: the tk entry for the latest version. */
export function entryPath(name: string): string {
  return `entries/${encodeSegment(name)}.json`;
}
