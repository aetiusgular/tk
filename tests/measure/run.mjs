#!/usr/bin/env node
// T3.5 runner. Runs every task in tasks.json under each condition, k times,
// one run at a time, and appends one scored line per run to
// <out>/results.jsonl. Raw harness output goes to <out>/raw/<run>.jsonl.
//
//   node run.mjs --out <dir> --conditions pi-deferred,pi-direct,cc-unset,cc-auto,cc-false \
//     --k 3 [--tasks t01,t02] [--timeout 480] [--proxy http://127.0.0.1:8091]
//     [--state <dir shared with the proxy>] [--tag <run id prefix>]
//
// Expects the proxy (proxy.mjs) to be running with --state <out>/state.
// Paths to Pi, Claude Code and the tk packages come from the environment
// (TKM_PI, TKM_CLAUDE, TKM_TK); see README.md.
import { spawn } from "node:child_process";
import { appendFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const arg = (k, d) => {
  const i = process.argv.indexOf(`--${k}`);
  return i > 0 ? process.argv[i + 1] : d;
};
const out = resolve(arg("out", join(here, "results")));
const conditions = arg("conditions", "pi-deferred,pi-direct").split(",");
const K = Number(arg("k", "3"));
const kFrom = Number(arg("k-from", "1"));
const only = arg("tasks") ? arg("tasks").split(",") : null;
const timeoutMs = Number(arg("timeout", "480")) * 1000;
const proxy = arg("proxy", "http://127.0.0.1:8091");
const model = arg("model", "qwen27b-q4");
const PI = process.env.TKM_PI || "pi";
const CLAUDE = process.env.TKM_CLAUDE || "claude";
const TK = process.env.TKM_TK; // a tk checkout
if (!TK) throw new Error("set TKM_TK to a tk checkout");

const spec = JSON.parse(readFileSync(join(here, "tasks.json"), "utf8"));
const tasks = spec.tasks.filter((t) => !only || only.includes(t.id));
const state = resolve(arg("state", join(out, "state")));
const tag = arg("tag", "");
for (const d of [out, state, join(out, "raw"), join(out, "work")]) mkdirSync(d, { recursive: true });

const SERVERS = { context7: "context7.json", exa: "exa.json", hf: "hugging-face.json", shopify: "shopify-dev.json" };
const stubEntry = (name) => ({
  command: process.execPath,
  args: [join(here, "stub-server.mjs"), "--server", name, "--tools", join(here, "tools", SERVERS[name]), "--tasks", join(here, "tasks.json"), "--state", state],
});

// Pi: one agent folder per condition.
function piAgentDir(exposure) {
  const dir = join(out, `pi-agent-${exposure}`);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "settings.json"), JSON.stringify({ enableInstallTelemetry: false, packages: [join(TK, "packages/tk-pi")] }, null, 2));
  writeFileSync(
    join(dir, "models.json"),
    JSON.stringify({ providers: { tkm: { baseUrl: `${proxy}/v1`, api: "openai-completions", apiKey: "none", models: [{ id: model, name: `${model} via tk-measure proxy`, contextWindow: 131072, maxTokens: 16384, reasoning: true }] } } }, null, 2),
  );
  const mcpServers = {};
  for (const s of Object.keys(SERVERS)) mcpServers[s] = { ...stubEntry(s), exposure };
  writeFileSync(join(dir, "mcp.json"), JSON.stringify({ mcpServers }, null, 2));
  return dir;
}

// Claude Code: one config folder for all three settings; MCP by --mcp-config.
function ccSetup() {
  const dir = join(out, "cc-config");
  mkdirSync(dir, { recursive: true });
  const mcpServers = {};
  for (const s of Object.keys(SERVERS)) mcpServers[s] = { type: "stdio", ...stubEntry(s) };
  const mcp = join(out, "cc-mcp.json");
  writeFileSync(mcp, JSON.stringify({ mcpServers }, null, 2));
  return { dir, mcp };
}

const policy = join(out, "policy.json");
writeFileSync(policy, readFileSync(join(here, "policy.json")));
const piDirs = { "pi-deferred": piAgentDir("deferred"), "pi-direct": piAgentDir("direct") };
const cc = ccSetup();

