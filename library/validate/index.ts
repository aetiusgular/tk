// The tk library validator (lane E, T3.1). Rules, by family:
//   schema/*    the entry's shape (vendored upstream server.json 2025-12-11,
//               ToolHive's skill.json, Claude Code's plugin.json, tk's _meta)
//   upstream/*  the official registry's semantic rules, transcribed (upstream.ts)
//   skill/*     SKILL.md per the Agent Skills specification (skill-md.ts)
//   lib/*       the library's own rules (plan T3.1; note AP section 8):
//               unique tool names across the catalog once mangled, explicit
//               annotations, outputSchema, the reject list of
//               contracts/policy.md section 3, no install-time execution, no
//               remote code loading, no literal secret, lookalike names
// Nothing here reads the network or runs a program unless an environment
// variable turns it on (upstream.ts).
import { existsSync, lstatSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { checkName, type Brand, type Reserved } from "../trust/names.ts";
import { loadReserved } from "../trust/identity.ts";
import { hostsOf, imagesOf, namespaceOf, packagesOf } from "./identity.ts";
import { SERVER_ID, TOOL_NAME_CHARSET, mangle, policyKey } from "./names.ts";
import { SECRET_LIKE, isEnvReference, scanRejects } from "./rejects.ts";
import { REPO_ROOT, checkTkSchema, checkUpstreamServerSchema } from "./schemas.ts";
import { allowedToolsOf, parseSkillMd, validateSkillFrontmatter } from "./skill-md.ts";
import type { Catalog, Finding, Kind, LoadedEntry, TkMeta } from "./types.ts";
import { finding, getMeta, hasErrors, isRecord } from "./types.ts";
import { checkSkillsRef, checkUpstreamOnline, checkUpstreamSemantics } from "./upstream.ts";

export type { Finding, Kind, LoadedEntry, Catalog } from "./types.ts";
export { hasErrors } from "./types.ts";
export { loadCatalog, loadEntry, CATALOG_DIR } from "./catalog.ts";
// The brand seeds: lane E's catalog list merged with lane H's generated ones,
// and lane H's reserved names (integration w2, lane H's request).
export { loadBrands, loadReserved } from "../trust/identity.ts";
export { hostsOf, imagesOf, namespaceOf, packagesOf } from "./identity.ts";

/** Permissive licences the library includes (ToolHive's and Docker's rule; plan section 11, call 4). SPDX ids. */
export const PERMISSIVE_LICENSES = new Set(["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "0BSD", "MPL-2.0", "Unlicense", "CC0-1.0", "Zlib", "BSL-1.0"]);

/** Note AE's measured ceiling for the 27B (plan R3, corner case 61). */
export const DIRECT_TOOL_CEILING = 10;

/** Well-known token shapes a manifest must never carry as a literal. */
const TOKEN_SHAPES: RegExp[] = [/\bghp_[A-Za-z0-9]{20,}/, /\bgithub_pat_[A-Za-z0-9_]{20,}/, /\bAKIA[0-9A-Z]{16}\b/, /\bsk-[A-Za-z0-9_-]{20,}/, /\bxox[baprs]-[A-Za-z0-9-]{10,}/, /\bglpat-[A-Za-z0-9_-]{20,}/];

export type ValidateOptions = { brands?: Brand[]; reserved?: Reserved[]; catalog?: Catalog };

/** Validate one entry on its own: schema, upstream rules, library rules that
 *  need no other entry. Catalog-wide rules are in `validateCatalog`. */
/** `$`, `{env:` and `{file:`: forms a harness expands (lane C, spec.rs). */
const EXPANDS = /\$|\{env:|\{file:/i;
/** An upstream placeholder value: an optional literal prefix and one `{var}` at the end (what tk connect accepts). */
const UPSTREAM_REF = /^[^{}$]*\{[A-Za-z_][A-Za-z0-9_]*\}$/;

export function validateEntry(entry: LoadedEntry, opts: ValidateOptions = {}): Finding[] {
  const out: Finding[] = [];
  const { kind, data } = entry;

  // 1. Shape.
  if (kind === "server") {
    out.push(...checkUpstreamServerSchema(data));
    out.push(...checkTkSchema("tk-server", data));
    out.push(...checkUpstreamSemantics(data));
    if (entry.tools) out.push(...checkTkSchema("tools", entry.tools).map((f) => ({ ...f, path: "tools.json" + f.path })));
  } else if (kind === "skill") {
    out.push(...checkTkSchema("tk-skill", data));
  } else {
    out.push(...checkTkSchema("tk-bundle", data));
  }
  const meta = getMeta(kind, data);
  if (!meta) {
    out.push(finding("lib/meta", "error", "", `no tk entry object (${kind === "bundle" ? "metadata" : "_meta"}[io.github.aetiusgular.tk])`));
    return out;
  }

  // 2. Identity: the folder id, the manifest name and the harness-safe server id.
  const shortName = shortNameOf(kind, data);
  if (!SERVER_ID.test(entry.id)) out.push(finding("lib/id", "error", "", `entry id '${entry.id}' must match ${SERVER_ID} (letters, digits, hyphens; Gemini splits server names at '_', note AP section 4)`));
  if (shortName !== undefined && shortName !== entry.id) out.push(finding("lib/id-matches-name", "error", "/name", `the folder is '${entry.id}' but the manifest name ends in '${shortName}'; they must match so tk connect and the policy file name one thing`));

  // 3. The reject list over everything the entry ships as text.
  const texts: { where: string; text: string }[] = [{ where: entry.file, text: JSON.stringify(data) }];
  if (entry.tools) texts.push({ where: "tools.json", text: JSON.stringify(entry.tools) });
  if (entry.skillMd) texts.push({ where: entry.skillMd.path, text: entry.skillMd.text });
  if (kind === "bundle") texts.push(...bundleTexts(entry.dir, data));
  for (const t of texts) {
    for (const hit of scanRejects(t.text)) out.push(finding("lib/reject", "error", t.where, hit.reason));
  }

  // 4. Secrets: a secret-like key with a literal value, or a token shape anywhere.
  // server.json carries headers[] and environmentVariables[] as {name, value|default, isSecret}
  // (upstream KeyValueInput), so those items are read by their own name.
  walkObjects(data, "", (path, obj) => {
    const name = obj["name"];
    if (typeof name !== "string") return;
    const secret = obj["isSecret"] === true || SECRET_LIKE.test(name) || /^(authorization|x-api-key|api-key|cookie)$/i.test(name);
    for (const field of ["value", "default"]) {
      const v = obj[field];
      if (secret && typeof v === "string" && v !== "" && !isEnvReference(v) && !/^bearer \$\{[A-Za-z_][A-Za-z0-9_]*\}$/i.test(v) && !UPSTREAM_REF.test(v)) {
        out.push(finding("lib/literal-secret", "error", `${path}/${field}`, `'${name}' holds a literal value; only a \${VAR} reference may appear in a manifest`));
      }
    }
  });
  walkStrings(data, "", (path, key, value) => {
    if (SECRET_LIKE.test(key) && value !== "" && !isEnvReference(value) && !path.includes("/description") && !/^(name|env)$/i.test(key)) {
      out.push(finding("lib/literal-secret", "error", path, `'${key}' holds a literal value; only a \${VAR} reference may appear in a manifest`));
    }
    if (TOKEN_SHAPES.some((re) => re.test(value))) out.push(finding("lib/literal-secret", "error", path, "a value looks like an access token"));
  });

  // 4b. Placeholders (integration w1). A server entry is upstream server.json:
  // its placeholders are `{var}` declared in the item's `variables`
  // (schema InputWithVariables). `$`, `{env:` and `{file:` are what harnesses
  // expand from your environment or disk (Claude Code, Gemini, Qwen, Crush,
  // Pi, OpenCode; lane C's recordings), so tk connect refuses them and so does
  // the library. `_meta` and descriptions are not values and are skipped.
  if (kind === "server") {
    walkObjects(data, "", (path, obj) => {
      if (path.startsWith("/_meta")) return;
      for (const field of ["value", "default", "url", "identifier"]) {
        const v = obj[field];
        if (typeof v === "string" && EXPANDS.test(v)) {
          out.push(finding("lib/expansion", "error", `${path}/${field}`, `'${field}' holds $, {env: or {file:, which harnesses expand from your environment or disk; use an upstream {variable} declared in variables`));
        }
      }
      for (const field of ["value", "default"]) {
        const v = obj[field];
        if (typeof v !== "string") continue;
        const vars = obj["variables"];
        for (const m of v.matchAll(/\{([A-Za-z_][A-Za-z0-9_]*)\}/g)) {
          if (!isRecord(vars) || !Object.hasOwn(vars, m[1]!)) out.push(finding("lib/variables", "error", `${path}/${field}`, `'{${m[1]}}' is not declared in this item's variables (upstream InputWithVariables)`));
        }
      }
    });
  }

  // 5. The tk data object.
  if (meta.runner.kind === "image" && !meta.runner.imageDigest && meta.tier !== "mirrored") {
    out.push(finding("lib/image-digest", "warning", "/_meta/runner", "an image runner without imageDigest is not pinned; tk connect cannot verify what it starts"));
  }
  if (meta.entrySkill === undefined && kind === "server" && meta.exposure.default !== "hidden") {
    out.push(finding("lib/entry-skill", "warning", "/_meta/entrySkill", "no entry skill: the catalog will cost tool definitions instead of about 100 tokens (plan R3)"));
  }
  if (meta.trust.liveness.state === "dead") out.push(finding("lib/liveness", "warning", "/_meta/trust/liveness", "the last liveness probe found the remote dead (corner case 58); the card says so and tk connect warns"));

  // 6. Tools: names, annotations, outputSchema.
  if (kind === "server") {
    if (!entry.tools) {
      out.push(finding("lib/tools-record", meta.tier === "own" ? "error" : "warning", "", "no tools.json beside server.json: the catalog-wide name check and the annotation check cannot run for this entry"));
    } else {
      const seen = new Map<string, string>();
      for (const [i, tool] of entry.tools.tools.entries()) {
        const p = `tools.json/tools/${i}`;
        if (!TOOL_NAME_CHARSET.test(tool.name)) out.push(finding("lib/tool-name-charset", "warning", p, `tool '${tool.name}' has characters outside [A-Za-z0-9_]; harnesses mangle it to '${mangle(tool.name)}' and the policy file must use that spelling`));
        const m = mangle(tool.name);
        const prior = seen.get(m);
        if (prior !== undefined) out.push(finding("lib/tool-name-unique", "error", p, `tools '${prior}' and '${tool.name}' are one name '${m}' once mangled`));
        seen.set(m, tool.name);
        const annotations = tool.annotations ?? meta.annotationsOverride?.[tool.name] ?? meta.annotationsOverride?.[m];
        if (!annotations || annotations.readOnlyHint === undefined) {
          out.push(finding("lib/annotations", "error", p, `tool '${tool.name}' has no readOnlyHint, from the server or from annotationsOverride; the gate and the plan turn need it (plan R3; case 36)`));
        }
        if (!tool.outputSchema) {
          // An error only for code this repository ships; a record of a third party's server is a warning.
          out.push(finding("lib/output-schema", authoredHere(data) ? "error" : "warning", p, `tool '${tool.name}' declares no outputSchema (plan R3: structuredContent is how outputs chain)`));
        }
      }
      if (meta.exposure.default === "direct" && entry.tools.tools.length > DIRECT_TOOL_CEILING) {
        out.push(finding("lib/exposure-direct", "warning", "/_meta/exposure/default", `${entry.tools.tools.length} tools exposed direct by default; the 27B holds about ${DIRECT_TOOL_CEILING} (note AE; corner case 61)`));
      }
      if (meta.annotationsOverride) {
        const names = new Set(entry.tools.tools.map((t) => t.name));
        const mangled = new Set(entry.tools.tools.map((t) => mangle(t.name)));
        for (const k of Object.keys(meta.annotationsOverride)) {
          if (!names.has(k) && !mangled.has(k)) out.push(finding("lib/annotations-override", "warning", `/_meta/annotationsOverride/${k}`, `overrides a tool the record does not list`));
        }
      }
    }
  }

  // 7. Skills.
  if (kind === "skill") {
    if (!entry.skillMd) {
      out.push(finding("skill/missing", "error", "", "SKILL.md not found: the git package must point into this repository, or skill/SKILL.md must sit beside skill.json"));
    } else {
      const { parsed, findings } = parseSkillMd(entry.skillMd.text);
      out.push(...findings.map((f) => ({ ...f, path: entry.skillMd!.path + f.path })));
      if (parsed) {
        const dirName = entry.skillMd.path.split("/").at(-2);
        out.push(...validateSkillFrontmatter(parsed.fields, dirName).map((f) => ({ ...f, path: entry.skillMd!.path + f.path })));
        const name = parsed.fields["name"];
        if (typeof name === "string" && name !== data["name"]) out.push(finding("skill/name-matches-index", "error", "/name", `skill.json names '${String(data["name"])}' but SKILL.md names '${name}'`));
        if (parsed.body.trim() === "") out.push(finding("skill/empty-body", "error", entry.skillMd.path, "SKILL.md has no instructions after the frontmatter"));
      }
    }
    checkLicense(kind, data["license"], meta, out);
  }

  // 8. Bundles.
  if (kind === "bundle") {
    for (const skillDir of bundleSkillDirs(entry.dir, data)) {
      const md = join(skillDir, "SKILL.md");
      if (!existsSync(md)) continue;
      const rel = md.slice(entry.dir.length + 1);
      const { parsed, findings } = parseSkillMd(readFileSync(md, "utf8"));
      out.push(...findings.map((f) => ({ ...f, path: rel + f.path })));
      if (parsed) out.push(...validateSkillFrontmatter(parsed.fields, skillDir.split(sep).at(-1)).map((f) => ({ ...f, path: rel + f.path })));
    }
    const servers = data["mcpServers"];
    if (isRecord(servers)) {
      for (const [name, cfg] of Object.entries(servers)) {
        if (!SERVER_ID.test(name.toLowerCase())) out.push(finding("lib/id", "error", `/mcpServers/${name}`, `server name '${name}' must match ${SERVER_ID}`));
        if (isRecord(cfg) && typeof cfg["command"] === "string" && meta.runner.kind !== "host") {
          out.push(finding("lib/host-process", "error", `/mcpServers/${name}`, "a bundle that starts a local command must declare runner {kind: host, note: 'runs as you'} (plan R2)"));
        }
      }
    }
    checkLicense(kind, data["license"], meta, out);
  }

  // 9. Names against the brand seeds and the reserved list: lane H's
  // checkName (rules trust/lookalike, trust/reserved, trust/reserved-prefix,
  // trust/mixed-script, trust/punycode-namespace) on the gate's own fold,
  // in place of lane E's hand-picked subset (integration w2; plan T4.3).
  if ((opts.brands && opts.brands.length > 0) || (opts.reserved && opts.reserved.length > 0)) {
    const identity = { namespace: namespaceOf(kind, data), hosts: hostsOf(data), images: imagesOf(data, meta), packages: packagesOf(data) };
    const title = typeof data["title"] === "string" ? (data["title"] as string) : undefined;
    const description = typeof data["description"] === "string" ? (data["description"] as string) : undefined;
    out.push(...checkName({ id: entry.id, ...(title !== undefined ? { title } : {}), ...(description !== undefined ? { description } : {}), identity }, { brands: opts.brands ?? [], reserved: opts.reserved ?? [] }));
  }

  return out;
}

/** Catalog-wide rules, plus every entry's own findings keyed by id. */
export function validateCatalog(catalog: Catalog, brands: Brand[], reserved: Reserved[] = loadReserved()): { byEntry: Map<string, Finding[]>; catalog: Finding[] } {
  const byEntry = new Map<string, Finding[]>();
  for (const e of catalog.entries) byEntry.set(e.id, validateEntry(e, { brands, reserved, catalog }));
  const out: Finding[] = [];

  // Tool names unique across the catalog once mangled.
  const toolOwners = new Map<string, string>();
  const serverIds = new Map<string, string>();
  const skillIds = new Set<string>();
  for (const e of catalog.entries) {
    if (e.kind === "skill") skillIds.add(e.id);
    if (e.kind !== "server") continue;
    // Pi counts server names that differ only in - and _ as one server (docs/mcp.md at 1.4.0).
    const collapsed = e.id.replace(/[-_]/g, "");
    const prior = serverIds.get(collapsed);
    if (prior) out.push(finding("lib/server-id-unique", "error", e.id, `'${e.id}' and '${prior}' are one server name to Pi (differ only in - and _)`));
    serverIds.set(collapsed, e.id);
    for (const t of e.tools?.tools ?? []) {
      const key = policyKey(e.id, t.name);
      const bare = mangle(t.name);
      const owner = toolOwners.get(bare);
      if (owner && owner !== e.id) out.push(finding("lib/tool-name-unique", "error", `${e.id}/tools.json`, `tool '${t.name}' collides with '${bare}' of '${owner}' across the catalog; a harness that flattens server names (OpenCode, ToolHive vMCP) cannot tell them apart`));
      toolOwners.set(bare, e.id);
      toolOwners.set(key, e.id);
    }
  }
  // entrySkill and allowedTools point at things the catalog holds.
  for (const e of catalog.entries) {
    const meta = getMeta(e.kind, e.data);
    if (meta?.entrySkill && !skillIds.has(meta.entrySkill)) out.push(finding("lib/entry-skill", "error", e.id, `entrySkill '${meta.entrySkill}' is not a skill in the catalog`));
    if (e.kind === "skill") {
      const listed = Array.isArray(e.data["allowedTools"]) ? (e.data["allowedTools"] as unknown[]).filter((x): x is string => typeof x === "string") : [];
      const fromMd = e.skillMd ? allowedToolsOf(parseSkillMd(e.skillMd.text).parsed?.fields ?? {}) : [];
      for (const ref of [...listed, ...fromMd]) {
        const [server, tool] = ref.split("/");
        if (!server || !tool) continue;
        if (!toolOwners.has(policyKey(server, tool))) out.push(finding("lib/allowed-tools-known", "warning", `${e.id}/allowedTools`, `'${ref}' names a tool the catalog does not record (ToolHive requires every referenced server to be in the catalog)`));
      }
    }
  }
  // Sets: the file's shape, then every entry it names.
  out.push(...checkTkSchema("sets", catalog.sets).map((f) => ({ ...f, path: "sets.json" + f.path })));
  const ids = new Set(catalog.entries.map((e) => e.id));
  for (const [setId, set] of Object.entries(catalog.sets.sets)) {
    for (const id of set.entries) if (!ids.has(id)) out.push(finding("lib/set-entry", "error", `sets.json/${setId}`, `set names '${id}', which is not in the catalog`));
  }
  return { byEntry, catalog: out };
}

/** The two checks that leave the process, each only when its variable is
 *  set: every server through the registry's `/v0.1/validate`
 *  (`TK_LIBRARY_ONLINE=1`) and every skill folder holding a SKILL.md
 *  through `skills-ref validate` (`TK_LIBRARY_SKILLS_REF=1`). The command
 *  line merges them into `validateCatalog`'s findings; until the box test
 *  found it (BX1), nothing called them outside their unit tests. */
export async function optInChecks(
  catalog: Catalog,
  deps: { fetchImpl?: typeof fetch; exec?: Parameters<typeof checkSkillsRef>[1]; repoRoot?: string } = {},
): Promise<Map<string, Finding[]>> {
  const out = new Map<string, Finding[]>();
  const add = (id: string, f: Finding[]) => {
    if (f.length) out.set(id, [...(out.get(id) ?? []), ...f]);
  };
  for (const e of catalog.entries) {
    if (e.kind === "server") add(e.id, await checkUpstreamOnline(e.data, deps.fetchImpl));
    // skillMd.path is relative to the repository (catalog.ts); resolved, so
    // skills-ref never reads the caller's working folder and a name cannot
    // start with `-` (fix pass security review, R7).
    if (e.kind === "skill" && e.skillMd) add(e.id, await checkSkillsRef(resolve(deps.repoRoot ?? REPO_ROOT, dirname(e.skillMd.path)), deps.exec));
  }
  return out;
}

export function catalogHasErrors(result: { byEntry: Map<string, Finding[]>; catalog: Finding[] }): boolean {
  if (hasErrors(result.catalog)) return true;
  for (const f of result.byEntry.values()) if (hasErrors(f)) return true;
  return false;
}

// ---- helpers

/** True when the entry's repository is this one: the library authors the server and answers for its schema. */
function authoredHere(data: Record<string, unknown>): boolean {
  const repo = data["repository"];
  return isRecord(repo) && typeof repo["url"] === "string" && /^https:\/\/github\.com\/aetiusgular\/tk(\.git)?\/?$/.test(repo["url"]);
}

function shortNameOf(kind: Kind, data: Record<string, unknown>): string | undefined {
  const name = data["name"];
  if (typeof name !== "string") return undefined;
  return kind === "server" ? name.split("/").at(-1) : name;
}

function checkLicense(kind: Kind, license: unknown, meta: TkMeta, out: Finding[]): void {
  if (typeof license === "string") {
    if (!PERMISSIVE_LICENSES.has(license)) out.push(finding("lib/license", "error", "/license", `'${license}' is not a permissive licence the library includes (README, inclusion rule)`));
  } else if (meta.tier !== "mirrored") {
    out.push(finding("lib/license", "warning", "/license", `no license on this ${kind}; the inclusion rule is permissive-only`));
  }
}

function walkObjects(v: unknown, path: string, visit: (path: string, obj: Record<string, unknown>) => void): void {
  if (Array.isArray(v)) v.forEach((x, i) => walkObjects(x, `${path}/${i}`, visit));
  else if (isRecord(v)) {
    visit(path, v);
    for (const [k, x] of Object.entries(v)) walkObjects(x, `${path}/${k}`, visit);
  }
}

function walkStrings(v: unknown, path: string, visit: (path: string, key: string, value: string) => void, key = ""): void {
  if (typeof v === "string") visit(path, key, v);
  else if (Array.isArray(v)) v.forEach((x, i) => walkStrings(x, `${path}/${i}`, visit, key));
  else if (isRecord(v)) for (const [k, x] of Object.entries(v)) walkStrings(x, `${path}/${k}`, visit, k);
}

/** Every text file a bundle ships that a harness reads: hooks, SKILL.md,
 *  commands and agents, bounded in size and held inside the bundle folder. */
function bundleTexts(dir: string, data: Record<string, unknown>): { where: string; text: string }[] {
  const out: { where: string; text: string }[] = [];
  const root = resolve(dir);
  const walk = (d: string, depth: number) => {
    if (depth > 6) return;
    for (const name of readdirSync(d).sort()) {
      const p = join(d, name);
      const st = lstatSync(p);
      if (st.isSymbolicLink()) continue;
      if (st.isDirectory()) {
        if (name === "node_modules" || name === ".git") continue;
        walk(p, depth + 1);
      } else if (/\.(json|md|sh|mjs|cjs|js|ts|py|toml|ya?ml)$/.test(name) && st.size <= 512 * 1024 && p.startsWith(root + sep)) {
        if (p === join(root, ".claude-plugin", "plugin.json")) continue;
        out.push({ where: p.slice(root.length + 1), text: readFileSync(p, "utf8") });
      }
    }
  };
  walk(root, 0);
  void data;
  return out;
}

function bundleSkillDirs(dir: string, data: Record<string, unknown>): string[] {
  const declared = data["skills"];
  const roots = typeof declared === "string" ? [declared] : Array.isArray(declared) ? declared.filter((x): x is string => typeof x === "string") : ["./skills"];
  const out: string[] = [];
  for (const r of roots) {
    if (r.includes("..")) continue;
    const abs = resolve(dir, r);
    if (!(abs === resolve(dir) || abs.startsWith(resolve(dir) + sep)) || !existsSync(abs)) continue;
    if (existsSync(join(abs, "SKILL.md"))) out.push(abs);
    else for (const name of readdirSync(abs).sort()) if (statSync(join(abs, name)).isDirectory()) out.push(join(abs, name));
  }
  return out;
}
