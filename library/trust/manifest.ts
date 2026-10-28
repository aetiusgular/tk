// Per-version manifests, drift and description pins (toolkit plan T4.2; lane H).
//
// What a server advertised at one version is stored as a manifest: its
// remotes and packages, and each tool's name, description, input schema and
// annotations. Two manifests of one server diff into the contract's
// `capabilityDrift.changed` values (contracts/entry.schema.json, trust):
// tools-added, tools-removed, description, input-schema, endpoint-host,
// annotations. Silent drift is common: 51.1% of multi-version registry servers
// changed advertised capabilities, 40.6% silently, and 4.2% moved the remote
// endpoint to another host under the same identity (arXiv 2609.14119, note AP
// section 5, V). The rug pull Invariant described is a description that
// changes after approval (note AP section 5).
//
// The pin is the hash every matcher checks: SHA-256 of the description's
// UTF-8 bytes, unnormalized (packages/tk-claude-plugin/src/core/policy.ts:454,
// packages/tk-pi/src/policy.ts:367, crates/tk-core/src/policy.rs:557).

import { createHash } from "node:crypto";
import type { ToolsRecord } from "../validate/types.ts";
import { isRecord } from "../validate/types.ts";
import { hostsOf } from "./identity.ts";

export const DRIFT_KINDS = ["tools-added", "tools-removed", "description", "input-schema", "endpoint-host", "annotations"] as const;
export type DriftKind = (typeof DRIFT_KINDS)[number];

