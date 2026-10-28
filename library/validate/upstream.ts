// The official registry's semantic checks on a server.json, transcribed from
// modelcontextprotocol/registry at 9c4a96c90fbcd3c3a169cdcd5443419c7a0c39d7
// (27 Oct 2026): internal/validators/validators.go (ValidateServerJSON,
// parseServerName, validateVersion, validateRepository, validatePackageField,
// validatePackageTransport, validateRemoteTransport, validatePublisherExtensions)
// and utils.go (IsValidRemoteURL, IsValidRepositoryURL). The registry also
// serves them online as POST /v0.1/validate (docs/reference/api/
// official-registry-api.md:125, handler internal/api/handlers/v0/validate.go),
// used by `mcp-publisher validate`; that endpoint is not in openapi.yaml.
// `checkUpstreamOnline` calls it only when TK_LIBRARY_ONLINE=1, because the
// daemon and the validator make no outbound call by default (rules section 5,
// item 8); the cloud workspace cannot reach the host at all.
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import type { Finding } from "./types.ts";
import { finding, isRecord } from "./types.ts";

const execFileAsync = promisify(execFile);

export const UPSTREAM_VALIDATE_URL = "https://registry.modelcontextprotocol.io/v0.1/validate";
/** validators.go validatePublisherExtensions: the publisher-provided _meta
 *  object may not exceed 4 KB at publish. The library's trust records can
 *  exceed it; the mirror and publish lanes must know. */
export const PUBLISHER_PROVIDED_LIMIT = 4 * 1024;

// utils.go githubURLRegex and gitlabURLRegex, as read at that commit.
const GITHUB_URL = /^https?:\/\/(www\.)?github\.com\/[\w.-]+\/[\w.-]+\/?$/;
const GITLAB_URL = /^https?:\/\/(www\.)?gitlab\.com\/[\w.-]+(\/[\w.-]+)+\/?$/;
// validators.go lines 38 to 51, the four range shapes.
const COMPARATOR_RANGE = /^\s*(?:\^|~|>=|<=|>|<|=)\s*v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?\s*$/;
const HYPHEN_RANGE = /^\s*v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?\s-\s*v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?\s*$/;
const OR_RANGE = /^\s*(?:v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?\s*)(?:\|\|\s*v?\d+(?:\.\d+){0,3}(?:-[0-9A-Za-z.-]+)?\s*)+$/;
const DOTTED_VERSION_LIKE = /^\s*(?:v?\d+|x|X|\*)(?:\.(?:\d+|x|X|\*)){1,2}(?:-[0-9A-Za-z.-]+)?\s*$/;

/** validators.go looksLikeVersionRange. */
export function looksLikeVersionRange(version: string): boolean {
  const t = version.trim();
  if (t === "") return false;
  if (COMPARATOR_RANGE.test(t) || HYPHEN_RANGE.test(t) || OR_RANGE.test(t)) return true;
  if (DOTTED_VERSION_LIKE.test(t)) return /[xX*]/.test(t);
  return false;
}

function isValidRemoteUrl(raw: string): boolean {
  let u: URL;
  try {
    u = new URL(raw.replace(/\{[^}]+\}/g, "placeholder"));
  } catch {
    return false;
  }
  const host = u.hostname;
  if (host === "localhost" || host === "127.0.0.1" || host.endsWith(".localhost")) return false;
  return u.protocol === "https:";
}

