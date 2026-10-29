// The mirror's files on disk (lane G, T3.2).
//
//   state.json                     source, watermark, last runs, counts
//   servers/<name>/<version>.json  each version as the registry served it
//   entries/<name>.json            tk's entry for the server's latest version
//   liveness.json                  the last probe of each remote URL
//   index.json                     one line per server: latest, status, flags
//   skipped.json                   records the last run refused, with why
//
// Every file is written in one deterministic form (keys sorted, two-space
// indent, a final newline), through a temporary file and a rename, so a
// commit of the folder diffs only what changed and a crash leaves either the
// old or the new file. Nothing is deleted: a version the registry stops
// returning is flagged, since upstream says data resets may occur.
import { existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { canonicalJson } from "./canonical.ts";
import { encodeSegment, entryPath, versionPath } from "./paths.ts";
import type { ServerRecord } from "./registry.ts";
import { checkRecord } from "./registry.ts";

export const MAX_RECORD_BYTES = 1024 * 1024;

export type MirrorState = {
  version: 1;
  source: string;
  /** The greatest `updatedAt` seen in a pull that finished. */
  watermark?: string;
  lastRunAt?: string;
  lastFullAt?: string;
  counts?: { servers: number; versions: number; latest: number; deleted: number; deprecated: number };
};

/** Sort keys recursively; arrays keep their order. */
export function sortKeys(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(sortKeys);
  if (v && typeof v === "object") {
    const o = v as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(o).sort()) out[k] = sortKeys(o[k]);
    return out;
  }
  return v;
}

export function stableText(v: unknown): string {
  return JSON.stringify(sortKeys(v), null, 2) + "\n";
}

export class Store {
  readonly root: string;

  constructor(root: string) {
    this.root = resolve(root);
    mkdirSync(this.root, { recursive: true });
    // The store is a git checkout CI commits; a link inside it could send a
    // write anywhere (rules section 5, item 3).
    if (lstatSync(this.root).isSymbolicLink()) throw new Error(`${this.root} is a symbolic link; the mirror writes only into a real folder`);
  }

  private abs(rel: string): string {
    const p = resolve(this.root, rel);
    if (p !== this.root && !p.startsWith(this.root + sep)) throw new Error(`path ${rel} leaves the store`);
    // No component between the root and the file may be a link.
    let cur = this.root;
    for (const part of relative(this.root, dirname(p)).split(sep).filter(Boolean)) {
      cur = join(cur, part);
      if (existsSync(cur) && lstatSync(cur).isSymbolicLink()) throw new Error(`${relative(this.root, cur)} is a symbolic link`);
    }
    if (existsSync(p) && lstatSync(p).isSymbolicLink()) throw new Error(`${rel} is a symbolic link`);
    return p;
  }

  read(rel: string): unknown {
    const p = this.abs(rel);
    if (!existsSync(p)) return undefined;
    const st = lstatSync(p);
    if (!st.isFile() || st.size > 64 * 1024 * 1024) throw new Error(`${rel} is not a regular file under 64 MB`);
    return JSON.parse(readFileSync(p, "utf8")) as unknown;
  }

  /** Writes `value` when its stable text differs from the file's. Returns
   *  whether the file changed. */
  write(rel: string, value: unknown): boolean {
    const p = this.abs(rel);
    const text = stableText(value);
    if (existsSync(p) && readFileSync(p, "utf8") === text) return false;
    mkdirSync(dirname(p), { recursive: true });
    const tmp = `${p}.tmp-${process.pid}`;
    writeFileSync(tmp, text, { flag: "wx", mode: 0o644 });
    renameSync(tmp, p);
    return true;
  }

  state(source: string): MirrorState {
    const s = this.read("state.json") as MirrorState | undefined;
    if (s && s.version === 1) {
      if (s.source !== source) throw new Error(`the store at ${this.root} mirrors ${s.source}, not ${source}`);
      return s;
    }
    return { version: 1, source };
  }

  saveState(s: MirrorState): void {
    this.write("state.json", s);
  }

  /** Stores one version. Returns "new", "changed" or "same". */
  putVersion(r: ServerRecord): "new" | "changed" | "same" {
    const rel = versionPath(r.server.name, r.server.version);
    const before = this.read(rel);
    if (Buffer.byteLength(canonicalJson(r)) > MAX_RECORD_BYTES) throw new Error(`${r.server.name}@${r.server.version} is over ${MAX_RECORD_BYTES} bytes`);
    const changed = this.write(rel, r);
    return before === undefined ? "new" : changed ? "changed" : "same";
  }

  /** Every stored version of one server. */
  versions(name: string): ServerRecord[] {
    const dir = this.abs(`servers/${encodeSegment(name)}`);
    if (!existsSync(dir)) return [];
    return this.recordsIn(dir).filter((r) => r.server.name === name);
  }

  /** Every server name in the store, from the files themselves. */
  names(): string[] {
    const dir = this.abs("servers");
    if (!existsSync(dir)) return [];
    const out = new Set<string>();
    for (const d of readdirSync(dir).sort()) {
      const full = join(dir, d);
      if (!lstatSync(full).isDirectory()) continue;
      const first = this.recordsIn(full)[0];
      if (first) out.add(first.server.name);
    }
    return [...out].sort();
  }

  private recordsIn(dir: string): ServerRecord[] {
    const out: ServerRecord[] = [];
    for (const f of readdirSync(dir).sort()) {
      if (!f.endsWith(".json")) continue;
      const full = join(dir, f);
      const st = lstatSync(full);
      if (!st.isFile() || st.size > MAX_RECORD_BYTES * 2) continue;
      const r = JSON.parse(readFileSync(full, "utf8")) as unknown;
      if (checkRecord(r) === undefined) out.push(r as ServerRecord);
    }
    return out;
  }

  putEntry(name: string, entry: unknown): boolean {
    return this.write(entryPath(name), entry);
  }
}
