// Loading library/catalog/ (lane E, T3.1).
//
// Layout, one folder per entry under library/catalog/:
//   <id>/server.json [+ tools.json]          an MCP server
//   <id>/skill.json  + skill/SKILL.md         a skill (ToolHive's layout)
//   <id>/.claude-plugin/plugin.json + ...     a bundle (Claude Code's layout)
//   sets.json, brands.json, README.md         catalog-wide files
import { existsSync, lstatSync, readdirSync, readFileSync, statSync } from "node:fs";
import { basename, join, relative, resolve, sep } from "node:path";
import type { Catalog, Kind, LoadedEntry, ToolsRecord } from "./types.ts";
import { isRecord } from "./types.ts";
import { LIBRARY_DIR, REPO_ROOT, readJson } from "./schemas.ts";
import type { Brand } from "../trust/names.ts";

export const CATALOG_DIR = join(LIBRARY_DIR, "catalog");
export const MAX_MANIFEST_BYTES = 2 * 1024 * 1024;
const TK_REPO_URLS = ["https://github.com/aetiusgular/tk", "https://github.com/aetiusgular/tk.git"];

/** Reads one entry folder. Returns undefined when the folder holds no manifest. */
export function loadEntry(dir: string, repoRoot = REPO_ROOT): LoadedEntry | undefined {
  const id = basename(dir);
  const serverPath = join(dir, "server.json");
  const skillPath = join(dir, "skill.json");
  const bundlePath = join(dir, ".claude-plugin", "plugin.json");
  let kind: Kind;
  let file: string;
  if (existsSync(serverPath)) {
    kind = "server";
    file = "server.json";
  } else if (existsSync(skillPath)) {
    kind = "skill";
    file = "skill.json";
  } else if (existsSync(bundlePath)) {
    kind = "bundle";
    file = ".claude-plugin/plugin.json";
  } else {
    return undefined;
  }
  // Manifests are small; a multi-megabyte file is not one (bounds the parse).
  const size = statSync(join(dir, file)).size;
  if (size > MAX_MANIFEST_BYTES) throw new Error(`${id}/${file} is ${size} bytes; manifests are capped at ${MAX_MANIFEST_BYTES}`);
  const raw = readJson(join(dir, file));
  const data = isRecord(raw) ? raw : {};
  const entry: LoadedEntry = { id, kind, dir, file, data };
  if (kind === "server" && existsSync(join(dir, "tools.json"))) {
    const t = readJson(join(dir, "tools.json"));
    if (isRecord(t)) entry.tools = t as unknown as ToolsRecord;
  }
  if (kind === "skill") {
    const md = findSkillMd(data, dir, repoRoot);
    if (md) entry.skillMd = md;
  }
  return entry;
}

/** The SKILL.md of a skill whose git package points into this repository, or
 *  the conventional skill/SKILL.md beside skill.json. Paths are held inside
 *  the repository root; `..` and absolute subfolders are refused. */
function findSkillMd(data: Record<string, unknown>, dir: string, repoRoot: string): { path: string; text: string } | undefined {
  const candidates: string[] = [];
  const packages = data["packages"];
  if (Array.isArray(packages)) {
    for (const p of packages) {
      if (!isRecord(p) || p["registryType"] !== "git") continue;
      const url = p["url"];
      const sub = p["subfolder"];
      if (typeof url === "string" && TK_REPO_URLS.includes(url) && typeof sub === "string" && !sub.includes("..") && !sub.startsWith("/")) {
        const abs = resolve(repoRoot, sub);
        if (abs === repoRoot || abs.startsWith(repoRoot + sep)) candidates.push(join(abs, "SKILL.md"));
      }
    }
  }
  candidates.push(join(dir, "skill", "SKILL.md"));
  for (const c of candidates) {
    // A link is not followed: the text must sit inside the repository (rules section 5, item 3).
    if (!existsSync(c)) continue;
    const st = lstatSync(c);
    if (!st.isFile() || st.size > MAX_MANIFEST_BYTES) continue;
    return { path: relative(repoRoot, c), text: readFileSync(c, "utf8") };
  }
  return undefined;
}

export function loadCatalog(catalogDir = CATALOG_DIR, repoRoot = REPO_ROOT): Catalog {
  const entries: LoadedEntry[] = [];
  for (const name of readdirSync(catalogDir).sort()) {
    const dir = join(catalogDir, name);
    if (!statSync(dir).isDirectory()) continue;
    const entry = loadEntry(dir, repoRoot);
    if (entry) entries.push(entry);
  }
  const setsPath = join(catalogDir, "sets.json");
  const sets = existsSync(setsPath) ? (readJson(setsPath) as Catalog["sets"]) : { version: 1 as const, sets: {} };
  const brandsPath = join(catalogDir, "brands.json");
  const brands = existsSync(brandsPath) ? (readJson(brandsPath) as { brands: Brand[] }).brands : [];
  return { dir: catalogDir, entries, sets, brands: brands.map((b) => b.name) };
}

// The brand list with lane H's seeds merged is library/trust/identity.ts
// loadBrands, re-exported by validate/index.ts (integration w2).
