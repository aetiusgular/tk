// The trust record store (toolkit plan T4.1, T4.2; lane H).
//
// Layout, git-tracked so history is the audit trail (plan T3.2: "the catalog
// as git history"):
//
//   library/trust/records/<entry-id>/history.json         versions in the order recorded
//   library/trust/records/<entry-id>/<version>/manifest.json
//   library/trust/records/<entry-id>/<version>/scans.json  the contract's trust.scans[] items, appended
//   library/trust/records/<entry-id>/<version>/scans/<date>-<scanner>.sarif.json
//
// A version is immutable once recorded (upstream: "Versions are immutable",
// plan T3.3): a second, different manifest under the same version is refused
// and reported, because a server that republishes a version with other tools
// is the rug pull a pin exists to catch. Writes go through a temporary file
// and a rename inside the record folder; no path component may be a link,
// and every name is checked before it becomes a path (rules section 5, item 3).

import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import { join, sep } from "node:path";
import { TRUST_DIR } from "./identity.ts";
import type { Manifest } from "./manifest.ts";
import { canonicalJson, sha256Hex } from "./manifest.ts";

export const RECORDS_DIR = join(TRUST_DIR, "records");

/** The contract's `trust.scans[]` item (contracts/entry.schema.json). */
export type ScanRecord = { scanner: string; scannerVersion: string; date: string; findings: number; sarifSha256?: string };

const ID = /^[a-z0-9][a-z0-9-]{0,63}$/;
// Upstream versions are free strings up to 255 characters; the store takes the
// path-safe subset and refuses the rest with a reason.
const VERSION = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$/;
const SCANNER = /^[a-z0-9][a-z0-9-]{0,63}$/;
export const DATE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/;
/** SARIF logs are capped: a scanner's output is data, not a reason to fill the disk. */
export const MAX_SARIF_BYTES = 8 * 1024 * 1024;

export function checkId(id: string): string {
  if (!ID.test(id)) throw new Error(`not an entry id: ${JSON.stringify(id)}`);
  return id;
}

export function checkVersion(version: string): string {
  if (!VERSION.test(version) || version.includes("..")) throw new Error(`version ${JSON.stringify(version)} is not one the record store can name a folder after ([A-Za-z0-9._+-], no '..')`);
  return version;
}

export function checkScanner(name: string): string {
  if (!SCANNER.test(name)) throw new Error(`not a scanner id: ${JSON.stringify(name)}`);
  return name;
}

export class RecordStore {
  constructor(readonly root: string = RECORDS_DIR) {}

  /** Refuses a link anywhere between the root and the path, and the root itself as a link. */
  private guard(path: string): void {
    if (!(path === this.root || path.startsWith(this.root + sep))) throw new Error(`${path} is outside the record store`);
    const parts = path.slice(this.root.length).split(sep).filter((p) => p !== "");
    let at = this.root;
    for (const p of ["", ...parts]) {
      if (p !== "") at = join(at, p);
      if (!existsSync(at)) return;
      if (lstatSync(at).isSymbolicLink()) throw new Error(`${at} is a link; the record store follows none`);
    }
  }

  private dir(...segments: string[]): string {
    const path = join(this.root, ...segments);
    this.guard(path);
    mkdirSync(path, { recursive: true, mode: 0o755 });
    this.guard(path);
    return path;
  }

  private write(path: string, text: string): void {
    this.guard(path);
    const tmp = `${path}.${process.pid}.${Date.now()}.tmp`;
    writeFileSync(tmp, text, { flag: "wx", mode: 0o644 });
    renameSync(tmp, path);
  }

  private read(path: string, max = 2 * 1024 * 1024): string | undefined {
    this.guard(path);
    if (!existsSync(path)) return undefined;
    const st = lstatSync(path);
    if (!st.isFile()) throw new Error(`${path} is not a plain file`);
    if (st.size > max) throw new Error(`${path} is ${st.size} bytes, over ${max}`);
    return readFileSync(path, "utf8");
  }

  versions(id: string): string[] {
    const text = this.read(join(this.root, checkId(id), "history.json"));
    if (text === undefined) return [];
    const h: unknown = JSON.parse(text);
    if (!Array.isArray(h) || !h.every((v) => typeof v === "string")) throw new Error(`${id}/history.json is not a list of versions`);
    return h;
  }

  manifest(id: string, version: string): Manifest | undefined {
    const text = this.read(join(this.root, checkId(id), checkVersion(version), "manifest.json"));
    return text === undefined ? undefined : (JSON.parse(text) as Manifest);
  }

  /**
   * Records a manifest. Returns `new` the first time, `same` when the version
   * already holds an equal manifest, and `reused` (writing nothing) when the
   * version holds a different one.
   */
  putManifest(id: string, manifest: Manifest): "new" | "same" | "reused" {
    checkId(id);
    const version = checkVersion(manifest.serverVersion);
    const existing = this.manifest(id, version);
    if (existing) return existing.manifestSha256 === manifest.manifestSha256 ? "same" : "reused";
    const dir = this.dir(id, version);
    this.write(join(dir, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
    const history = this.versions(id);
    if (!history.includes(version)) this.write(join(this.dir(id), "history.json"), JSON.stringify([...history, version], null, 2) + "\n");
    return "new";
  }

  scans(id: string, version: string): ScanRecord[] {
    const text = this.read(join(this.root, checkId(id), checkVersion(version), "scans.json"));
    return text === undefined ? [] : (JSON.parse(text) as ScanRecord[]);
  }

  /** Stores a SARIF log and appends its record. The record's hash is of the
   *  bytes stored, so `sarifSha256` names the file a reader can open. */
  putScan(id: string, version: string, record: Omit<ScanRecord, "sarifSha256">, sarifText: string | undefined): ScanRecord {
    checkId(id);
    checkVersion(version);
    checkScanner(record.scanner);
    if (!DATE.test(record.date)) throw new Error(`scan date ${JSON.stringify(record.date)} is not YYYY-MM-DDTHH:MM:SSZ`);
    if (!Number.isInteger(record.findings) || record.findings < 0) throw new Error("findings must be a count");
    const out: ScanRecord = { ...record };
    if (sarifText !== undefined) {
      if (Buffer.byteLength(sarifText, "utf8") > MAX_SARIF_BYTES) throw new Error(`SARIF log over ${MAX_SARIF_BYTES} bytes`);
      out.sarifSha256 = sha256Hex(sarifText);
      const name = `${record.date.replace(/:/g, "")}-${record.scanner}.sarif.json`;
      const path = join(this.dir(id, version, "scans"), name);
      if (existsSync(path)) throw new Error(`${name} exists; a scan is recorded once`);
      this.write(path, sarifText);
    }
    const list = this.scans(id, version);
    this.write(join(this.dir(id, version), "scans.json"), JSON.stringify([...list, out], null, 2) + "\n");
    return out;
  }

  /** Every entry id with records, sorted. */
  ids(): string[] {
    if (!existsSync(this.root)) return [];
    this.guard(this.root);
    return readdirSync(this.root).filter((n) => ID.test(n)).sort();
  }
}

/** A stable digest of a scan list, for tests and cards. */
export function scansDigest(scans: ScanRecord[]): string {
  return sha256Hex(canonicalJson(scans));
}
