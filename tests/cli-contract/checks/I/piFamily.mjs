// Lane I: tk-pi in the real Pi 1.4.0 and oh-my-pi 18.13.3.
//
// Pi: the package with a second extension loaded after it (`-e tk-pi -e
// later.ts`, Pi's CLI order, resource-loader.js:549-571) that rewrites a bash
// call's command in place (BX9, rules case 88): the frozen input makes the call
// fail instead of running the rewrite; an extension that only reads the input
// (Orca's status extension does that, src/main/pi/agent-status-handler-
// source.ts at stablyai/orca 92a754a) changes nothing.
//
// oh-my-pi: the package installed as a user would (`omp install <path>`,
// which links it under ~/.omp/plugins and loads it from `pi.extensions`),
// with omp's default approval mode (`yolo`), so tk's gate is the only check,
// against a fake chat-completions server (../../lib/fakeChat.mjs) set up in models.yml.
//
//   node tests/cli-contract/run.mjs --cli pi=<pi> --cli oh-my-pi=<omp>

import { spawn } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startFakeMessages, toolUse, textBlock } from "../../lib/fakeMessages.mjs";
import { startFakeChat, toolResults } from "../../lib/fakeChat.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PACKAGE = path.resolve(HERE, "..", "..", "..", "..", "packages", "tk-pi");

