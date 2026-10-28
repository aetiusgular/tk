#!/usr/bin/env node
// Copies the shared hook core from packages/tk-claude-plugin/src/core/ (the
// canonical copy) into the Codex and OpenCode plugins. Each harness copies a
// plugin's own folder into its cache and nothing outside it, so each plugin
// carries the core itself. A test in each plugin checks the copies are
// byte-identical (src/core.vendored.test.ts).
//
//   node packages/tk-claude-plugin/scripts/sync-core.mjs          copy
//   node packages/tk-claude-plugin/scripts/sync-core.mjs --check  exit 1 on any difference

import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const source = path.join(here, "..", "src", "core");
// tk-gemini-extension and tk-kilo-plugin added by integration w1 for wave 2 lane I;
// tk-hermes-plugin and tk-openclaw-plugin by integration w2 (lane I's request 3).
const targets = ["tk-codex-plugin", "tk-opencode-plugin", "tk-gemini-extension", "tk-kilo-plugin", "tk-hermes-plugin", "tk-openclaw-plugin"].map((p) => path.join(here, "..", "..", p, "src", "core"));
const check = process.argv.includes("--check");

// Tests stay with the canonical copy.
const files = readdirSync(source).filter((f) => f.endsWith(".ts") && !f.endsWith(".test.ts")).sort();
let differences = 0;
for (const target of targets) {
  if (!check) mkdirSync(target, { recursive: true });
  const present = existsSync(target) ? readdirSync(target).filter((f) => f.endsWith(".ts")) : [];
  for (const extra of present.filter((f) => !files.includes(f))) {
    differences++;
    if (check) console.error(`extra: ${path.join(target, extra)}`);
    else rmSync(path.join(target, extra));
  }
  for (const f of files) {
    const want = readFileSync(path.join(source, f));
    const dest = path.join(target, f);
    const have = existsSync(dest) ? readFileSync(dest) : null;
    if (have && have.equals(want)) continue;
    differences++;
    if (check) console.error(`differs: ${dest}`);
    else writeFileSync(dest, want);
  }
}
if (check) {
  if (differences) process.exit(1);
  console.log(`core copies match (${files.length} files x ${targets.length} plugins)`);
} else {
  console.log(`synced ${files.length} core files into ${targets.length} plugins (${differences} written or removed)`);
}
