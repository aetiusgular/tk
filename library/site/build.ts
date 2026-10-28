// node library/site/build.ts <out-dir> [catalog-dir]
// Writes <out-dir>/index.html: the library browser over library/catalog/
// (lane J, T5). Validates the catalog first and shows each entry's
// warnings on its card; refuses a catalog with errors, as the exporter does.
// Reads the git commit of the checkout for the footer; runs nothing else
// and touches no network.
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { CATALOG_DIR, catalogHasErrors, loadBrands, loadCatalog, validateCatalog } from "../validate/index.ts";
import { REPO_ROOT } from "../validate/schemas.ts";
import { buildSite, loadHarnesses } from "./model.ts";
import { renderPage } from "./page.ts";

export function build(outDir: string, catalogDir = CATALOG_DIR, commit = gitCommit()): string {
  const catalog = loadCatalog(catalogDir);
  const result = validateCatalog(catalog, loadBrands(catalogDir));
  if (catalogHasErrors(result)) throw new Error("refusing to build the browser over a catalog with errors (node library/cli.ts validate lists them)");
  const site = buildSite(catalog, result.byEntry, loadHarnesses(), commit);
  mkdirSync(outDir, { recursive: true, mode: 0o755 });
  const path = join(outDir, "index.html");
  writeFileSync(path, renderPage(site), { mode: 0o644 });
  return path;
}

function gitCommit(): string {
  try {
    const sha = execFileSync("git", ["-C", REPO_ROOT, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    return /^[0-9a-f]{40}$/.test(sha) ? sha : "unknown";
  } catch {
    return "unknown";
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [out, catalogArg] = process.argv.slice(2);
  if (!out) {
    console.error("usage: node library/site/build.ts <out-dir> [catalog-dir]");
    process.exit(2);
  }
  try {
    console.log(build(resolve(out), catalogArg ? resolve(catalogArg) : CATALOG_DIR));
  } catch (e) {
    console.error(String((e as Error).message));
    process.exit(1);
  }
}
