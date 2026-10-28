// `tk publish` (lane G, T3.3): validate an entry, run the rejects, prove its
// namespace, record provenance and the content hash, and lay out the pull
// request against tk's library. Upstream publishing stays the publisher's own
// `mcp-publisher` run; tk prints the trimmed copy for it.
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import type { FetchLike } from "../mirror/registry.ts";
import { loadBrands, loadCatalog, loadEntry, validateCatalog, validateEntry, type Catalog, type Finding, type LoadedEntry } from "../validate/index.ts";
import { getMeta, isRecord } from "../validate/types.ts";
import { PROOF_FILE, entryDigest } from "./digest.ts";
import { domainKeys, githubMayPublish, namespaceOf, recordFor, signStatement, verifyStatement, type FetchWellKnown, type Gh, type ResolveTxt, type Statement } from "./namespace.ts";
import { checkPackages, type PackageReport } from "./packages.ts";
import { trimForUpstream, type Trimmed } from "./upstream.ts";

export type ProofMethod = "github-oauth" | "dns" | "http-well-known" | "none";

export type PublishProof = {
  version: 1;
  entry: string;
  kind: LoadedEntry["kind"];
  name: string;
  entryVersion: string;
  contentSha256: string;
  files: Record<string, string>;
  namespace: { method: ProofMethod; subject?: string; signedAt?: string; signature?: string; record?: string };
  packages: PackageReport[];
  createdAt: string;
  tool: "tk publish";
};

export type PublishDeps = {
  fetchImpl?: FetchLike;
  gh?: Gh;
  resolveTxt?: ResolveTxt;
  fetchWellKnown?: FetchWellKnown;
  now?: () => Date;
};

export type PublishOptions = {
  /** The entry folder to publish. */
  dir: string;
  /** A clone of tk's library repository; the catalog the entry joins. */
  library: string;
  /** How to prove the namespace: `github` (gh's login), `dns` or `http` (a key file). */
  proof: "github" | "dns" | "http" | "none";
  /** The private key in PEM, for `dns` and `http`; read by the caller from a file or stdin, never argv. */
  privateKeyPem?: string;
};

export type PublishResult = {
  ok: boolean;
  findings: Finding[];
  proof?: PublishProof;
  /** The manifest with tk's trust record filled in. */
  manifest?: Record<string, unknown>;
  upstream?: Trimmed;
  id?: string;
};

export function iso(d: Date): string {
  return d.toISOString().replace(/\.\d{3}Z$/, "Z");
}

const f = (rule: string, severity: "error" | "warning", message: string, path = ""): Finding => ({ rule, severity, path, message });

/** The registry name an entry publishes under: a server's `name`, a skill's
 *  `<namespace>/<name>`; a bundle has none. */
export function publishName(entry: LoadedEntry): string | undefined {
  const d = entry.data;
  if (entry.kind === "server" && typeof d["name"] === "string") return d["name"];
  if (entry.kind === "skill" && typeof d["namespace"] === "string" && typeof d["name"] === "string") return `${d["namespace"]}/${d["name"]}`;
  return undefined;
}

export function entryVersion(entry: LoadedEntry): string | undefined {
  return typeof entry.data["version"] === "string" ? entry.data["version"] : undefined;
}

/** Rules a published version must meet against the catalog it joins. */
export function catalogRules(entry: LoadedEntry, catalog: Catalog, contentSha256: string): Finding[] {
  const out: Finding[] = [];
  const name = publishName(entry);
  const version = entryVersion(entry);
  for (const other of catalog.entries) {
    const otherName = publishName(other);
    if (other.id === entry.id) {
      // Versions are immutable: one version number, one content.
      if (version && entryVersion(other) === version) {
        const had = getMeta(other.kind, other.data)?.trust.contentSha256;
        if (had && had !== contentSha256 && !/^0{64}$/.test(had)) out.push(f("publish/immutable", "error", `${other.id} ${version} is already in the library with other content; publish a new version`));
      }
      if (otherName && name && otherName !== name) out.push(f("publish/id-taken", "error", `the folder ${other.id} holds ${otherName}, not ${name}`));
    } else if (name && otherName && otherName.toLowerCase() === name.toLowerCase()) {
      out.push(f("publish/name-taken", "error", `${name} is already in the library as ${other.id}`));
    }
  }
  return out;
}

