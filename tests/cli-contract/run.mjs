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

import { existsSync, writeFileSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import path from "node:path";
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

function parseArgs(argv) {
  const out = { codex: null, claude: [], only: null, json: null, timeout: 240 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const next = () => {
      const v = argv[++i];
      if (v === undefined) throw new Error(`${a} needs a value`);
      return v;
    };
    if (a === "--codex") out.codex = next();
    else if (a === "--claude") out.claude.push(next());
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
    console.log("node tests/cli-contract/run.mjs --codex <path> --claude <path> [--claude <path>] [--only ids] [--json file] [--timeout s] [--keep] [--edit-adapter bin]");
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
        item.run(item.harness === "codex" ? { codexPath: item.bin } : { claudePath: item.bin, version: item.cliVersion }),
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
  else rmSync(runRoot, { recursive: true, force: true });
  const bad = results.filter((r) => r.status === "fail" || r.status === "error");
  console.log(`\n${results.length} checks: ${results.filter((r) => r.status === "pass").length} pass, ${results.filter((r) => r.status === "info").length} info, ${bad.length} fail or error, ${results.filter((r) => r.status === "skip").length} skipped`);
  process.exit(bad.length ? 1 : 0);
}

main();
