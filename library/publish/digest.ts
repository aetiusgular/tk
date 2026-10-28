// The content hash of a published entry (lane G, T3.3).
//
// contentSha256 is the SHA-256 of the RFC 8785 form of what the publisher
// wrote: the manifest with tk's `trust` record taken out (it holds the hash
// itself, and scans and liveness that change without the content changing),
// and the SHA-256 of every other file in the entry's folder, by path. For a
// skill whose SKILL.md lives elsewhere in the repository, that text too.
// A mirrored entry's hash is the same idea for the upstream server object
// (library/mirror/entry.ts).
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { canonicalJson, sha256Hex } from "../mirror/canonical.ts";
import type { LoadedEntry } from "../validate/types.ts";
import { PUBLISHER_PROVIDED, TK_META_KEY, isRecord } from "../validate/types.ts";

export const PROOF_FILE = "publish-proof.json";
export const MAX_FILES = 500;
export const MAX_FILE_BYTES = 2 * 1024 * 1024;

/** The manifest with tk's trust record removed. */
export function withoutTrust(kind: LoadedEntry["kind"], data: Record<string, unknown>): Record<string, unknown> {
  const copy = structuredClone(data);
  const holder = kind === "bundle" ? copy["metadata"] : isRecord(copy["_meta"]) ? (copy["_meta"] as Record<string, unknown>)[PUBLISHER_PROVIDED] : undefined;
  if (isRecord(holder) && isRecord(holder[TK_META_KEY])) delete (holder[TK_META_KEY] as Record<string, unknown>)["trust"];
  return copy;
}

/** Every regular file under `dir` but the manifest and the proof, sorted.
 *  A link or a special file is refused: the hash must cover what a clone holds. */
export function entryFiles(dir: string, manifest: string): { files: Record<string, string>; errors: string[] } {
  const files: Record<string, string> = {};
  const errors: string[] = [];
  let count = 0;
  const walk = (d: string, depth: number) => {
    if (depth > 8) return void errors.push(`${relative(dir, d)} is nested deeper than 8 folders`);
    for (const name of readdirSync(d).sort()) {
      const full = join(d, name);
      const rel = relative(dir, full).split(sep).join("/");
      const st = lstatSync(full);
      if (st.isSymbolicLink()) errors.push(`${rel} is a symbolic link`);
      else if (st.isDirectory()) walk(full, depth + 1);
      else if (!st.isFile()) errors.push(`${rel} is not a regular file`);
      else if (rel === manifest || rel === PROOF_FILE) continue;
      else if (st.size > MAX_FILE_BYTES) errors.push(`${rel} is over ${MAX_FILE_BYTES} bytes`);
      else if (++count > MAX_FILES) return void errors.push(`more than ${MAX_FILES} files`);
      else files[rel] = sha256Hex(readFileSync(full));
    }
  };
  walk(dir, 0);
  return { files, errors };
}

export function entryDigest(entry: LoadedEntry): { contentSha256: string; files: Record<string, string>; errors: string[] } {
  const { files, errors } = entryFiles(entry.dir, entry.file);
  const doc: Record<string, unknown> = { manifest: withoutTrust(entry.kind, entry.data), files };
  if (entry.skillMd) doc["skillMd"] = { path: entry.skillMd.path, sha256: sha256Hex(entry.skillMd.text) };
  return { contentSha256: sha256Hex(canonicalJson(doc)), files, errors };
}