function command(cond, prompt) {
  const base = { ...process.env, TK_POLICY: policy, PI_TELEMETRY: "0", TKM_STATE: state };
  delete base.TK_CHAT_TOKEN;
  delete base.TK_CHAT_TOKEN_FILE;
  if (cond.startsWith("pi-")) {
    return { cmd: PI, argv: ["--mode", "json", "--no-session", "--model", `tkm/${model}`, prompt], env: { ...base, PI_CODING_AGENT_DIR: piDirs[cond] } };
  }
  const env = {
    ...base,
    CLAUDE_CONFIG_DIR: cc.dir,
    ANTHROPIC_BASE_URL: proxy,
    ANTHROPIC_AUTH_TOKEN: "tk-measure-local",
    ANTHROPIC_MODEL: model,
    ANTHROPIC_DEFAULT_HAIKU_MODEL: model,
    ANTHROPIC_DEFAULT_SONNET_MODEL: model,
    ANTHROPIC_DEFAULT_OPUS_MODEL: model,
    CLAUDE_CODE_SUBAGENT_MODEL: model,
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
    DISABLE_AUTOUPDATER: "1",
  };
  delete env.ANTHROPIC_API_KEY;
  delete env.ENABLE_TOOL_SEARCH;
  if (cond === "cc-auto") env.ENABLE_TOOL_SEARCH = "auto";
  if (cond === "cc-false") env.ENABLE_TOOL_SEARCH = "false";
  if (cond === "cc-true") env.ENABLE_TOOL_SEARCH = "true";
  const argv = [
    "-p", prompt,
    "--output-format", "stream-json", "--verbose",
    "--mcp-config", cc.mcp, "--strict-mcp-config",
    "--plugin-dir", join(TK, "packages/tk-claude-plugin"),
    "--disallowedTools", "WebFetch,WebSearch",
  ];
  return { cmd: CLAUDE, argv, env };
}

function runOnce(cmd, argv, env, cwd) {
  return new Promise((res) => {
    const started = Date.now();
    const child = spawn(cmd, argv, { cwd, env, stdio: ["ignore", "pipe", "pipe"], detached: true });
    let stdout = "",
      stderr = "",
      timedOut = false;
    child.stdout.on("data", (d) => (stdout += d));
    child.stderr.on("data", (d) => (stderr += d));
    const t = setTimeout(() => {
      timedOut = true;
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {}
      setTimeout(() => {
        try {
          process.kill(-child.pid, "SIGKILL");
        } catch {}
      }, 5000);
    }, timeoutMs);
    child.on("close", (code, signal) => {
      clearTimeout(t);
      res({ code, signal, timedOut, ms: Date.now() - started, stdout, stderr });
    });
  });
}

function parseEvents(stdout) {
  const ev = [];
  for (const l of stdout.split("\n")) {
    if (!l.trim()) continue;
    try {
      ev.push(JSON.parse(l));
    } catch {}
  }
  return ev;
}

function harnessView(cond, ev) {
  const toolsUsed = [];
  let finalText = "";
  let turns = 0;
  let errors = [];
  if (cond.startsWith("pi-")) {
    for (const e of ev) {
      if (e.type === "tool_execution_start") toolsUsed.push(e.toolName);
      if (e.type === "message_end" && e.message?.role === "assistant") {
        turns++;
        const text = (e.message.content || []).filter((c) => c.type === "text").map((c) => c.text).join("");
        if (text.trim()) finalText = text;
        if (e.message.errorMessage) errors.push(e.message.errorMessage);
        if (e.message.stopReason === "error") errors.push(e.message.errorMessage || "stopReason error");
      }
    }
  } else {
    let init = null;
    for (const e of ev) {
      if (e.type === "system" && e.subtype === "init") init = e;
      if (e.type === "assistant") {
        turns++;
        for (const c of e.message?.content || []) if (c.type === "tool_use") toolsUsed.push(c.name);
      }
      if (e.type === "result") {
        finalText = e.result || finalText;
        if (e.is_error) errors.push(e.subtype || "result error");
      }
    }
    return { toolsUsed, finalText, turns, errors, initTools: init?.tools?.length ?? null, initMcp: init?.mcp_servers ?? null };
  }
  return { toolsUsed, finalText, turns, errors };
}

