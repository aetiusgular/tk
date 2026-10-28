// The library's own check of a publish pull request (lane G, T3.3).
//
// A proof made on the publisher's machine is self-reported. This runs in
// tk's CI, from the base branch's code, over the pull request's files as
// data (ci/verify-pr.yml), and re-derives each claim:
//   - the entry passes the library's rules and the catalog rules;
//   - the content hash, recomputed, equals the proof's and the manifest's;
//   - the namespace: for io.github.<owner>, the pull request's author is the
//     owner (an org's owners are checked by hand: the workflow's token cannot
//     read another user's org role); for a domain, the signature verifies
//     against a key the domain publishes now;
//   - the packages: install scripts and provenance again, from npm and PyPI.
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import type { FetchLike } from "../mirror/registry.ts";
import { loadBrands, loadCatalog, loadEntry, validateCatalog, validateEntry, type Catalog, type Finding } from "../validate/index.ts";
import { getMeta } from "../validate/types.ts";
import { PROOF_FILE, entryDigest } from "./digest.ts";
import { domainKeys, namespaceOf, verifyStatement, type FetchWellKnown, type ResolveTxt } from "./namespace.ts";
import { checkPackages } from "./packages.ts";
import { catalogRules, entryVersion, isProof, publishName } from "./publish.ts";

export type VerifyDeps = { fetchImpl?: FetchLike; resolveTxt?: ResolveTxt; fetchWellKnown?: FetchWellKnown };

/** The library's owner: the one account whose `own` entries need no proof (call 1 renames it with the repository). */
export const LIBRARY_OWNER = "aetiusgular";

const f = (rule: string, severity: "error" | "warning", message: string): Finding => ({ rule, severity, path: "", message });

/** `prLibrary` is the pull request's checkout, `baseLibrary` the base
 *  branch's; `id` the catalog folder the pull request adds or changes. */