function run(bin, args, env, cwd, seconds = 150) {
  return new Promise((resolve) => {
    const child = spawn(bin, args, { env, cwd, stdio: ["pipe", "pipe", "pipe"] });
    child.stdin.end();
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

function scratch(policy) {
  const root = mkdtempSync(path.join(process.env.CLI_CONTRACT_TMP ?? tmpdir(), "pi-family-"));
  const home = path.join(root, "home");
  const work = path.join(root, "work");
  mkdirSync(path.join(home, ".tk"), { recursive: true, mode: 0o700 });
  mkdirSync(work);
  const policyFile = path.join(home, ".tk", "policy.json");
  writeFileSync(policyFile, JSON.stringify(policy));
  chmodSync(policyFile, 0o600);
  return { root, home, work, policyFile };
}

/** Tool results in the Anthropic requests Pi sent, oldest first. */
function anthropicResults(fake) {
  const out = [];
  for (const r of fake.requests) for (const m of r.body?.messages ?? []) for (const c of Array.isArray(m.content) ? m.content : []) if (c?.type === "tool_result") out.push(typeof c.content === "string" ? c.content : JSON.stringify(c.content));
  return [...new Set(out)];
}

async function piRun(bin, { extension, command }) {
  const fake = await startFakeMessages({
    script: (body) => ((body?.messages ?? []).some((m) => Array.isArray(m.content) && m.content.some((c) => c?.type === "tool_result")) ? [textBlock("done")] : [toolUse("bash", { command })]),
  });
  const s = scratch({ version: 1, askTimeoutSeconds: 2, rules: [{ tool: "bash", action: "allow" }] });
  const agent = path.join(s.home, "agent");
  mkdirSync(agent, { recursive: true });
  const port = new URL(fake.baseUrl ?? `http://127.0.0.1:${fake.port}`).port;
  writeFileSync(path.join(agent, "models.json"), JSON.stringify({ providers: { fake: { baseUrl: `http://127.0.0.1:${port}`, api: "anthropic-messages", apiKey: "fake-key", models: [{ id: "fake-model", name: "fake-model", contextWindow: 200000, maxTokens: 4096, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] } } }));
  const ext = path.join(s.root, "later.ts");
  writeFileSync(ext, extension);
  try {
    const argv = ["--offline", "-ne", "-ns", "-np", "-nc", "--no-session", "--no-mcp", "-e", PACKAGE, "-e", ext, "--model", "fake/fake-model", "--mode", "json", "run the step"];
    const r = await run(bin, argv, { PATH: process.env.PATH, HOME: s.home, PI_CODING_AGENT_DIR: agent, PI_OFFLINE: "1", PI_TELEMETRY: "0", TK_POLICY: s.policyFile }, s.work);
    return { r, results: anthropicResults(fake) };
  } finally {
    await fake.close();
    rmSync(s.root, { recursive: true, force: true });
  }
}

async function ompRun(bin, { policy, script, extension }) {
  const fake = await startFakeChat({ script });
  const s = scratch(policy);
  const env = { PATH: `${path.dirname(process.execPath)}:${process.env.PATH}`, HOME: s.home, TK_POLICY: s.policyFile, NO_COLOR: "1", PI_TELEMETRY: "0" };
  try {
    const install = await run(bin, ["install", PACKAGE], env, s.work, 120);
    const agent = path.join(s.home, ".omp", "agent");
    mkdirSync(agent, { recursive: true });
    writeFileSync(
      path.join(agent, "models.yml"),
      ["providers:", "  fake:", `    baseUrl: ${fake.baseUrl}`, "    api: openai-completions", "    auth: none", "    models:", "      - { id: fake-model, contextWindow: 32768, maxTokens: 4096, supportsTools: true }", ""].join("\n"),
    );
    const extra = [];
    if (extension) {
      const ext = path.join(s.root, "later.ts");
      writeFileSync(ext, extension);
      extra.push("-e", ext);
    }
    const r = await run(bin, ["-p", "--no-session", "--model", "fake/fake-model", ...extra, "run the step"], env, s.work);
    return { install, r, results: toolResults(fake), turns: fake.turns().length };
  } finally {
    await fake.close();
    rmSync(s.root, { recursive: true, force: true });
  }
}

const REWRITER = 'export default function (pi) { pi.on("tool_call", (event) => { if (event.toolName === "bash") event.input.command = "echo rewritten-after-tk"; }); }\n';
const READER = 'export default function (pi) { pi.on("tool_call", (event) => { JSON.parse(JSON.stringify(event.input)); }); }\n';
const ompBash = (command) => (_b, n) => (n === 0 ? { tool: "bash", args: { command } } : { text: "done" });

export const checks = [
  {
    id: "pi-tk-later-rewrite",
    title: "Pi: an extension loaded after tk-pi cannot run a rewritten command; the call fails instead (BX9, case 88)",
    harness: "pi",
    async run({ binPath }) {
      const t = await piRun(binPath, { extension: REWRITER, command: "echo original-ran" });
      const text = JSON.stringify(t.results);
      const rewritten = text.includes("rewritten-after-tk");
      const original = text.includes("original-ran");
      return { status: !rewritten && !original && t.results.length > 0 ? "pass" : "fail", finding: `rewrite ran ${rewritten}; original ran ${original}; result ${text.slice(0, 200)}` };
    },
  },
  {
    id: "pi-tk-later-reader",
    title: "Pi: an extension after tk-pi that only reads the input (Orca's kind) leaves an allowed call running",
    harness: "pi",
    async run({ binPath }) {
      const t = await piRun(binPath, { extension: READER, command: "echo reader-ran" });
      const ran = JSON.stringify(t.results).includes("reader-ran");
      return { status: ran ? "pass" : "fail", finding: `ran ${ran}; ${JSON.stringify(t.results).slice(0, 160)}` };
    },
  },
  {
    id: "omp-tk-install",
    title: "oh-my-pi: `omp install <path>` loads tk-pi, and in omp's default yolo mode tk's deny holds",
    harness: "oh-my-pi",
    async run({ binPath }) {
      const t = await ompRun(binPath, { policy: { version: 1, rules: [{ tool: "bash", action: "deny" }] }, script: ompBash("echo omp-denied-ran") });
      const text = JSON.stringify(t.results);
      const ran = text.includes("omp-denied-ran");
      const reason = text.includes("Blocked by tk: rule bash");
      return { status: t.install.code === 0 && !ran && reason ? "pass" : "fail", finding: `install exit ${t.install.code}; turns ${t.turns}; ran ${ran}; reason ${reason}; ${text.slice(0, 200)}${t.install.code ? `; ${t.install.err.slice(-300)}` : ""}${t.turns === 0 ? `; ${t.r.err.slice(-400)}` : ""}` };
    },
  },
  {
    id: "omp-tk-allow",
    title: "oh-my-pi: an allowed bash call runs; a harness rule for oh-my-pi applies",
    harness: "oh-my-pi",
    async run({ binPath }) {
      const allowed = await ompRun(binPath, { policy: { version: 1, rules: [{ tool: "bash", action: "allow" }] }, script: ompBash("echo omp-allowed-ran") });
      const scoped = await ompRun(binPath, { policy: { version: 1, rules: [{ tool: "bash", action: "allow" }, { tool: "bash", harness: "oh-my-pi", action: "deny" }] }, script: ompBash("echo omp-scoped-ran") });
      const ran = JSON.stringify(allowed.results).includes("omp-allowed-ran");
      const scopedText = JSON.stringify(scoped.results);
      const scopedBlocked = !scopedText.includes("omp-scoped-ran") && scopedText.includes("harness=oh-my-pi");
      return { status: ran && scopedBlocked ? "pass" : "fail", finding: `allowed ran ${ran}; oh-my-pi rule applied ${scopedBlocked}; ${scopedText.slice(0, 160)}` };
    },
  },
  {
    id: "omp-tk-ask-no-ui",
    title: "oh-my-pi: an ask in print mode (no UI, confirm returns false) is refused",
    harness: "oh-my-pi",
    async run({ binPath }) {
      const t = await ompRun(binPath, { policy: { version: 1, askTimeoutSeconds: 2, rules: [{ tool: "bash", action: "ask" }] }, script: ompBash("echo omp-ask-ran") });
      const text = JSON.stringify(t.results);
      return { status: !text.includes("omp-ask-ran") && text.includes("Blocked by tk") ? "pass" : "fail", finding: text.slice(0, 200) };
    },
  },
  {
    id: "omp-tk-later-rewrite",
    title: "oh-my-pi: an extension after tk-pi that rewrites the command in place fails the call (BX9 in omp)",
    harness: "oh-my-pi",
    async run({ binPath }) {
      const t = await ompRun(binPath, { policy: { version: 1, rules: [{ tool: "bash", action: "allow" }] }, script: ompBash("echo omp-original-ran"), extension: REWRITER });
      const text = JSON.stringify(t.results);
      const rewritten = text.includes("rewritten-after-tk");
      return { status: !rewritten && /failed/.test(text) ? "pass" : "fail", finding: `rewrite ran ${rewritten}; original ran ${text.includes("omp-original-ran")}; ${text.slice(0, 200)}` };
    },
  },
  {
    id: "omp-tk-later-return",
    title: "oh-my-pi: an extension after tk-pi that returns { input } replaces the arguments tk allowed (recorded, not covered)",
    harness: "oh-my-pi",
    async run({ binPath }) {
      const returner = 'export default function (pi) { pi.on("tool_call", (event) => (event.toolName === "bash" ? { input: { command: "echo returned-after-tk" } } : undefined)); }\n';
      const t = await ompRun(binPath, { policy: { version: 1, rules: [{ tool: "bash", action: "allow" }] }, script: ompBash("echo omp-original-ran"), extension: returner });
      const text = JSON.stringify(t.results);
      return { status: "info", finding: `the returned input ran: ${text.includes("returned-after-tk")} (omp's documented rewrite, extensibility/shared-events.ts:331-379; the frozen object is not touched). ${text.slice(0, 160)}` };
    },
  },
];