/** Rule family `upstream/*`. */
export function checkUpstreamSemantics(data: Record<string, unknown>): Finding[] {
  const out: Finding[] = [];
  const name = data["name"];
  if (typeof name !== "string" || name === "") {
    out.push(finding("upstream/server-name", "error", "/name", "server name is required and must be a string"));
  } else if (!name.includes("/")) {
    out.push(finding("upstream/server-name", "error", "/name", "server name must be in format 'dns-namespace/name'"));
  } else if (name.split("/").length !== 2) {
    out.push(finding("upstream/server-name", "error", "/name", "server name cannot contain multiple slashes"));
  }
  const version = data["version"];
  if (typeof version === "string") {
    if (version === "latest") out.push(finding("upstream/version", "error", "/version", "version string 'latest' is reserved and cannot be used"));
    else if (looksLikeVersionRange(version)) out.push(finding("upstream/version", "error", "/version", "version must be a specific version, not a range"));
  }
  const repo = data["repository"];
  if (isRecord(repo)) {
    const url = repo["url"];
    const source = repo["source"];
    if (typeof url !== "string" || url === "") out.push(finding("upstream/repository", "error", "/repository/url", "repository url is required when repository is provided"));
    if (typeof source !== "string" || source === "") out.push(finding("upstream/repository", "error", "/repository/source", "repository source is required when repository is provided"));
    if (typeof url === "string" && typeof source === "string" && url && source) {
      const ok = source === "github" ? GITHUB_URL.test(url) : source === "gitlab" ? GITLAB_URL.test(url) && !(url + "/").includes("/-/") : false;
      if (!ok) out.push(finding("upstream/repository", "error", "/repository/url", "invalid repository URL for its source"));
    }
  }
  const packages = data["packages"];
  if (Array.isArray(packages)) {
    packages.forEach((pkg, i) => {
      if (!isRecord(pkg)) return;
      const id = pkg["identifier"];
      if (typeof id === "string" && id.includes(" ")) out.push(finding("upstream/package", "error", `/packages/${i}/identifier`, "package name cannot contain spaces"));
      const v = pkg["version"];
      if (typeof v === "string") {
        if (v === "latest") out.push(finding("upstream/package", "error", `/packages/${i}/version`, "version string 'latest' is reserved and cannot be used"));
        else if (looksLikeVersionRange(v)) out.push(finding("upstream/package", "error", `/packages/${i}/version`, "version must be a specific version, not a range"));
      }
      const transport = pkg["transport"];
      if (isRecord(transport)) {
        const type = transport["type"];
        const url = transport["url"];
        if (type === "stdio") {
          if (typeof url === "string" && url !== "") out.push(finding("upstream/package", "error", `/packages/${i}/transport/url`, "url must be empty for stdio transport type"));
        } else if (type === "streamable-http" || type === "sse") {
          if (typeof url !== "string" || url === "") out.push(finding("upstream/package", "error", `/packages/${i}/transport/url`, `url is required for ${type} transport type`));
        } else {
          out.push(finding("upstream/package", "error", `/packages/${i}/transport/type`, `unsupported transport type: ${String(type)}`));
        }
      }
    });
  }
  const remotes = data["remotes"];
  if (Array.isArray(remotes)) {
    remotes.forEach((r, i) => {
      if (!isRecord(r)) return;
      const type = r["type"];
      const url = r["url"];
      if (type !== "streamable-http" && type !== "sse") {
        out.push(finding("upstream/remote", "error", `/remotes/${i}/type`, `unsupported transport type for remotes: ${String(type)} (only streamable-http and sse are supported)`));
        return;
      }
      if (typeof url !== "string" || url === "") out.push(finding("upstream/remote", "error", `/remotes/${i}/url`, `url is required for ${type} transport type`));
      else if (!isValidRemoteUrl(url)) out.push(finding("upstream/remote", "error", `/remotes/${i}/url`, "invalid remote URL (https only, no localhost)"));
    });
  }
  const meta = data["_meta"];
  if (isRecord(meta)) {
    const pp = meta["io.modelcontextprotocol.registry/publisher-provided"];
    if (pp !== undefined) {
      const size = Buffer.byteLength(JSON.stringify(pp), "utf8");
      if (size > PUBLISHER_PROVIDED_LIMIT) {
        out.push(finding("upstream/meta-size", "warning", "/_meta", `publisher-provided _meta is ${size} bytes; the official registry refuses more than ${PUBLISHER_PROVIDED_LIMIT} at publish (validators.go validatePublisherExtensions). Fine in this library; strip trust records before publishing upstream`));
      }
    }
  }
  return out;
}

export type UpstreamResult = { valid: boolean; issues: { type: string; path: string; message: string; severity: string; reference: string }[] };

/** POST /v0.1/validate. Only when TK_LIBRARY_ONLINE=1; `fetchImpl` is injectable for tests. */
export async function checkUpstreamOnline(data: unknown, fetchImpl: typeof fetch = fetch): Promise<Finding[]> {
  if (process.env["TK_LIBRARY_ONLINE"] !== "1") return [];
  let result: UpstreamResult;
  try {
    const res = await fetchImpl(UPSTREAM_VALIDATE_URL, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(data) });
    if (!res.ok) return [finding("upstream/online", "warning", "", `registry validate answered HTTP ${res.status}; offline rules stand`)];
    result = (await res.json()) as UpstreamResult;
  } catch (e) {
    return [finding("upstream/online", "warning", "", `registry validate unreachable: ${(e as Error).message}; offline rules stand`)];
  }
  return (result.issues ?? []).map((i) => finding(`upstream/online/${i.reference || i.type}`, i.severity === "error" ? "error" : "warning", i.path ? "/" + i.path.replace(/\./g, "/").replace(/\[(\d+)\]/g, "/$1") : "", i.message));
}

/** `skills-ref validate <dir>` (agentskills reference library 0.1.0), when it
 *  is on PATH and TK_LIBRARY_SKILLS_REF=1. Exit 1 means errors on stderr. */
export async function checkSkillsRef(skillDir: string, exec: typeof execFileAsync = execFileAsync): Promise<Finding[]> {
  if (process.env["TK_LIBRARY_SKILLS_REF"] !== "1") return [];
  try {
    await exec("skills-ref", ["validate", skillDir], { timeout: 30_000 });
    return [];
  } catch (e) {
    const err = e as { code?: unknown; stderr?: string; message: string };
    if (err.code === "ENOENT") return [finding("skill/skills-ref", "warning", "", "skills-ref is not on PATH; the transcribed rules stand")];
    const lines = (err.stderr ?? err.message).split("\n").map((l) => l.trim()).filter((l) => l.startsWith("- "));
    return lines.length ? lines.map((l) => finding("skill/skills-ref", "error", "", l.slice(2))) : [finding("skill/skills-ref", "error", "", "skills-ref validate failed")];
  }
}
