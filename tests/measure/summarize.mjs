#!/usr/bin/env node
// Reads one or more results.jsonl files from run.mjs and prints the tables
// for the status doc: per condition, then per task.
//
//   node summarize.mjs <results.jsonl> [more.jsonl ...] [--requests <state/requests.jsonl>] [--json]
//
// With --requests, the per-request figures are taken from the proxy's log:
// the first turn request is the first POST that is not a count_tokens call
// (Claude Code's `auto` mode counts its MCP tools first), and a request whose
// prompt was read again from the start (under 10% from the server's cache)
// after the first is counted as a full re-read.
import { readFileSync } from "node:fs";

const argv = process.argv.slice(2);
const reqFile = argv.includes("--requests") ? argv[argv.indexOf("--requests") + 1] : null;
const files = argv.filter((a, i) => !a.startsWith("--") && argv[i - 1] !== "--requests");
const asJson = argv.includes("--json");
const jsonl = (f) =>
  readFileSync(f, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => JSON.parse(l));
const rows = files.flatMap(jsonl);
if (reqFile) {
  const byRun = new Map();
  for (const q of jsonl(reqFile)) {
    if (!q.run || q.method !== "POST") continue;
    if (!byRun.has(q.run)) byRun.set(q.run, []);
    byRun.get(q.run).push(q);
  }
  const total = (u) => (u?.prompt_tokens != null ? u.prompt_tokens : u?.input_tokens != null ? u.input_tokens + (u.cache_read_input_tokens || 0) + (u.cache_creation_input_tokens || 0) : null);
  const cached = (u) => (u?.timings?.cache_n != null ? u.timings.cache_n : u?.cache_read_input_tokens ?? null);
  for (const r of rows) {
    const qs = byRun.get(r.run) || [];
    const turns = qs.filter((q) => !q.path.includes("count_tokens") && q.status === 200);
    const counts = qs.filter((q) => q.path.includes("count_tokens"));
    const first = turns[0] || {};
    r.firstRequest = { ...(r.firstRequest || {}), tools: first.tools, toolBytes: first.toolBytes, deferred: first.deferred, promptTokens: total(first.usage) };
    r.countTokens = counts.map((q) => q.usage?.input_tokens ?? null);
    r.fullRereads = turns.slice(1).filter((q) => {
      const t = total(q.usage);
      const c = cached(q.usage);
      return t && c != null && c / t < 0.1;
    }).length;
    r.maxToolsDeclared = Math.max(0, ...turns.map((q) => q.tools || 0));
    r.maxPromptTokens = Math.max(0, ...turns.map((q) => total(q.usage) || 0));
    r.modelRequests = turns.length;
    r.mcpDeclaredFirst = (first.loadedNames || []).filter((n) => n.startsWith("mcp__")).length;
  }
}
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
    fullRereadsMean: mean(rs.map((r) => r.fullRereads).filter(Number.isFinite)),
    mcpDeclaredFirstMean: mean(rs.map((r) => r.mcpDeclaredFirst).filter(Number.isFinite)),
    countTokens: [...new Set(rs.flatMap((r) => r.countTokens || []))],
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
L.push(`| Condition | Runs | Pass | MCP tasks pass | Controls pass | Tasks passed k of k | Tasks passed 1 of k | MCP runs with no MCP call | Missed MCP calls per run | tool_search calls per run | Tools declared, first request | Tool bytes, first request | Prompt tokens, first request | Largest prompt per run | Model requests per run | Output tokens per run | Full prompt re-reads per run | Seconds, median (mean) | Timeouts |`);
L.push(`|${"---|".repeat(19)}`);
for (const c of conds) {
  const s = byCond[c];
  L.push(
    `| ${c} | ${s.runs} | ${s.pass} (${pct(s.pass, s.runs)}) | ${s.mcpPass}/${s.mcpRuns} (${pct(s.mcpPass, s.mcpRuns)}) | ${s.controlPass}/${s.controlRuns} | ${s.tasksAllK}/${s.tasks} | ${s.tasksAnyK}/${s.tasks} | ${s.mcpNoCall} | ${f1(s.missedCallsMean)} | ${f1(s.searchCallsMean)} | ${f1(s.firstToolsMean)} | ${f0(s.firstToolBytesMean)} | ${f0(s.firstPromptTokensMean)} | ${f0(s.maxPromptTokensMean)} | ${f1(s.modelRequestsMean)} | ${f0(s.completionTokensMean)} | ${f1(s.fullRereadsMean)} | ${f1(s.secondsMedian)} (${f1(s.secondsMean)}) | ${s.timeouts} |`,
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