export async function publish(o: PublishOptions, deps: PublishDeps = {}): Promise<PublishResult> {
  const now = deps.now ?? (() => new Date());
  const fetchImpl = deps.fetchImpl ?? (globalThis.fetch as unknown as FetchLike);
  const libraryRoot = resolve(o.library);
  const catalogDir = join(libraryRoot, "library", "catalog");
  if (!existsSync(catalogDir)) return { ok: false, findings: [f("publish/library", "error", `${libraryRoot} is not a clone of tk's library (no library/catalog)`)] };
  const entry = loadEntry(resolve(o.dir), libraryRoot);
  if (!entry) return { ok: false, findings: [f("publish/no-manifest", "error", "no server.json, skill.json or .claude-plugin/plugin.json in the folder")] };
  const findings: Finding[] = [];
  const meta = getMeta(entry.kind, entry.data);
  if (!meta) return { ok: false, findings: [f("publish/no-tk-meta", "error", "the manifest has no tk object; add one as contracts/entry.schema.json describes")] };
  if (meta.tier !== "curated" && meta.tier !== "own") findings.push(f("publish/tier", "error", "a published entry is `curated` (or `own` for the library's owner); `mirrored` is the mirror's", "/tier"));

  // 1. The library's own rules, the entry alone and with the catalog it joins.
  const brands = loadBrands(catalogDir);
  const catalog = loadCatalog(catalogDir, libraryRoot);
  const joined: Catalog = { ...catalog, entries: [...catalog.entries.filter((e) => e.id !== entry.id), entry] };
  findings.push(...validateEntry(entry, { brands, catalog: joined }));
  const wide = validateCatalog(joined, brands);
  findings.push(...wide.catalog);
  findings.push(...(wide.byEntry.get(entry.id) ?? []).filter((x) => !findings.some((y) => y.rule === x.rule && y.path === x.path && y.message === x.message)));

  // 2. The content hash, and the rules against the catalog's earlier version.
  const digest = entryDigest(entry);
  for (const e of digest.errors) findings.push(f("publish/files", "error", e));
  findings.push(...catalogRules(entry, catalog, digest.contentSha256));

  // 3. Packages: install-time execution and provenance (servers only).
  const packages = entry.kind === "server" ? await checkPackages(entry.data, fetchImpl) : [];
  for (const p of packages) for (const x of p.findings) findings.push(f(x.rule, x.severity, x.message, `/packages/${p.identifier}`));

  // 4. The namespace.
  const name = publishName(entry);
  const version = entryVersion(entry);
  const signedAt = iso(now());
  let ns: PublishProof["namespace"] = { method: "none" };
  if (!name || !version) {
    findings.push(f("publish/namespace", entry.kind === "bundle" ? "warning" : "error", entry.kind === "bundle" ? "a bundle has no registry namespace; its pull request is reviewed by hand" : "no name or version to publish under"));
  } else if (o.proof === "none") {
    findings.push(f("publish/namespace", "error", "a namespace proof is required (--proof github, dns or http)"));
  } else {
    const parsed = namespaceOf(name);
    if ("error" in parsed) findings.push(f("publish/namespace", "error", parsed.error));
    else if (parsed.kind === "github") {
      if (o.proof !== "github") findings.push(f("publish/namespace", "error", `${name} is a GitHub namespace; prove it with --proof github`));
      else if (!deps.gh) findings.push(f("publish/namespace", "error", "no gh to ask who is logged in"));
      else {
        const r = await githubMayPublish(parsed.owner, deps.gh);
        if (r.ok) ns = { method: "github-oauth", subject: r.login! };
        else findings.push(f("publish/namespace", "error", r.note ?? `not allowed to publish under io.github.${parsed.owner}`));
      }
    } else {
      if (o.proof === "github") findings.push(f("publish/namespace", "error", `${name} is a domain namespace; prove it with --proof dns or --proof http`));
      else if (!o.privateKeyPem) findings.push(f("publish/namespace", "error", "a domain proof needs the private key (--key-file <path> or --key-stdin)"));
      else {
        let record: string | undefined;
        try {
          record = recordFor(o.privateKeyPem);
        } catch (e) {
          findings.push(f("publish/namespace", "error", (e as Error).message));
        }
        if (record) {
          const statement: Statement = { name, version, contentSha256: digest.contentSha256, signedAt };
          const signature = signStatement(statement, o.privateKeyPem);
          const keys = await domainKeys(parsed.domain, { ...(deps.resolveTxt ? { resolveTxt: deps.resolveTxt } : {}), ...(deps.fetchWellKnown ? { fetchWellKnown: deps.fetchWellKnown } : {}) });
          const want = o.proof === "dns" ? "dns" : "http-well-known";
          const match = keys.keys.find((k) => k.method === want && verifyStatement(statement, signature, k.key));
          if (match) ns = { method: want, subject: match.domain, signedAt, signature, record };
          else findings.push(f("publish/namespace", "error", `no ${o.proof === "dns" ? "TXT record" : "well-known file"} for ${parsed.domain} carries this key (expected: ${record})${keys.notes.length ? `; ${keys.notes.join("; ")}` : ""}`));
        }
      }
    }
  }

  // 5. The record.
  const first = packages.find((p) => p.provenance.kind !== "none");
  const manifest = structuredClone(entry.data);
  const tk = getMeta(entry.kind, manifest)!;
  tk.trust = { ...tk.trust, contentSha256: digest.contentSha256, namespaceProof: ns.method, provenance: first ? first.provenance : { kind: "none" } };
  const proof: PublishProof = {
    version: 1,
    entry: entry.id,
    kind: entry.kind,
    name: name ?? entry.id,
    entryVersion: version ?? "",
    contentSha256: digest.contentSha256,
    files: digest.files,
    namespace: ns,
    packages,
    createdAt: signedAt,
    tool: "tk publish",
  };
  const upstream = entry.kind === "server" ? trimForUpstream(manifest) : undefined;
  if (upstream?.error) findings.push(f("publish/upstream-meta", "warning", upstream.error));
  const ok = !findings.some((x) => x.severity === "error");
  return { ok, findings, proof, manifest, ...(upstream ? { upstream } : {}), id: entry.id };
}

/** True when `x` looks like a proof file tk wrote. */
export function isProof(x: unknown): x is PublishProof {
  return isRecord(x) && x["version"] === 1 && x["tool"] === "tk publish" && typeof x["contentSha256"] === "string" && isRecord(x["namespace"]);
}

export { PROOF_FILE };
