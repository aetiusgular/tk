// Lane I: contract checks for the tk OpenClaw plugin (T2.4) against the real
// OpenClaw CLI, `openclaw agent exec` (no gateway), with a fake
// chat-completions server as the model (docs/gateway/local-models.md:141-187)
// and the plugin loaded from plugins.load.paths with plugins.entries.tk.enabled
// (docs/plugins/hooks/tool-policy.md:184-213). OpenClaw 2026.10.8 needs Node
// 24.22.1 or later (package.json engines): pass its folder as OPENCLAW_NODE_BIN,
// or have it first on PATH.
//
//   OPENCLAW_NODE_BIN=/path/to/node24/bin node tests/cli-contract/run.mjs --cli openclaw=<openclaw>

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startFakeChat, toolResults } from "./fakeChat.js";
import { startFakeDaemon } from "../../../../packages/tk-claude-plugin/src/testing/fakeDaemon.ts";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN = path.resolve(HERE, "..", "..", "..", "..", "packages", "tk-openclaw-plugin");

function run(bin, args, env, cwd, seconds = 180) {
  return new Promise((resolve) => {
    const child = spawn(bin, args, { env, cwd, stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    const timer = setTimeout(() => child.kill("SIGKILL"), seconds * 1000);
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ code, out, err });
    });
  });
}

/** One `openclaw agent exec` turn; `extraPlugins` are written as plugin folders and loaded beside tk. */
async function ocTurn(bin, { policy, script, approve = "none", extraPlugins = {} }) {
  const fake = await startFakeChat({ script });
  const daemon = await startFakeDaemon({ approve, root: "/tmp" });
  const root = mkdtempSync(path.join(process.env.CLI_CONTRACT_TMP ?? tmpdir(), "openclaw-tk-"));
  const home = path.join(root, "home");
  const work = path.join(root, "work");
  mkdirSync(path.join(home, ".tk"), { recursive: true, mode: 0o700 });
  mkdirSync(path.join(home, ".openclaw"), { recursive: true });
  mkdirSync(work);
  const policyFile = path.join(home, ".tk", "policy.json");
  writeFileSync(policyFile, JSON.stringify(policy), { mode: 0o600 });
  const paths = [PLUGIN];
  const entries = { tk: { enabled: true } };
  for (const [id, source] of Object.entries(extraPlugins)) {
    const dir = path.join(root, "plugins", id);
    mkdirSync(dir, { recursive: true });
    writeFileSync(path.join(dir, "package.json"), JSON.stringify({ name: id, version: "0.0.0", type: "module", openclaw: { extensions: ["./index.ts"] } }));
    writeFileSync(path.join(dir, "openclaw.plugin.json"), JSON.stringify({ id, name: id, activation: { onStartup: true }, configSchema: { type: "object", additionalProperties: false } }));
    writeFileSync(path.join(dir, "index.ts"), source);
    paths.push(dir);
    entries[id] = { enabled: true };
  }
  const config = {
    models: { providers: { local: { baseUrl: fake.baseUrl, apiKey: "sk-local", api: "openai-completions", models: [{ id: "fake-model", name: "fake", contextWindow: 32768, maxTokens: 4096 }] } } },
    agents: { defaults: { model: { primary: "local/fake-model" } } },
    plugins: { load: { paths }, entries },
  };
  const cfg = path.join(home, ".openclaw", "openclaw.json");
  writeFileSync(cfg, JSON.stringify(config, null, 1));
  const nodeBin = process.env.OPENCLAW_NODE_BIN;
  const env = { PATH: nodeBin ? `${nodeBin}:${process.env.PATH}` : process.env.PATH, HOME: home, OPENCLAW_CONFIG_PATH: cfg, TK_POLICY: policyFile, NO_COLOR: "1", ...daemon.env() };
  try {
    const r = await run(bin, ["agent", "exec", "run the step", "--model", "local/fake-model", "--cwd", work, "--json", "--timeout", "120"], env, work);
    return { r, results: toolResults(fake), turns: fake.turns().length, daemon: { events: daemon.events(), approvals: daemon.approvals() } };
  } finally {
    await fake.close();
    await daemon.close();
    rmSync(root, { recursive: true, force: true });
  }
}

const execCall = (command) => (_b, n) => (n === 0 ? { tool: "exec", args: { command } } : { text: "done" });
const ALLOW = { version: 1, askTimeoutSeconds: 5, rules: [{ tool: "bash", action: "allow" }] };

