// tk library command line (lane E).
//   node library/cli.ts validate [catalog-dir]
//   node library/cli.ts export <set|all> <out-dir> [catalog-dir]
// Runs on Node 22.18+ with type stripping; no build step. Exit 1 on an error
// finding, 2 on usage. Nothing here touches the network or runs a program
// unless asked: TK_LIBRARY_ONLINE=1 posts each server to the registry's
// /v0.1/validate, TK_LIBRARY_SKILLS_REF=1 runs skills-ref on each skill
// (validate only; integration w1 fix pass, box test BX1).
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { CATALOG_DIR, catalogHasErrors, loadBrands, loadCatalog, optInChecks, validateCatalog } from "./validate/index.ts";
import type { Finding } from "./validate/index.ts";
import { PLACEHOLDER_SHA, exportSet, type ExportFile } from "./export/index.ts";
import { REPO_ROOT } from "./validate/schemas.ts";

function usage(): never {
  console.error("usage: node library/cli.ts validate [catalog-dir]\n       node library/cli.ts export <set|all> <out-dir> [catalog-dir]");
  process.exit(2);
}

function print(prefix: string, findings: Finding[]): void {
  for (const f of findings) console.log(`${f.severity === "error" ? "error  " : "warning"} ${prefix}${f.path ? ` ${f.path}` : ""}: [${f.rule}] ${f.message}`);
}

const [cmd, ...rest] = process.argv.slice(2);
if (cmd === "validate") {
  const catalogDir = rest[0] ? resolve(rest[0]) : CATALOG_DIR;
  const catalog = loadCatalog(catalogDir);
  const result = validateCatalog(catalog, loadBrands(catalogDir));
  for (const [id, extra] of await optInChecks(catalog)) result.byEntry.set(id, [...(result.byEntry.get(id) ?? []), ...extra]);
  let errors = 0;
  let warnings = 0;
  for (const [id, findings] of result.byEntry) {
    print(id, findings);
    for (const f of findings) f.severity === "error" ? errors++ : warnings++;
  }
  print("catalog", result.catalog);
  for (const f of result.catalog) f.severity === "error" ? errors++ : warnings++;
  console.log(`${catalog.entries.length} entries, ${errors} errors, ${warnings} warnings`);
  process.exit(catalogHasErrors(result) ? 1 : 0);
} else if (cmd === "export") {
  const [setId, outDir, catalogArg] = rest;
  if (!setId || !outDir) usage();
  const catalogDir = catalogArg ? resolve(catalogArg) : CATALOG_DIR;
  const catalog = loadCatalog(catalogDir);
  const result = validateCatalog(catalog, loadBrands(catalogDir));
  if (catalogHasErrors(result)) {
    for (const [id, findings] of result.byEntry) print(id, findings.filter((f) => f.severity === "error"));
    print("catalog", result.catalog.filter((f) => f.severity === "error"));
    console.error("refusing to export a catalog with errors");
    process.exit(1);
  }
  const sets = setId === "all" ? Object.keys(catalog.sets.sets) : [setId];
  // Git sources are pinned to the commit the export was made from (Plugin4Shell, note AP section 5).
  let sha = PLACEHOLDER_SHA;
  try {
    sha = execFileSync("git", ["-C", REPO_ROOT, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    if (!/^[0-9a-f]{40}$/.test(sha)) sha = PLACEHOLDER_SHA;
  } catch {
    // not a git checkout
  }
  if (sha === PLACEHOLDER_SHA) console.error("warning: not a git checkout; git sources are pinned to a placeholder sha and will not install");
  const { writeFileSync } = await import("node:fs");
  for (const s of sets) {
    if (!catalog.sets.sets[s]) {
      console.error(`no set '${s}' in ${catalogDir}/sets.json`);
      process.exit(2);
    }
    const files: ExportFile[] = exportSet(catalog, s, { sha });
    for (const f of files) {
      const path = resolve(outDir, f.path);
      mkdirSync(resolve(path, ".."), { recursive: true });
      writeFileSync(path, f.content);
      console.log(`wrote ${path}`);
    }
  }
} else {
  usage();
}
