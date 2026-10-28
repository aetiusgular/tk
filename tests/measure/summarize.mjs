#!/usr/bin/env node
// Reads one or more results.jsonl files from run.mjs and prints the tables
// for the status doc: per condition, then per task.
//
//   node summarize.mjs <results.jsonl> [more.jsonl ...] [--json]
import { readFileSync } from "node:fs";

const files = process.argv.slice(2).filter((a) => !a.startsWith("--"));
const asJson = process.argv.includes("--json");
const rows = files.flatMap((f) =>
  readFileSync(f, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l)),
);
const order = ["pi-deferred", "pi-direct", "cc-unset", "cc-auto", "cc-false", "cc-true"];
const conds = [...new Set(rows.map((r) => r.cond))].sort((a, b) => order.indexOf(a) - order.indexOf(b));
const tasks = [...new Set(rows.map((r) => r.task))].sort();

const mean = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : NaN);
const median = (xs) => {
  if (!xs.length) return NaN;
  const s = [...xs].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
};
const pct = (n, d) => (d ? `${((100 * n) / d).toFixed(0)}%` : "n/a");
const f0 = (x) => (Number.isFinite(x) ? Math.round(x).toLocaleString("en-US") : "n/a");
const f1 = (x) => (Number.isFinite(x) ? x.toFixed(1) : "n/a");

const byCond = {};
for (const c of conds) {
  const rs = rows.filter((r) => r.cond === c);
  const mcp = rs.filter((r) => r.kind === "mcp");
  const ctl = rs.filter((r) => r.kind === "control");
  const perTask = tasks.map((t) => rs.filter((r) => r.task === t));
  const tasksAll = perTask.filter((x) => x.length && x.every((r) => r.pass)).length;
  const tasksAny = perTask.filter((x) => x.some((r) => r.pass)).length;
  byCond[c] = {
    runs: rs.length,
    pass: rs.filter((r) => r.pass).length,
    mcpRuns: mcp.length,
    mcpPass: mcp.filter((r) => r.pass).length,
    controlRuns: ctl.length,
    controlPass: ctl.filter((r) => r.pass).length,
    tasks: perTask.filter((x) => x.length).length,
    tasksAllK: tasksAll,
    tasksAnyK: tasksAny,
    mcpNoCall: mcp.filter((r) => r.mcpCalls === 0).length,
    missedCallsMean: mean(mcp.map((r) => r.mcpCallsMissed)),
    searchCallsMean: mean(rs.map((r) => r.searchCalls)),
    firstToolsMean: mean(rs.map((r) => r.firstRequest?.tools).filter(Number.isFinite)),
    firstToolBytesMean: mean(rs.map((r) => r.firstRequest?.toolBytes).filter(Number.isFinite)),
    firstPromptTokensMean: mean(rs.map((r) => r.firstRequest?.promptTokens).filter(Number.isFinite)),
    maxPromptTokensMean: mean(rs.map((r) => r.maxPromptTokens).filter(Number.isFinite)),
    maxToolsDeclaredMean: mean(rs.map((r) => r.maxToolsDeclared)),
    modelRequestsMean: mean(rs.map((r) => r.modelRequests)),
    completionTokensMean: mean(rs.map((r) => r.completionTokensSum)),
    secondsMedian: median(rs.map((r) => r.ms / 1000)),
    secondsMean: mean(rs.map((r) => r.ms / 1000)),
    timeouts: rs.filter((r) => r.timedOut).length,
    requestErrors: rs.filter((r) => (r.modelRequestErrors || []).length).length,
  };
}

if (asJson) {
  console.log(JSON.stringify({ conditions: byCond }, null, 2));
  process.exit(0);
}

const L = [];
L.push(`| Condition | Runs | Pass | MCP tasks pass | Controls pass | Tasks passed k of k | Tasks passed 1 of k | MCP runs with no MCP call | Missed MCP calls per run | tool_search calls per run | Tools declared, first request | Tool bytes, first request | Prompt tokens, first request | Largest prompt per run | Model requests per run | Output tokens per run | Seconds, median (mean) | Timeouts |`);
L.push(`|${"---|".repeat(18)}`);
for (const c of conds) {
  const s = byCond[c];
  L.push(
    `| ${c} | ${s.runs} | ${s.pass} (${pct(s.pass, s.runs)}) | ${s.mcpPass}/${s.mcpRuns} (${pct(s.mcpPass, s.mcpRuns)}) | ${s.controlPass}/${s.controlRuns} | ${s.tasksAllK}/${s.tasks} | ${s.tasksAnyK}/${s.tasks} | ${s.mcpNoCall} | ${f1(s.missedCallsMean)} | ${f1(s.searchCallsMean)} | ${f1(s.firstToolsMean)} | ${f0(s.firstToolBytesMean)} | ${f0(s.firstPromptTokensMean)} | ${f0(s.maxPromptTokensMean)} | ${f1(s.modelRequestsMean)} | ${f0(s.completionTokensMean)} | ${f1(s.secondsMedian)} (${f1(s.secondsMean)}) | ${s.timeouts} |`,
  );
}
L.push("");
L.push(`| Task | ${conds.join(" | ")} |`);
L.push(`|${"---|".repeat(conds.length + 1)}`);
for (const t of tasks) {
  const cells = conds.map((c) => {
    const rs = rows.filter((r) => r.cond === c && r.task === t);
    return rs.length ? `${rs.filter((r) => r.pass).length}/${rs.length}` : "";
  });
  L.push(`| ${t} | ${cells.join(" | ")} |`);
}
console.log(L.join("\n"));
