#!/usr/bin/env node
// Runs the harness contract checks against the real CLIs with fake model
// servers on loopback.
//
//   node tests/cli-contract/run.mjs --codex <path> --claude <path> [--claude <path2>]
//       [--only <id,id>] [--json <file>] [--timeout <seconds per check>]
//       [--edit-adapter <path to the edit_adapter_proxy example binary>]
//
// A check whose CLI is not given, or not found, is skipped. Exit code 0 when
// every check that ran has status pass or info, 1 when one failed or errored,
// 2 on bad arguments.

import { existsSync, writeFileSync, mkdirSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { runsDir } from "./lib/codexDriver.mjs";
import { codexChecks } from "./checks/codex.mjs";
import { claudeChecks } from "./checks/claude.mjs";
import { editAdapterChecks } from "./checks/editAdapter.mjs";
// [w3-a] imports
import { otelCodexChecks as otelCodexChecks_w3a, otelClaudeChecks as otelClaudeChecks_w3a } from "./checks/otel.mjs";
import { claudeTurnEndChecks } from "./checks/claudeTurnEnd.mjs";
// [w3-b] imports
// [w3-c] imports
import { computerUseClaudeChecks as computerUseClaudeChecks_w3c, computerUseCodexChecks as computerUseCodexChecks_w3c } from "./checks/computerUse.mjs";
// [tk-F] imports: the tk plugin checks (lane F). They import the fake daemon from
// packages/tk-claude-plugin/src/testing/fakeDaemon.ts, so Node 22.18 or later.
import { tkClaudeChecks } from "./checks/tkClaude.mjs";
import { tkCodexChecks } from "./checks/tkCodex.mjs";

// [integration w1] Lane folders. Every .mjs file in checks/<LANE>/ (one
// capital letter: I in wave 2; L, N, P, R in wave 4) exports `checks`, an
// array of { id, title, harness, run }. `run({ binPath, version, codexPath,
// claudePath })` returns { status, finding }. The harness's CLI comes from
// --cli <harness>=<path>, or --codex / --claude. Lanes add files, not lines.
async function laneChecks() {
  const dir = path.join(path.dirname(fileURLToPath(import.meta.url)), "checks");
  const out = [];
  for (const lane of readdirSync(dir, { withFileTypes: true }).filter((d) => d.isDirectory() && /^[A-Z]$/.test(d.name)).map((d) => d.name).sort()) {
    for (const file of readdirSync(path.join(dir, lane)).filter((f) => f.endsWith(".mjs")).sort()) {
      const mod = await import(pathToFileURL(path.join(dir, lane, file)).href);
      if (!Array.isArray(mod.checks)) throw new Error(`checks/${lane}/${file} exports no checks array`);
      for (const c of mod.checks) out.push({ ...c, lane });
    }
  }
  return out;
}

function parseArgs(argv) {
  const out = { codex: null, claude: [], cli: {}, only: null, json: null, timeout: 240 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const next = () => {
      const v = argv[++i];
      if (v === undefined) throw new Error(`${a} needs a value`);
      return v;
    };
    if (a === "--codex") out.codex = next();
    else if (a === "--claude") out.claude.push(next());
    else if (a === "--cli") {
      const v = next();
      const eq = v.indexOf("=");
      if (eq <= 0) throw new Error("--cli takes <harness>=<path>");
      out.cli[v.slice(0, eq)] = v.slice(eq + 1);
    }
    else if (a === "--only") out.only = new Set(next().split(","));
    else if (a === "--json") out.json = next();
    else if (a === "--timeout") out.timeout = Number(next());
    else if (a === "--keep") out.keep = true;
    else if (a === "--edit-adapter") out.editAdapter = next();
    else if (a === "--help" || a === "-h") out.help = true;
    else throw new Error(`unknown argument ${a}`);
  }
  return out;
}

function version(bin) {
  try {
    const PATH = [path.dirname(process.execPath), "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].join(":");
    return execFileSync(bin, ["--version"], { env: { PATH, HOME: "/nonexistent" }, encoding: "utf8", timeout: 20000 }).trim();
  } catch {
    return null;
  }
}

function withTimeout(promise, seconds, label) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out after ${seconds} s`)), seconds * 1000);
    }),
  ]).finally(() => clearTimeout(timer));
}

async function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exit(2);
  }
  if (args.help) {
    console.log("node tests/cli-contract/run.mjs --codex <path> --claude <path> [--claude <path>] [--cli <harness>=<path> ...] [--only ids] [--json file] [--timeout s] [--keep] [--edit-adapter bin]");
    return;
  }
  // Every temp home of this run goes in one folder, removed at the end
  // unless --keep.
  mkdirSync(runsDir(), { recursive: true });
  const runRoot = mkdtempSync(path.join(runsDir(), "run-"));
  process.env.CLI_CONTRACT_TMP = runRoot;
  const plan = [];
  const codexVersion = args.codex && existsSync(args.codex) ? version(args.codex) : null;
  for (const c of codexChecks) plan.push({ ...c, harness: "codex", bin: codexVersion ? args.codex : null, cliVersion: codexVersion });
  // The edit adapter check also needs the adapter binary (session 10).
  if (args.editAdapter) process.env.EDIT_ADAPTER_BIN = path.resolve(args.editAdapter);
  for (const c of editAdapterChecks) plan.push({ ...c, harness: "codex", bin: codexVersion ? args.codex : null, cliVersion: codexVersion });
  // [w3-a] codex-plan
  for (const c of otelCodexChecks_w3a) plan.push({ ...c, harness: "codex", bin: codexVersion ? args.codex : null, cliVersion: codexVersion });
  // [w3-b] codex-plan
  // [w3-c] codex-plan
  for (const c of computerUseCodexChecks_w3c) plan.push({ ...c, harness: "codex", bin: codexVersion ? args.codex : null, cliVersion: codexVersion });
  // [tk-F] codex-plan
  for (const c of tkCodexChecks) plan.push({ ...c, harness: "codex", bin: codexVersion ? args.codex : null, cliVersion: codexVersion });
  const claudes = args.claude.length ? args.claude : [null];
  for (const bin of claudes) {
    const v = bin && existsSync(bin) ? version(bin) : null;
    for (const c of claudeChecks) plan.push({ ...c, harness: "claude", bin: v ? bin : null, cliVersion: v, id: v ? `${c.id}@${v.split(" ")[0]}` : c.id, baseId: c.id });
    // [w3-a] claude-plan
    for (const c of otelClaudeChecks_w3a) plan.push({ ...c, harness: "claude", bin: v ? bin : null, cliVersion: v, id: v ? `${c.id}@${v.split(" ")[0]}` : c.id, baseId: c.id });
    for (const c of claudeTurnEndChecks) plan.push({ ...c, harness: "claude", bin: v ? bin : null, cliVersion: v, id: v ? `${c.id}@${v.split(" ")[0]}` : c.id, baseId: c.id });
    // [w3-b] claude-plan
    // [w3-c] claude-plan
    for (const c of computerUseClaudeChecks_w3c) plan.push({ ...c, harness: "claude", bin: v ? bin : null, cliVersion: v, id: v ? `${c.id}@${v.split(" ")[0]}` : c.id, baseId: c.id });
    // [tk-F] claude-plan
    for (const c of tkClaudeChecks) plan.push({ ...c, harness: "claude", bin: v ? bin : null, cliVersion: v, id: v ? `${c.id}@${v.split(" ")[0]}` : c.id, baseId: c.id });
  }
  // [integration w1] lane folders
  for (const c of await laneChecks()) {
    const bin = c.harness === "codex" ? args.codex : c.harness === "claude" ? (args.claude[0] ?? null) : (args.cli[c.harness] ?? null);
    const v = bin && existsSync(bin) ? version(bin) : null;
    plan.push({ ...c, bin: v ? bin : null, cliVersion: v, id: v ? `${c.id}@${v.split(" ")[0]}` : c.id, baseId: c.id, lane: c.lane });
  }

  const results = [];
  for (const item of plan) {
    if (args.only && !args.only.has(item.id) && !args.only.has(item.baseId ?? item.id)) continue;
    const row = { id: item.id, title: item.title, flag: item.flag, harness: item.harness, cli: item.cliVersion };
    if (!item.bin) {
      row.status = "skip";
      row.finding = `${item.harness} CLI not given or not found`;
      results.push(row);
      console.log(`SKIP  ${item.id}  ${row.finding}`);
      continue;
    }
    const started = Date.now();
    try {
      const out = await withTimeout(
        item.lane
          ? item.run({ binPath: item.bin, version: item.cliVersion, codexPath: args.codex, claudePath: args.claude[0] ?? null })
          : item.run(item.harness === "codex" ? { codexPath: item.bin } : { claudePath: item.bin, version: item.cliVersion }),
        args.timeout,
        item.id,
      );
      Object.assign(row, out);
    } catch (error) {
      row.status = "error";
      row.finding = error?.stack?.split("\n").slice(0, 3).join(" | ") ?? String(error);
    }
    row.seconds = Math.round((Date.now() - started) / 1000);
    results.push(row);
    console.log(`${row.status.toUpperCase().padEnd(5)} ${item.id} (${row.seconds}s)  ${row.finding}`);
  }
  if (args.json) writeFileSync(args.json, `${JSON.stringify(results, null, 2)}\n`);
  if (args.keep) console.log(`Temp homes kept in ${runRoot}`);
  else {
    // A process a check left (Mac test w2, MW6: a `codex app-server` still
    // writing) must not crash the run after its results: retry, then say so.
    try {
      rmSync(runRoot, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    } catch (error) {
      console.log(`Temp homes not removed (${error?.code ?? error}): ${runRoot}. Look for processes still writing there.`);
    }
  }
  const bad = results.filter((r) => r.status === "fail" || r.status === "error");
  console.log(`\n${results.length} checks: ${results.filter((r) => r.status === "pass").length} pass, ${results.filter((r) => r.status === "info").length} info, ${bad.length} fail or error, ${results.filter((r) => r.status === "skip").length} skipped`);
  process.exit(bad.length ? 1 : 0);
}

main();