function readLines(file, runId) {
  if (!existsSync(file)) return [];
  return readFileSync(file, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((l) => {
      try {
        return JSON.parse(l);
      } catch {
        return null;
      }
    })
    .filter((x) => x && x.run === runId);
}

const results = join(out, "results.jsonl");
let seq = existsSync(results) ? readFileSync(results, "utf8").split("\n").filter(Boolean).length : 0;
for (let k = kFrom; k <= K; k++) {
  for (const task of tasks) {
    for (const cond of conditions) {
      const runId = `${tag}r${String(++seq).padStart(4, "0")}-${cond}-${task.id}-k${k}`;
      const work = join(out, "work", runId);
      rmSync(work, { recursive: true, force: true });
      mkdirSync(work, { recursive: true });
      for (const [f, body] of Object.entries(task.files || {})) {
        mkdirSync(dirname(join(work, f)), { recursive: true });
        writeFileSync(join(work, f), body);
      }
      writeFileSync(join(state, "current"), `${task.id} ${runId}\n`);
      const prompt = `${task.prompt}\n\n${spec.suffix}`;
      const { cmd, argv, env } = command(cond, prompt);
      const r = await runOnce(cmd, argv, env, work);
      writeFileSync(join(state, "current"), `idle idle\n`);
      writeFileSync(join(out, "raw", `${runId}.jsonl`), r.stdout);
      if (r.stderr) writeFileSync(join(out, "raw", `${runId}.stderr.txt`), r.stderr.slice(-20000));
      const ev = parseEvents(r.stdout);
      const hv = harnessView(cond, ev);
      const calls = readLines(join(state, "calls.jsonl"), runId);
      const reqs = readLines(join(state, "requests.jsonl"), runId).filter((q) => q.method === "POST");
      const haystack = task.checkFile ? (existsSync(join(work, task.checkFile)) ? readFileSync(join(work, task.checkFile), "utf8") : "") : hv.finalText;
      const found = task.tokens.filter((tok) => haystack.toLowerCase().includes(tok.toLowerCase()));
      const expectedTools = Object.keys(task.responses || {});
      const first = reqs[0] || {};
      const sum = (f) => reqs.reduce((a, q) => a + (Number(f(q)) || 0), 0);
      const line = {
        run: runId, cond, task: task.id, kind: task.kind, k,
        pass: found.length === task.tokens.length,
        tokensFound: found.length, tokensWanted: task.tokens.length,
        reachedTool: calls.some((c) => c.hit),
        mcpCalls: calls.length,
        mcpCallsMissed: calls.filter((c) => !c.hit).length,
        mcpToolsCalled: calls.map((c) => `${c.server}.${c.tool}`),
        expectedTools,
        harnessTools: hv.toolsUsed,
        searchCalls: hv.toolsUsed.filter((n) => /^(tool_search|ToolSearch)$/.test(n)).length,
        turns: hv.turns,
        modelRequests: reqs.length,
        modelRequestErrors: reqs.filter((q) => q.status >= 400).map((q) => ({ status: q.status, body: q.errorBody })),
        firstRequest: { tools: first.tools, toolBytes: first.toolBytes, deferred: first.deferred, systemBytes: first.systemBytes, promptTokens: first.usage?.prompt_tokens ?? first.usage?.input_tokens ?? null, promptProcessed: first.usage?.timings?.prompt_n ?? null, beta: first.beta },
        maxToolsDeclared: Math.max(0, ...reqs.map((q) => q.tools || 0)),
        maxLoadedTools: Math.max(0, ...reqs.map((q) => (q.loadedNames || []).length)),
        promptTokensSum: sum((q) => q.usage?.prompt_tokens ?? (q.usage?.input_tokens != null ? q.usage.input_tokens + (q.usage.cache_read_input_tokens || 0) + (q.usage.cache_creation_input_tokens || 0) : 0)),
        promptProcessedSum: sum((q) => q.usage?.timings?.prompt_n),
        completionTokensSum: sum((q) => q.usage?.completion_tokens ?? q.usage?.output_tokens),
        maxPromptTokens: Math.max(0, ...reqs.map((q) => q.usage?.prompt_tokens ?? q.usage?.input_tokens ?? 0)),
        ms: r.ms, exit: r.code, signal: r.signal, timedOut: r.timedOut,
        errors: hv.errors.slice(0, 5),
        initTools: hv.initTools ?? null,
        finalText: hv.finalText.slice(0, 1500),
      };
      appendFileSync(results, JSON.stringify(line) + "\n");
      console.log(`${runId} pass=${line.pass} calls=${line.mcpCalls} search=${line.searchCalls} reqs=${line.modelRequests} firstTools=${first.tools} ${Math.round(r.ms / 1000)}s${r.timedOut ? " TIMEOUT" : ""}`);
    }
  }
}
console.log("done");