export function sha256Hex(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

/** JSON with object keys sorted at every depth and no whitespace, so two
 *  equal schemas hash equal whatever order a server wrote them in. */
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (isRecord(value)) {
    const keys = Object.keys(value).sort();
    return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalJson(value[k])}`).join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

export type ManifestTool = {
  name: string;
  /** The description as the server sent it, verbatim; absent when not recorded. */
  description?: string;
  descriptionSha256?: string;
  inputSchemaSha256?: string;
  annotations?: Record<string, unknown>;
};

export type Manifest = {
  version: 1;
  /** The server's `name` (`io.github.owner/id`). */
  server: string;
  /** The server's `version`. */
  serverVersion: string;
  /** Where the tool list came from (tools.json `source`). */
  source: string;
  recordedAt: string;
  dynamic: boolean;
  remotes: { type: string; url: string }[];
  remoteHosts: string[];
  packages: { registryType: string; identifier: string; version?: string }[];
  tools: ManifestTool[];
  /** SHA-256 of the canonical JSON of everything above. */
  manifestSha256: string;
};

/** Builds the manifest of one server version from its server.json and its tool record. */
export function manifestOf(server: Record<string, unknown>, tools: ToolsRecord | undefined): Manifest {
  const name = typeof server["name"] === "string" ? server["name"] : "";
  const serverVersion = typeof server["version"] === "string" ? server["version"] : "";
  if (name === "" || serverVersion === "") throw new Error("a manifest needs the server's name and version");
  const remotes: Manifest["remotes"] = [];
  if (Array.isArray(server["remotes"])) {
    for (const r of server["remotes"]) if (isRecord(r) && typeof r["url"] === "string") remotes.push({ type: typeof r["type"] === "string" ? r["type"] : "", url: r["url"] });
  }
  const packages: Manifest["packages"] = [];
  if (Array.isArray(server["packages"])) {
    for (const p of server["packages"]) {
      if (!isRecord(p) || typeof p["identifier"] !== "string") continue;
      const pkg: Manifest["packages"][number] = { registryType: typeof p["registryType"] === "string" ? p["registryType"] : "", identifier: p["identifier"] };
      if (typeof p["version"] === "string") pkg.version = p["version"];
      packages.push(pkg);
    }
  }
  const list: ManifestTool[] = (tools?.tools ?? []).map((t) => {
    const out: ManifestTool = { name: t.name };
    if (typeof t.description === "string") {
      out.description = t.description;
      out.descriptionSha256 = sha256Hex(t.description);
    }
    if (t.inputSchema !== undefined) out.inputSchemaSha256 = sha256Hex(canonicalJson(t.inputSchema));
    if (t.annotations !== undefined) out.annotations = { ...t.annotations };
    return out;
  });
  list.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  const body = {
    version: 1 as const,
    server: name,
    serverVersion,
    source: tools?.source ?? "no tool record",
    recordedAt: tools?.recordedAt ?? "",
    dynamic: tools?.dynamic === true,
    remotes: remotes.sort((a, b) => canonicalJson(a).localeCompare(canonicalJson(b))),
    remoteHosts: [...new Set(hostsOf(server).map((h) => h.toLowerCase()))].sort(),
    packages: packages.sort((a, b) => canonicalJson(a).localeCompare(canonicalJson(b))),
    tools: list,
  };
  return { ...body, manifestSha256: sha256Hex(canonicalJson(body)) };
}

export type Drift = {
  changed: DriftKind[];
  toolsAdded: string[];
  toolsRemoved: string[];
  description: string[];
  inputSchema: string[];
  annotations: string[];
  hostsAdded: string[];
  hostsRemoved: string[];
  /** Package identities that changed. The contract's enum has no value for
   *  this; it is recorded here and asked of integration. */
  packages: { removed: string[]; added: string[] };
  /** Tools whose description or schema one side did not record: not drift,
   *  because nothing can be compared, but shown so a card does not imply it was. */
  unrecorded: string[];
};

const pkgKey = (p: Manifest["packages"][number]) => `${p.registryType}:${p.identifier}`;

/** What changed between two manifests of the same server. Only two recorded
 *  values are compared: a description that appears where none was recorded is
 *  `unrecorded`, not drift. */
export function diffManifests(prev: Manifest, next: Manifest): Drift {
  if (prev.server !== next.server) throw new Error(`manifests of two servers: ${prev.server} and ${next.server}`);
  const before = new Map(prev.tools.map((t) => [t.name, t]));
  const after = new Map(next.tools.map((t) => [t.name, t]));
  const d: Drift = { changed: [], toolsAdded: [], toolsRemoved: [], description: [], inputSchema: [], annotations: [], hostsAdded: [], hostsRemoved: [], packages: { removed: [], added: [] }, unrecorded: [] };
  for (const name of after.keys()) if (!before.has(name)) d.toolsAdded.push(name);
  for (const name of before.keys()) if (!after.has(name)) d.toolsRemoved.push(name);
  for (const [name, b] of before) {
    const a = after.get(name);
    if (!a) continue;
    let unrecorded = false;
    if (b.descriptionSha256 !== undefined && a.descriptionSha256 !== undefined) {
      if (b.descriptionSha256 !== a.descriptionSha256) d.description.push(name);
    } else if (b.descriptionSha256 !== a.descriptionSha256) unrecorded = true;
    if (b.inputSchemaSha256 !== undefined && a.inputSchemaSha256 !== undefined) {
      if (b.inputSchemaSha256 !== a.inputSchemaSha256) d.inputSchema.push(name);
    } else if (b.inputSchemaSha256 !== a.inputSchemaSha256) unrecorded = true;
    if (b.annotations !== undefined && a.annotations !== undefined) {
      if (canonicalJson(b.annotations) !== canonicalJson(a.annotations)) d.annotations.push(name);
    } else if ((b.annotations === undefined) !== (a.annotations === undefined)) unrecorded = true;
    if (unrecorded) d.unrecorded.push(name);
  }
  const hb = new Set(prev.remoteHosts);
  const ha = new Set(next.remoteHosts);
  d.hostsAdded = [...ha].filter((h) => !hb.has(h)).sort();
  d.hostsRemoved = [...hb].filter((h) => !ha.has(h)).sort();
  const pb = new Set(prev.packages.map(pkgKey));
  const pa = new Set(next.packages.map(pkgKey));
  d.packages.added = [...pa].filter((p) => !pb.has(p)).sort();
  d.packages.removed = [...pb].filter((p) => !pa.has(p)).sort();
  for (const k of [d.toolsAdded, d.toolsRemoved, d.description, d.inputSchema, d.annotations, d.unrecorded]) k.sort();
  if (d.toolsAdded.length) d.changed.push("tools-added");
  if (d.toolsRemoved.length) d.changed.push("tools-removed");
  if (d.description.length) d.changed.push("description");
  if (d.inputSchema.length) d.changed.push("input-schema");
  // A new host is drift; a host dropped from a list that still has others is
  // not a move, but a remote that went from one host to another is.
  if (d.hostsAdded.length) d.changed.push("endpoint-host");
  if (d.annotations.length) d.changed.push("annotations");
  return d;
}

/** The contract's `trust.capabilityDrift` object, or undefined when nothing changed. */
export function capabilityDrift(prev: Manifest, next: Manifest): { fromVersion: string; toVersion: string; changed: DriftKind[] } | undefined {
  const d = diffManifests(prev, next);
  if (d.changed.length === 0) return undefined;
  return { fromVersion: prev.serverVersion, toVersion: next.serverVersion, changed: d.changed };
}

/**
 * Harnesses whose gate hands the matcher the tool's description, so a pin can
 * hold there. Pi's gate reads it from `pi.getAllTools()`
 * (packages/tk-pi/extensions/gate.ts:79). The Claude Code, Codex and OpenCode
 * hooks pass none (their hook input carries none), and a pinned `allow` with
 * no description on the call is `ask` (contracts/policy.md section 5 step 6),
 * so a pin there would turn every allow into an ask.
 */
export const HARNESSES_THAT_SEE_DESCRIPTIONS = ["pi"] as const;

/** The policy file's server part: contracts/policy.md section 2 step 1, the
 *  same step the matcher applies to the name it receives. */
export function policyServer(serverId: string): string {
  return serverId.trim().toLowerCase().replace(/[^a-z0-9_]/g, "_");
}

export function policyTool(toolName: string): string {
  return toolName.trim().toLowerCase().replace(/[^a-z0-9_]/g, "_");
}

export type PinRule = { tool: string; harness: string; action: "allow"; descriptionSha256: string };

export type Pins = { rules: PinRule[]; skipped: { tool: string; reason: string }[] };

/**
 * The rules that pin each approved tool's description for one harness. The
 * caller passes the tools the user approved; a tool with no recorded
 * description gets no pin (it would always ask) and is listed. Printed for the
 * user or for `tk connect`; lane H never writes the policy file, which is a
 * control file the gate guards (contracts/policy.md section 4 step 7a).
 */
export function pinsFor(manifest: Manifest, serverId: string, harness: string, approved: readonly string[] = manifest.tools.map((t) => t.name)): Pins {
  if (!(HARNESSES_THAT_SEE_DESCRIPTIONS as readonly string[]).includes(harness)) {
    throw new Error(`${harness}'s gate does not see a tool's description, so a pin there would turn every allow into an ask; pins are written for ${HARNESSES_THAT_SEE_DESCRIPTIONS.join(", ")} only`);
  }
  const byName = new Map(manifest.tools.map((t) => [t.name, t]));
  const out: Pins = { rules: [], skipped: [] };
  const server = policyServer(serverId);
  for (const name of approved) {
    const t = byName.get(name);
    if (!t) {
      out.skipped.push({ tool: name, reason: "not in the manifest" });
      continue;
    }
    if (t.descriptionSha256 === undefined) {
      out.skipped.push({ tool: name, reason: "no description recorded for this version (lane G's mirror records tools/list)" });
      continue;
    }
    out.rules.push({ tool: `${server}/${policyTool(t.name)}`, harness, action: "allow", descriptionSha256: t.descriptionSha256 });
  }
  return out;
}
