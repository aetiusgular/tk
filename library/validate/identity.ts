// What an entry points at: its namespace, remote hosts, images and other
// packages. Lane E wrote these inside library/validate/index.ts; lane H's
// library/trust/identity.ts copied them. Integration w2 moved them here so
// the validator and the trust rules read one copy (lane H's request).

import type { Kind, TkMeta } from "./types.ts";
import { isRecord } from "./types.ts";

export function namespaceOf(kind: Kind, data: Record<string, unknown>): string {
  if (kind === "server") return typeof data["name"] === "string" ? ((data["name"] as string).split("/")[0] ?? "") : "";
  if (kind === "skill") return typeof data["namespace"] === "string" ? (data["namespace"] as string) : "";
  const author = data["author"];
  return isRecord(author) && typeof author["name"] === "string" ? author["name"] : "";
}

/** Hostnames of every remote; `{placeholder}` parts are replaced before parsing. */
export function hostsOf(data: Record<string, unknown>): string[] {
  const hosts: string[] = [];
  const remotes = data["remotes"];
  if (!Array.isArray(remotes)) return hosts;
  for (const r of remotes) {
    if (!isRecord(r) || typeof r["url"] !== "string") continue;
    try {
      hosts.push(new URL(r["url"].replace(/\{[^}]+\}/g, "x")).hostname);
    } catch {
      // an invalid URL is reported by the upstream rules
    }
  }
  return hosts;
}

export function imagesOf(data: Record<string, unknown>, meta: TkMeta | undefined): string[] {
  const images: string[] = [];
  if (meta?.runner.kind === "image") images.push(meta.runner.image);
  const packages = data["packages"];
  if (Array.isArray(packages)) for (const p of packages) if (isRecord(p) && p["registryType"] === "oci" && typeof p["identifier"] === "string") images.push(p["identifier"]);
  return images;
}

/** Package identifiers that are not images (npm, pypi, nuget, mcpb, host commands). */
export function packagesOf(data: Record<string, unknown>): string[] {
  const out: string[] = [];
  const packages = data["packages"];
  if (Array.isArray(packages)) for (const p of packages) if (isRecord(p) && p["registryType"] !== "oci" && typeof p["identifier"] === "string") out.push(p["identifier"]);
  return out;
}