export const checks = [
  {
    id: "openclaw-tk-allow",
    title: "An allowed exec runs; the daemon gets the decision and the result record",
    harness: "openclaw",
    async run({ binPath }) {
      const t = await ocTurn(binPath, { policy: ALLOW, script: execCall("echo oc-allowed-ran") });
      const ran = t.results.some((x) => x.includes("oc-allowed-ran"));
      const kinds = t.daemon.events.map((e) => `${e.kind}:${e.harness}`);
      const ok = ran && kinds.includes("approval:openclaw") && kinds.includes("tool.call:openclaw");
      return { status: ok ? "pass" : "fail", finding: `exit ${t.r.code}; ran ${ran}; records ${JSON.stringify(kinds)}${ok ? "" : `; ${t.r.err.slice(-300)}`}` };
    },
  },
  {
    id: "openclaw-tk-deny",
    title: "A denied exec does not run; the model gets tk's reason",
    harness: "openclaw",
    async run({ binPath }) {
      const t = await ocTurn(binPath, { policy: { version: 1, rules: [{ tool: "bash", action: "deny" }] }, script: execCall("echo oc-denied-ran") });
      const ran = t.results.some((x) => x.includes("oc-denied-ran"));
      const reason = t.results.some((x) => x.includes("tk: rule bash"));
      return { status: !ran && reason ? "pass" : "fail", finding: `ran ${ran}; reason ${reason}; ${JSON.stringify(t.results).slice(0, 200)}` };
    },
  },
  {
    id: "openclaw-tk-shell-check",
    title: "OpenClaw's exec gets the shell-text check (case 82): a glob under bash: allow is asked, and with no approver in exec mode it does not run",
    harness: "openclaw",
    async run({ binPath }) {
      const t = await ocTurn(binPath, { policy: ALLOW, script: execCall("ls *.md; echo oc-glob-ran") });
      const ran = t.results.some((x) => x.includes("oc-glob-ran"));
      return { status: ran ? "fail" : "pass", finding: `ran ${ran}; ${JSON.stringify(t.results).slice(0, 240)}` };
    },
  },
  {
    id: "openclaw-tk-ask-approver",
    title: "An ask goes to the tk approver; a yes runs the call",
    harness: "openclaw",
    async run({ binPath }) {
      const t = await ocTurn(binPath, { approve: "yes", policy: { version: 1, askTimeoutSeconds: 5, rules: [{ tool: "bash", action: "ask" }] }, script: execCall("echo oc-approved-ran") });
      const ran = t.results.some((x) => x.includes("oc-approved-ran"));
      const asked = t.daemon.approvals.filter((a) => a.harness === "openclaw").length;
      return { status: ran && asked === 1 ? "pass" : "fail", finding: `approvals ${asked}; ran ${ran}` };
    },
  },
  {
    id: "openclaw-tk-runs-last",
    title: "A higher-priority plugin's params rewrite is replaced by the params tk checked (case 88)",
    harness: "openclaw",
    async run({ binPath }) {
      const rewriter = 'export default { id: "rewriter", name: "rewriter", description: "test", register(api) { api.on("before_tool_call", (event) => (event.toolName === "exec" ? { params: { ...event.params, command: "echo rewritten-before-tk" } } : undefined), { priority: 100 }); } };\n';
      const t = await ocTurn(binPath, { policy: ALLOW, script: execCall("echo oc-original-ran"), extraPlugins: { rewriter } });
      const text = JSON.stringify(t.results);
      const original = text.includes("oc-original-ran");
      const rewritten = text.includes("rewritten-before-tk");
      return { status: original && !rewritten ? "pass" : "fail", finding: `original ran ${original}; rewrite ran ${rewritten}; ${text.slice(0, 200)}${t.turns ? "" : `; ${t.r.err.slice(-300)}`}` };
    },
  },
  {
    id: "openclaw-tk-beside-observer",
    title: "Beside another before_tool_call plugin that answers nothing, a deny still wins and tk's gate runs once (case 75)",
    harness: "openclaw",
    async run({ binPath }) {
      const observer = 'export default { id: "observer", name: "observer", description: "test", register(api) { api.on("before_tool_call", () => undefined); } };\n';
      const t = await ocTurn(binPath, { policy: { version: 1, rules: [{ tool: "bash", action: "deny" }] }, script: execCall("echo oc-observer-ran"), extraPlugins: { observer } });
      const ran = t.results.some((x) => x.includes("oc-observer-ran"));
      const decisions = t.daemon.events.filter((e) => e.kind === "approval").length;
      return { status: !ran && decisions === 1 ? "pass" : "fail", finding: `ran ${ran}; tk decisions ${decisions}` };
    },
  },
];
