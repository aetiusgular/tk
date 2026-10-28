// What an entry points at, and the brand and reserved lists (lane H, T4.3).
//
// `identityOf` mirrors the validator's private helpers
// (library/validate/index.ts:317-355, lane E) so the name rules see the same
// namespace, hosts, images and packages the validator's lookalike rule does.
// A request for integration asks that lane E's be exported and this copy
// dropped; until then `identity.test.ts` holds the two to the same answers on
// the seed catalog through the validator's own lookalike outcome.

import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { Kind, LoadedEntry, TkMeta } from "../validate/types.ts";
import { getMeta, isRecord } from "../validate/types.ts";
import type { Brand, Identity, Reserved } from "./names.ts";

export const TRUST_DIR = dirname(fileURLToPath(import.meta.url));
export const LIBRARY_DIR = join(TRUST_DIR, "..");
export const CATALOG_DIR = join(LIBRARY_DIR, "catalog");

function namespaceOf(kind: Kind, data: Record<string, unknown>): string {
  if (kind === "server") return typeof data["name"] === "string" ? ((data["name"] as string).split("/")[0] ?? "") : "";
  if (kind === "skill") return typeof data["namespace"] === "string" ? (data["namespace"] as string) : "";
  const author = data["author"];
  return isRecord(author) && typeof author["name"] === "string" ? author["name"] : "";
}

/** Hostnames of every remote. `{placeholder}` parts are replaced before parsing, as the validator does. */
export function hostsOf(data: Record<string, unknown>): string[] {
  const hosts: string[] = [];
  const remotes = data["remotes"];
  if (!Array.isArray(remotes)) return hosts;
  for (const r of remotes) {
    if (!isRecord(r) || typeof r["url"] !== "string") continue;
    try {
      hosts.push(new URL(r["url"].replace(/\{[^}]+\}/g, "x")).hostname);
    } catch {
      // an invalid URL is the upstream rules' finding
    }
  }
  return hosts;
}

function imagesOf(data: Record<string, unknown>, meta: TkMeta | undefined): string[] {
  const images: string[] = [];
  if (meta?.runner.kind === "image") images.push(meta.runner.image);
  const packages = data["packages"];
  if (Array.isArray(packages)) for (const p of packages) if (isRecord(p) && p["registryType"] === "oci" && typeof p["identifier"] === "string") images.push(p["identifier"]);
  return images;
}

function packagesOf(data: Record<string, unknown>): string[] {
  const out: string[] = [];
  const packages = data["packages"];
  if (Array.isArray(packages)) for (const p of packages) if (isRecord(p) && p["registryType"] !== "oci" && typeof p["identifier"] === "string") out.push(p["identifier"]);
  return out;
}

export function identityOf(entry: Pick<LoadedEntry, "kind" | "data">): Identity {
  const meta = getMeta(entry.kind, entry.data);
  return { namespace: namespaceOf(entry.kind, entry.data), hosts: hostsOf(entry.data), images: imagesOf(entry.data, meta), packages: packagesOf(entry.data) };
}

function readList<T>(path: string, key: string): T[] {
  if (!existsSync(path)) return [];
  const raw: unknown = JSON.parse(readFileSync(path, "utf8"));
  if (!isRecord(raw) || !Array.isArray(raw[key])) throw new Error(`${path}: no '${key}' array`);
  return raw[key] as T[];
}

/** Two records of one brand name become one, owner lists joined: a brand's
 *  name is held for every owner any source names. */
export function mergeBrands(...lists: Brand[][]): Brand[] {
  const byName = new Map<string, Brand>();
  const join = (a: string[] | undefined, b: string[] | undefined) => [...new Set([...(a ?? []), ...(b ?? [])])];
  for (const list of lists) {
    for (const b of list) {
      const key = b.name.toLowerCase();
      const prev = byName.get(key);
      if (!prev) {
        byName.set(key, { ...b, namespaces: [...b.namespaces] });
        continue;
      }
      const merged: Brand = { name: prev.name, namespaces: join(prev.namespaces, b.namespaces), hosts: join(prev.hosts, b.hosts), images: join(prev.images, b.images), packages: join(prev.packages, b.packages) };
      const source = [prev.source, b.source].filter((s) => s !== undefined).join("; ");
      if (source !== "") merged.source = source;
      byName.set(key, merged);
    }
  }
  return [...byName.values()].sort((a, b) => a.name.localeCompare(b.name));
}

/** Lane E's seed list (library/catalog/brands.json) and every list lane H
 *  adds under library/trust/ (`brands*.json`). */
export function loadBrands(catalogDir = CATALOG_DIR, trustDir = TRUST_DIR, extra: string[] = ["brands.docker.json", "brands.directory.json"]): Brand[] {
  const lists: Brand[][] = [readList<Brand>(join(catalogDir, "brands.json"), "brands")];
  for (const f of extra) lists.push(readList<Brand>(join(trustDir, f), "brands"));
  return mergeBrands(...lists);
}

export function loadReserved(trustDir = TRUST_DIR): Reserved[] {
  return readList<Reserved>(join(trustDir, "reserved.json"), "reserved");
}
