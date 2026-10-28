// Lane I: contract checks for the tk Hermes plugin (T2.4) against a real
// Hermes Agent. NOT RUN by lane I: installing Hermes (Python 3.14 and its
// dependencies, from source) was refused in the lane's workspace, so these
// are written from Hermes v0.22.5's source and docs (bf0203a7) and first run
// on the box. Pass `--cli hermes=<path to the hermes-agent entry point>`
// (pyproject.toml:567; the in-process runner, no gateway; it loads Python
// plugins through model_tools' discovery, model_tools.py:155-165).
//
// Each run: a scratch HERMES_HOME with the plugin copied into plugins/tk and
// enabled in config.yaml (plugins.enabled is opt-in,
// hermes_cli/plugins_discovery.py:112-131), the fake chat-completions server
// as the model (agent/legacy_cli.py:36-55: --base-url, --api-key, --model),
// one `terminal` call.

import { spawn } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startFakeChat, toolResults } from "./fakeChat.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN = path.resolve(HERE, "..", "..", "..", "..", "packages", "tk-hermes-plugin");

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

async function hermesTurn(bin, { policy, command }) {
  const fake = await startFakeChat({ script: (_b, n) => (n === 0 ? { tool: "terminal", args: { command } } : { text: "done" }) });
  const root = mkdtempSync(path.join(process.env.CLI_CONTRACT_TMP ?? tmpdir(), "hermes-tk-"));
  const home = path.join(root, "hermes");
  const work = path.join(root, "work");
  mkdirSync(path.join(home, "plugins"), { recursive: true });
  mkdirSync(path.join(root, ".tk"), { recursive: true, mode: 0o700 });
  mkdirSync(work);
  cpSync(PLUGIN, path.join(home, "plugins", "tk"), { recursive: true, filter: (p) => !p.includes("node_modules") && !p.endsWith(".test.ts") && !p.includes(`${path.sep}tests`) });
  writeFileSync(path.join(home, "config.yaml"), "plugins:\n  enabled:\n    - tk\n");
  const policyFile = path.join(root, ".tk", "policy.json");
  writeFileSync(policyFile, JSON.stringify(policy), { mode: 0o600 });
  const env = { PATH: process.env.PATH, HOME: root, HERMES_HOME: home, TK_POLICY: policyFile, NO_COLOR: "1" };
  try {
    const r = await run(bin, ["--base-url", fake.baseUrl, "--api-key", "dummy", "--model", "fake-model", "--enabled-toolsets", "terminal", "--max-turns", "3", "--query", "run the step"], env, work);
    return { r, results: toolResults(fake), turns: fake.turns().length };
  } finally {
    await fake.close();
    rmSync(root, { recursive: true, force: true });
  }
}

export const checks = [
  {
    id: "hermes-tk-deny",
    title: "Hermes: a denied terminal call does not run; the model gets tk's block message",
    harness: "hermes",
    async run({ binPath }) {
      const t = await hermesTurn(binPath, { policy: { version: 1, rules: [{ tool: "terminal", action: "deny" }] }, command: "echo hermes-denied-ran" });
      const text = JSON.stringify(t.results);
      const ok = !text.includes("hermes-denied-ran\\n") && text.includes("tk: rule terminal");
      return { status: ok ? "pass" : "fail", finding: `exit ${t.r.code}; turns ${t.turns}; ${text.slice(0, 200)}${ok ? "" : `; ${t.r.err.slice(-300)}`}` };
    },
  },
  {
    id: "hermes-tk-allow",
    title: "Hermes: an allowed terminal call runs",
    harness: "hermes",
    async run({ binPath }) {
      const t = await hermesTurn(binPath, { policy: { version: 1, rules: [{ tool: "terminal", action: "allow" }] }, command: "echo hermes-allowed-ran" });
      const ran = JSON.stringify(t.results).includes("hermes-allowed-ran");
      return { status: ran ? "pass" : "fail", finding: `exit ${t.r.code}; turns ${t.turns}; ran ${ran}${ran ? "" : `; ${t.r.err.slice(-300)}`}` };
    },
  },
  {
    id: "hermes-tk-ask-unattended",
    title: "Hermes: an ask with no tk approver in the in-process runner (no human) is refused",
    harness: "hermes",
    async run({ binPath }) {
      const t = await hermesTurn(binPath, { policy: { version: 1, askTimeoutSeconds: 2, rules: [{ tool: "terminal", action: "ask" }] }, command: "echo hermes-ask-ran" });
      const ran = JSON.stringify(t.results).includes("hermes-ask-ran\\n");
      return { status: ran ? "fail" : "pass", finding: `ran ${ran}; ${JSON.stringify(t.results).slice(0, 200)}` };
    },
  },
];
