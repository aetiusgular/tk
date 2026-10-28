// What `tk publish` reads about an entry's packages (lane G, T3.3): install
// scripts and provenance. Only the two package registries whose APIs were
// read are asked; every other kind is recorded as not checked.
//
//   npm   GET https://registry.npmjs.org/<name>/<version>: `scripts`
//         (`preinstall`, `install`, `postinstall`) and `gypfile`; npm runs
//         `node-gyp rebuild` as the install script when a package has a
//         binding.gyp and no install script of its own (docs.npmjs.com,
//         "scripts", v11). GET .../-/npm/v1/attestations/<name>@<version>:
//         the SLSA provenance statement, whose subject digest is the
//         tarball's sha512 and whose `externalParameters.workflow` names the
//         source repository (run on @modelcontextprotocol/server-everything
//         2.0.1, 28 Oct 2026).
//   PyPI  GET https://pypi.org/pypi/<project>/<version>/json for file names,
//         then GET https://pypi.org/integrity/<project>/<version>/<file>/provenance
//         (PEP 740): `attestation_bundles[].publisher` names the Trusted
//         Publisher, the statement's subject digest is the file's sha256 (run
//         on mcp-server-fetch 2025.4.7, 28 Oct 2026).
//
// tk records what the attestation says and checks that it is bound to this
// package (the digest) and to the entry (the repository). It does not verify
// the Sigstore signature chain; `npm audit signatures` and PyPI do. The
// record says so.
import { isRecord } from "../validate/types.ts";
import type { FetchLike } from "../mirror/registry.ts";

export type Provenance = {
  kind: "npm-provenance" | "pypi-attestation" | "none";
  repository_uri?: string;
  runner_environment?: string;
  signer_identity?: string;
  sigstore_url?: string;
};

export type PackageFinding = { rule: string; severity: "error" | "warning"; message: string };

export type PackageReport = { registryType: string; identifier: string; version?: string; provenance: Provenance; findings: PackageFinding[]; checked: boolean };

const NPM = "https://registry.npmjs.org";
const PYPI = "https://pypi.org";

async function getJson(fetchImpl: FetchLike, url: string): Promise<{ status: number; body?: unknown }> {
  const r = await fetchImpl(url, { headers: { accept: "application/json" } });
  if (r.status !== 200) return { status: r.status };
  const text = await r.text();
  if (text.length > 32 * 1024 * 1024) return { status: 413 };
  try {
    return { status: 200, body: JSON.parse(text) as unknown };
  } catch {
    return { status: 502 };
  }
}

function sameRepo(a: string | undefined, b: string | undefined): boolean {
  const norm = (u: string) => u.trim().toLowerCase().replace(/^git\+/, "").replace(/\.git$/, "").replace(/\/+$/, "");
  return !!a && !!b && norm(a) === norm(b);
}

const INSTALL_SCRIPTS = ["preinstall", "install", "postinstall"] as const;

