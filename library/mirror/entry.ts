// The tk entry for a mirrored server (lane G, T3.2).
//
// The upstream server.json is kept as the publisher wrote it, with two
// changes: the registry's own `_meta[official]` stays outside (it is the
// registry's, not the publisher's), and any object a publisher put under
// tk's own key (`_meta[publisher-provided]["io.github.aetiusgular.tk"]`) is
// removed before tk writes its record there: a stranger could otherwise
// claim `tier: own` or a trust record. contracts/entry.schema.json is the
// shape of tk's object.
import { canonicalJson, sha256Hex } from "./canonical.ts";
import type { ServerRecord } from "./registry.ts";
import { OFFICIAL_KEY } from "./registry.ts";
import { PUBLISHER_PROVIDED, TK_META_KEY, isRecord } from "../validate/types.ts";

export type Liveness = { state: "alive" | "dead" | "unknown" | "not-applicable"; checkedAt?: string };

/** The publisher's server object with tk's key taken out, and whether it was there. */
export function stripTkKey(server: Record<string, unknown>): { server: Record<string, unknown>; hadTkKey: boolean } {
  const copy = structuredClone(server);
  const meta = copy["_meta"];
  if (!isRecord(meta)) return { server: copy, hadTkKey: false };
  const pp = meta[PUBLISHER_PROVIDED];
  if (!isRecord(pp)) return { server: copy, hadTkKey: false };
  // Any spelling that folds to the same key is taken out too (corner case 30, 33).
  let had = false;
  for (const k of Object.keys(pp)) {
    if (k.normalize("NFKC").toLowerCase().replace(/[​-‏⁠﻿]/g, "") === TK_META_KEY) {
      delete pp[k];
      had = true;
    }
  }
  if (Object.keys(pp).length === 0) delete meta[PUBLISHER_PROVIDED];
  if (Object.keys(meta).length === 0) delete copy["_meta"];
  return { server: copy, hadTkKey: had };
}

/** The content hash of a mirrored version: SHA-256 of the RFC 8785 form of
 *  the publisher's server object, tk's key removed. The same bytes on every
 *  machine, so a changed hash between two pulls of one version is drift. */
export function contentSha256(server: Record<string, unknown>): string {
  return sha256Hex(canonicalJson(stripTkKey(server).server));
}

type Runner = { kind: "remote" } | { kind: "image"; image: string } | { kind: "host"; note: "runs as you" };

/** How the entry's code would run, from what the publisher declared (plan
 *  R2: a bare package runs as you, and the card says so). A server with both
 *  a remote and packages is a remote: that is what tk connects by default. */
export function runnerOf(server: Record<string, unknown>): Runner {
  const remotes = server["remotes"];
  if (Array.isArray(remotes) && remotes.length > 0) return { kind: "remote" };
  const pkgs = server["packages"];
  if (Array.isArray(pkgs)) {
    const oci = pkgs.find((p) => isRecord(p) && p["registryType"] === "oci" && typeof p["identifier"] === "string");
    if (isRecord(oci) && pkgs.length === 1) return { kind: "image", image: String(oci["identifier"]).slice(0, 300) };
  }
  return { kind: "host", note: "runs as you" };
}

export function remoteUrls(server: Record<string, unknown>): string[] {
  const remotes = server["remotes"];
  if (!Array.isArray(remotes)) return [];
  return remotes.filter((r) => isRecord(r) && typeof r["url"] === "string").map((r) => String((r as Record<string, unknown>)["url"]));
}

/** The server.json tk stores for the latest version: the publisher's object
 *  with tk's record under its key. */
export function mirroredEntry(record: ServerRecord, liveness: Liveness): Record<string, unknown> {
  const { server } = stripTkKey(record.server);
  const official = record._meta[OFFICIAL_KEY];
  const tk = {
    kind: "server",
    tier: "mirrored",
    status: official.status,
    runner: runnerOf(server),
    exposure: { default: "deferred" },
    trust: {
      contentSha256: sha256Hex(canonicalJson(server)),
      scans: [],
      liveness,
    },
  };
  const meta = isRecord(server["_meta"]) ? (server["_meta"] as Record<string, unknown>) : {};
  const pp = isRecord(meta[PUBLISHER_PROVIDED]) ? (meta[PUBLISHER_PROVIDED] as Record<string, unknown>) : {};
  return { ...server, _meta: { ...meta, [PUBLISHER_PROVIDED]: { ...pp, [TK_META_KEY]: tk } } };
}