export async function verifyEntry(o: { prLibrary: string; baseLibrary: string; id: string; prAuthor: string }, deps: VerifyDeps = {}): Promise<Finding[]> {
  const out: Finding[] = [];
  if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(o.id)) return [f("verify/id", "error", "the catalog folder name is not a library id")];
  const prRoot = resolve(o.prLibrary);
  const dir = join(prRoot, "library", "catalog", o.id);
  const entry = existsSync(dir) ? loadEntry(dir, prRoot) : undefined;
  if (!entry) return [f("verify/no-entry", "error", `library/catalog/${o.id} holds no manifest`)];
  const proofPath = join(dir, PROOF_FILE);
  const proofRaw = existsSync(proofPath) ? (JSON.parse(readFileSync(proofPath, "utf8")) as unknown) : undefined;
  const meta = getMeta(entry.kind, entry.data);
  const tier = meta?.tier;
  if (!isProof(proofRaw)) {
    // The library's owner edits own entries without the publish flow.
    if (tier === "own" && o.prAuthor.toLowerCase() === LIBRARY_OWNER) return [f("verify/own", "warning", "an own entry changed by the library's owner; no proof expected")];
    return [f("verify/no-proof", "error", `library/catalog/${o.id}/${PROOF_FILE} is missing or not a tk publish proof`)];
  }
  const proof = proofRaw;
  if (tier === "mirrored") out.push(f("verify/tier", "error", "`mirrored` is the mirror's tier, never a pull request's"));
  if (tier === "own" && o.prAuthor.toLowerCase() !== LIBRARY_OWNER) out.push(f("verify/tier", "error", "`own` is the library owner's tier; a contributed entry is `curated`"));

  // Rules, alone and in the base catalog.
  const baseRoot = resolve(o.baseLibrary);
  const baseCatalogDir = join(baseRoot, "library", "catalog");
  const brands = loadBrands(baseCatalogDir);
  const base = loadCatalog(baseCatalogDir, baseRoot);
  const joined: Catalog = { ...base, entries: [...base.entries.filter((e) => e.id !== entry.id), entry] };
  out.push(...validateEntry(entry, { brands, catalog: joined }).filter((x) => x.severity === "error"));
  out.push(...validateCatalog(joined, brands).catalog.filter((x) => x.severity === "error"));

  // The hash.
  const digest = entryDigest(entry);
  for (const e of digest.errors) out.push(f("verify/files", "error", e));
  if (digest.contentSha256 !== proof.contentSha256) out.push(f("verify/hash", "error", "the entry's files do not hash to the proof's contentSha256"));
  if (meta?.trust.contentSha256 !== digest.contentSha256) out.push(f("verify/hash", "error", "the manifest's trust.contentSha256 is not the entry's hash"));
  out.push(...catalogRules(entry, base, digest.contentSha256));
  const name = publishName(entry);
  const version = entryVersion(entry);
  if (name !== proof.name || version !== proof.entryVersion || proof.entry !== o.id) out.push(f("verify/proof-subject", "error", "the proof names another entry or version"));
  if (meta?.trust["namespaceProof"] !== proof.namespace.method) out.push(f("verify/proof-subject", "error", "the manifest's namespaceProof is not the proof's method"));

  // The namespace, again.
  if (name && version) {
    const ns = namespaceOf(name);
    if ("error" in ns) out.push(f("verify/namespace", "error", ns.error));
    else if (ns.kind === "github") {
      if (proof.namespace.method !== "github-oauth") out.push(f("verify/namespace", "error", "a GitHub namespace is proved by its owner's account"));
      else if (o.prAuthor.toLowerCase() !== ns.owner.toLowerCase())
        out.push(f("verify/namespace", "warning", `the pull request is by ${o.prAuthor}, not ${ns.owner}; an org namespace needs a reviewer to confirm ${o.prAuthor} owns io.github.${ns.owner}`));
    } else {
      const m = proof.namespace.method;
      if ((m !== "dns" && m !== "http-well-known") || !proof.namespace.signature || !proof.namespace.signedAt) out.push(f("verify/namespace", "error", "a domain namespace needs a DNS or well-known signature"));
      else {
        const keys = await domainKeys(ns.domain, { ...(deps.resolveTxt ? { resolveTxt: deps.resolveTxt } : {}), ...(deps.fetchWellKnown ? { fetchWellKnown: deps.fetchWellKnown } : {}) });
        const statement = { name, version, contentSha256: digest.contentSha256, signedAt: proof.namespace.signedAt };
        if (!keys.keys.some((k) => k.method === m && verifyStatement(statement, proof.namespace.signature!, k.key)))
          out.push(f("verify/namespace", "error", `no key ${ns.domain} publishes by ${m} verifies the signature${keys.notes.length ? ` (${keys.notes.join("; ")})` : ""}`));
      }
    }
  } else if (entry.kind !== "bundle") out.push(f("verify/namespace", "error", "no name or version"));
  else out.push(f("verify/namespace", "warning", "a bundle has no namespace; review it by hand"));

  // Packages, again.
  if (entry.kind === "server") {
    const reports = await checkPackages(entry.data, deps.fetchImpl ?? (globalThis.fetch as unknown as FetchLike));
    for (const p of reports) for (const x of p.findings) if (x.severity === "error") out.push(f(x.rule.replace(/^publish\//, "verify/"), "error", x.message));
    const firstWith = reports.find((p) => p.provenance.kind !== "none");
    const recorded = meta?.trust["provenance"] as { kind?: string; repository_uri?: string } | undefined;
    if ((firstWith?.provenance.kind ?? "none") !== (recorded?.kind ?? "none") || (firstWith?.provenance.repository_uri ?? "") !== (recorded?.repository_uri ?? ""))
      out.push(f("verify/provenance", "error", "the manifest's provenance record is not what the package registries say now"));
  }
  return out;
}