export async function checkNpm(name: string, version: string, repoUrl: string | undefined, fetchImpl: FetchLike): Promise<PackageReport> {
  const report: PackageReport = { registryType: "npm", identifier: name, version, provenance: { kind: "none" }, findings: [], checked: true };
  // npm names: optional @scope/ and a URL-safe name (encoded either way).
  if (!/^(@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/.test(name) || name.length > 214) {
    report.findings.push({ rule: "publish/package-name", severity: "error", message: "not an npm package name" });
    return report;
  }
  const enc = name.replace("/", "%2f");
  const doc = await getJson(fetchImpl, `${NPM}/${enc}/${encodeURIComponent(version)}`);
  if (doc.status !== 200 || !isRecord(doc.body)) {
    report.findings.push({ rule: "publish/package-missing", severity: "error", message: `npm has no ${name}@${version} (HTTP ${doc.status})` });
    return report;
  }
  const scripts = isRecord(doc.body["scripts"]) ? doc.body["scripts"] : {};
  const ran = INSTALL_SCRIPTS.filter((s) => typeof scripts[s] === "string");
  if (ran.length || doc.body["gypfile"] === true)
    report.findings.push({ rule: "publish/install-script", severity: "error", message: `${name}@${version} runs code when installed (${[...ran, ...(doc.body["gypfile"] === true ? ["node-gyp"] : [])].join(", ")}); the library refuses install-time execution` });
  const integrity = isRecord(doc.body["dist"]) && typeof doc.body["dist"]["integrity"] === "string" ? doc.body["dist"]["integrity"] : undefined;
  const att = await getJson(fetchImpl, `${NPM}/-/npm/v1/attestations/${enc}@${encodeURIComponent(version)}`);
  if (att.status === 404) {
    report.findings.push({ rule: "publish/no-provenance", severity: "warning", message: `${name}@${version} was published without provenance` });
    return report;
  }
  if (att.status !== 200 || !isRecord(att.body) || !Array.isArray(att.body["attestations"])) {
    report.findings.push({ rule: "publish/provenance-unreadable", severity: "warning", message: `npm's attestations for ${name}@${version} could not be read (HTTP ${att.status})` });
    return report;
  }
  for (const a of att.body["attestations"]) {
    if (!isRecord(a) || a["predicateType"] !== "https://slsa.dev/provenance/v1") continue;
    const bundle = isRecord(a["bundle"]) ? a["bundle"] : {};
    const env = isRecord(bundle["dsseEnvelope"]) ? bundle["dsseEnvelope"] : {};
    let stmt: Record<string, unknown> = {};
    try {
      stmt = JSON.parse(Buffer.from(String(env["payload"] ?? ""), "base64").toString("utf8")) as Record<string, unknown>;
    } catch {
      continue;
    }
    const subj = Array.isArray(stmt["subject"]) ? stmt["subject"].filter(isRecord) : [];
    const sha512 = subj.map((s) => (isRecord(s["digest"]) ? s["digest"]["sha512"] : undefined)).find((d) => typeof d === "string") as string | undefined;
    const pred = isRecord(stmt["predicate"]) ? stmt["predicate"] : {};
    const wf = isRecord(pred["buildDefinition"]) && isRecord(pred["buildDefinition"]["externalParameters"]) && isRecord(pred["buildDefinition"]["externalParameters"]["workflow"]) ? pred["buildDefinition"]["externalParameters"]["workflow"] : {};
    const builder = isRecord(pred["runDetails"]) && isRecord(pred["runDetails"]["builder"]) ? pred["runDetails"]["builder"]["id"] : undefined;
    const repo = typeof wf["repository"] === "string" ? wf["repository"] : undefined;
    const tlog = isRecord(bundle["verificationMaterial"]) && Array.isArray(bundle["verificationMaterial"]["tlogEntries"]) ? bundle["verificationMaterial"]["tlogEntries"][0] : undefined;
    const logIndex = isRecord(tlog) && typeof tlog["logIndex"] === "string" && /^\d+$/.test(tlog["logIndex"]) ? tlog["logIndex"] : undefined;
    report.provenance = {
      kind: "npm-provenance",
      ...(repo ? { repository_uri: repo } : {}),
      ...(typeof builder === "string" ? { runner_environment: builder } : {}),
      ...(repo && typeof wf["path"] === "string" ? { signer_identity: `${repo}/${wf["path"]}${typeof wf["ref"] === "string" ? `@${wf["ref"]}` : ""}` } : {}),
      ...(logIndex ? { sigstore_url: `https://search.sigstore.dev/?logIndex=${logIndex}` } : {}),
    };
    const wantHex = integrity && integrity.startsWith("sha512-") ? Buffer.from(integrity.slice(7), "base64").toString("hex") : undefined;
    if (!wantHex || sha512 !== wantHex) report.findings.push({ rule: "publish/provenance-mismatch", severity: "error", message: `the provenance statement for ${name}@${version} is not about the tarball npm serves` });
    if (repoUrl && repo && !sameRepo(repo, repoUrl)) report.findings.push({ rule: "publish/provenance-repository", severity: "error", message: `${name}@${version} was built from ${repo}, not the entry's repository` });
    return report;
  }
  report.findings.push({ rule: "publish/no-provenance", severity: "warning", message: `${name}@${version} has attestations but no SLSA provenance` });
  return report;
}

export async function checkPypi(project: string, version: string, repoUrl: string | undefined, fetchImpl: FetchLike): Promise<PackageReport> {
  const report: PackageReport = { registryType: "pypi", identifier: project, version, provenance: { kind: "none" }, findings: [], checked: true };
  if (!/^[A-Za-z0-9]([A-Za-z0-9._-]*[A-Za-z0-9])?$/.test(project) || project.length > 200) {
    report.findings.push({ rule: "publish/package-name", severity: "error", message: "not a PyPI project name" });
    return report;
  }
  const doc = await getJson(fetchImpl, `${PYPI}/pypi/${encodeURIComponent(project)}/${encodeURIComponent(version)}/json`);
  if (doc.status !== 200 || !isRecord(doc.body) || !Array.isArray(doc.body["urls"])) {
    report.findings.push({ rule: "publish/package-missing", severity: "error", message: `PyPI has no ${project} ${version} (HTTP ${doc.status})` });
    return report;
  }
  const files = doc.body["urls"].filter(isRecord);
  // An sdist with no wheel runs its build backend on the installer's machine.
  if (!files.some((f) => f["packagetype"] === "bdist_wheel"))
    report.findings.push({ rule: "publish/install-script", severity: "error", message: `${project} ${version} ships no wheel, so installing it runs its build code` });
  const wheel = files.find((f) => f["packagetype"] === "bdist_wheel") ?? files[0];
  if (!wheel || typeof wheel["filename"] !== "string" || !/^[A-Za-z0-9._+-]+$/.test(wheel["filename"])) return report;
  const sha = isRecord(wheel["digests"]) ? wheel["digests"]["sha256"] : undefined;
  const prov = await getJson(fetchImpl, `${PYPI}/integrity/${encodeURIComponent(project)}/${encodeURIComponent(version)}/${encodeURIComponent(wheel["filename"])}/provenance`);
  if (prov.status !== 200 || !isRecord(prov.body) || !Array.isArray(prov.body["attestation_bundles"])) {
    report.findings.push({ rule: "publish/no-provenance", severity: "warning", message: `${wheel["filename"]} has no PEP 740 attestation (HTTP ${prov.status})` });
    return report;
  }
  for (const b of prov.body["attestation_bundles"].filter(isRecord)) {
    const pub = isRecord(b["publisher"]) ? b["publisher"] : {};
    const repo = pub["kind"] === "GitHub" && typeof pub["repository"] === "string" ? `https://github.com/${pub["repository"]}` : undefined;
    const atts = Array.isArray(b["attestations"]) ? b["attestations"].filter(isRecord) : [];
    const digests = atts.map((a) => {
      try {
        const env = isRecord(a["envelope"]) ? a["envelope"] : {};
        const st = JSON.parse(Buffer.from(String(env["statement"] ?? ""), "base64").toString("utf8")) as Record<string, unknown>;
        const s0 = Array.isArray(st["subject"]) ? st["subject"][0] : undefined;
        return isRecord(s0) && isRecord(s0["digest"]) ? s0["digest"]["sha256"] : undefined;
      } catch {
        return undefined;
      }
    });
    report.provenance = {
      kind: "pypi-attestation",
      ...(repo ? { repository_uri: repo } : {}),
      ...(typeof pub["kind"] === "string" ? { runner_environment: `${pub["kind"]}${typeof pub["environment"] === "string" ? ` (${pub["environment"]})` : ""}` } : {}),
      ...(repo && typeof pub["workflow"] === "string" ? { signer_identity: `${repo}/.github/workflows/${pub["workflow"]}` } : {}),
    };
    if (!digests.includes(sha)) report.findings.push({ rule: "publish/provenance-mismatch", severity: "error", message: `the attestation for ${wheel["filename"]} is not about the file PyPI serves` });
    if (repoUrl && repo && !sameRepo(repo, repoUrl)) report.findings.push({ rule: "publish/provenance-repository", severity: "error", message: `${project} ${version} was published from ${repo}, not the entry's repository` });
    return report;
  }
  return report;
}

/** Every package of a server.json, checked where tk knows how. */
export async function checkPackages(server: Record<string, unknown>, fetchImpl: FetchLike): Promise<PackageReport[]> {
  const out: PackageReport[] = [];
  const repo = isRecord(server["repository"]) && typeof server["repository"]["url"] === "string" ? server["repository"]["url"] : undefined;
  const pkgs = Array.isArray(server["packages"]) ? server["packages"].filter(isRecord) : [];
  for (const p of pkgs) {
    const type = String(p["registryType"] ?? "");
    const id = String(p["identifier"] ?? "");
    const version = typeof p["version"] === "string" ? p["version"] : undefined;
    const base = typeof p["registryBaseUrl"] === "string" ? p["registryBaseUrl"].replace(/\/+$/, "") : undefined;
    if (type === "npm" && version && (!base || base === NPM)) out.push(await checkNpm(id, version, repo, fetchImpl));
    else if (type === "pypi" && version && (!base || base === PYPI)) out.push(await checkPypi(id, version, repo, fetchImpl));
    else
      out.push({
        registryType: type,
        identifier: id,
        ...(version ? { version } : {}),
        provenance: { kind: "none" },
        checked: false,
        findings: [{ rule: "publish/package-not-checked", severity: "warning", message: `${type || "this"} package${base ? ` on ${base}` : ""} is not checked by tk publish yet: no install-script or provenance record` }],
      });
  }
  return out;
}
