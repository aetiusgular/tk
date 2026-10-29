// Lane I: contract checks for the tk Kilo plugin (T2.4) against the real
// Kilo CLI, driven by `kilo run --format json` against a fake OpenAI
// chat-completions server (../../lib/fakeChat.mjs) with a fake tk daemon. Kilo's own
// permission for bash is `allow` in every run, so tk's gate is the only check
// (Orca has no full-access entry for Kilo, note AQ section 8; this is the
// closest).
//
//   node tests/cli-contract/run.mjs --cli kilo=/path/to/kilo --only kilo-tk-deny,...

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { startFakeChat, toolResults } from "../../lib/fakeChat.mjs";
import { startFakeDaemon } from "../../../../packages/tk-claude-plugin/src/testing/fakeDaemon.ts";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN = path.resolve(HERE, "..", "..", "..", "..", "packages", "tk-kilo-plugin");

function run(bin, args, env, cwd, seconds = 150) {
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

/**
 * One `kilo run`. `install` is "config" (a file: URL in the config's plugin
 * array, the README's route) or "dir" (a one-line file in
 * ~/.config/kilo/plugin/, which Kilo loads at start). `extraPlugins` are
 * written into the global plugin folder beside it.
 */
async function kiloTurn(bin, { policy, script, approve = "none", install = "config", extraPlugins = {}, daemon: withDaemon = true }) {
  const fake = await startFakeChat({ script });
  const daemon = await startFakeDaemon({ approve, root: "/tmp" });
  const root = mkdtempSync(path.join(process.env.CLI_CONTRACT_TMP ?? tmpdir(), "kilo-tk-"));
  const home = path.join(root, "home");
  const work = path.join(root, "work");
  const pluginDir = path.join(home, ".config", "kilo", "plugin");
  mkdirSync(path.join(home, ".tk"), { recursive: true, mode: 0o700 });
  mkdirSync(pluginDir, { recursive: true });
  mkdirSync(work);
  const policyFile = path.join(home, ".tk", "policy.json");
  writeFileSync(policyFile, JSON.stringify(policy), { mode: 0o600 });
  if (install === "dir") writeFileSync(path.join(pluginDir, "tk.ts"), `export { default } from ${JSON.stringify(pathToFileURL(path.join(PLUGIN, "src", "plugin.ts")).href)};\n`);
  for (const [name, text] of Object.entries(extraPlugins)) writeFileSync(path.join(pluginDir, name), text);
  const config = {
    provider: { fake: { npm: "@ai-sdk/openai-compatible", name: "Fake", options: { baseURL: fake.baseUrl, apiKey: "x" }, models: { "fake-model": { name: "fake", tool_call: true } } } },
    model: "fake/fake-model",
    permission: { bash: "allow", edit: "allow" },
    ...(install === "config" ? { plugin: [pathToFileURL(PLUGIN).href] } : {}),
  };
  const env = {
    PATH: process.env.PATH,
    HOME: home,
    XDG_CONFIG_HOME: path.join(home, ".config"),
    XDG_CACHE_HOME: path.join(home, ".cache"),
    XDG_DATA_HOME: path.join(home, ".local", "share"),
    KILO_CONFIG_CONTENT: JSON.stringify(config),
    NO_COLOR: "1",
    TK_POLICY: policyFile,
    ...(withDaemon ? daemon.env() : { TK_DAEMON_SOCKET: path.join(root, "absent.sock") }),
  };
  try {
    const r = await run(bin, ["run", "--format", "json", "run the step"], env, work);
    return { r, results: toolResults(fake), turns: fake.turns().length, daemon: { events: daemon.events(), approvals: daemon.approvals() }, work };
  } finally {
    await fake.close();
    await daemon.close();
    rmSync(root, { recursive: true, force: true });
  }
}

const bash = (command) => (_b, n) => (n === 0 ? { tool: "bash", args: { command, description: "step" } } : { text: "done" });
const ALLOW = { version: 1, askTimeoutSeconds: 5, rules: [{ tool: "bash", action: "allow" }] };

export const checks = [
  {
    id: "kilo-tk-allow",
    title: "Loaded from the config's plugin array, an allowed bash call runs and the daemon gets both records",
    harness: "kilo",
    async run({ binPath }) {
      const t = await kiloTurn(binPath, { policy: ALLOW, script: bash("echo kilo-allowed-ran") });
      const ran = t.results.some((x) => x.includes("kilo-allowed-ran"));
      const kinds = t.daemon.events.map((e) => `${e.kind}:${e.harness}`);
      const ok = t.r.code === 0 && ran && kinds.includes("approval:kilo") && kinds.includes("tool.call:kilo");
      return { status: ok ? "pass" : "fail", finding: `exit ${t.r.code}; ran ${ran}; records ${JSON.stringify(kinds)}${ok ? "" : `; ${t.r.err.slice(-300)}`}` };
    },
  },
  {
    id: "kilo-tk-deny",
    title: "Loaded from ~/.config/kilo/plugin/, a denied bash call does not run and the model gets tk's reason",
    harness: "kilo",
    async run({ binPath }) {
      const t = await kiloTurn(binPath, { install: "dir", policy: { version: 1, rules: [{ tool: "bash", action: "deny" }] }, script: bash("echo kilo-denied-ran") });
      const ran = t.results.some((x) => x.includes("kilo-denied-ran"));
      const reason = t.results.some((x) => x.includes("tk: rule bash"));
      return { status: !ran && reason ? "pass" : "fail", finding: `ran ${ran}; reason ${reason}; ${JSON.stringify(t.results).slice(0, 200)}` };
    },
  },
  {
    id: "kilo-tk-reject",
    title: "The reject list blocks a write of an endpoint key under an allow rule",
    harness: "kilo",
    async run({ binPath }) {
      const t = await kiloTurn(binPath, { policy: ALLOW, script: bash("echo export ANTHROPIC_BASE_URL=http://x >> .envrc") });
      const ok = t.results.some((x) => x.includes("reject list: anthropic_base_url"));
      return { status: ok ? "pass" : "fail", finding: JSON.stringify(t.results).slice(0, 240) };
    },
  },
  {
    id: "kilo-tk-ask",
    title: "An ask is the tk approver's yes; with no approver it is a deny (Kilo never fires permission.ask)",
    harness: "kilo",
    async run({ binPath }) {
      const policy = { version: 1, askTimeoutSeconds: 5, rules: [{ tool: "bash", action: "ask" }] };
      const yes = await kiloTurn(binPath, { policy, approve: "yes", script: bash("echo kilo-approved-ran") });
      const none = await kiloTurn(binPath, { policy, approve: "none", script: bash("echo kilo-unapproved-ran") });
      const ranYes = yes.results.some((x) => x.includes("kilo-approved-ran"));
      const ranNone = none.results.some((x) => x.includes("kilo-unapproved-ran"));
      const asked = yes.daemon.approvals.filter((a) => a.harness === "kilo").length;
      return { status: ranYes && !ranNone && asked === 1 ? "pass" : "fail", finding: `yes: ran ${ranYes}, approvals ${asked}; none: ran ${ranNone}, ${JSON.stringify(none.results).slice(0, 160)}` };
    },
  },
  {
    id: "kilo-tk-control",
    title: "An allowed command that drops a file into Kilo's plugin folder asks, and is denied with no approver",
    harness: "kilo",
    async run({ binPath }) {
      const t = await kiloTurn(binPath, { policy: ALLOW, script: bash("mkdir -p .kilo/plugin && echo x > .kilo/plugin/evil.ts") });
      const blocked = t.results.some((x) => x.includes("control file (.kilo/)"));
      return { status: blocked ? "pass" : "fail", finding: JSON.stringify(t.results).slice(0, 240) };
    },
  },
  {
    id: "kilo-tk-beside-plugin",
    title: "Beside another plugin's tool.execute.before (an observer), a deny still wins and tk's gate runs once (case 75)",
    harness: "kilo",
    async run({ binPath }) {
      const observer = 'export default { id: "observer", server: async () => ({ "tool.execute.before": async () => {} }) };\n';
      const t = await kiloTurn(binPath, { install: "dir", extraPlugins: { "observer.ts": observer }, policy: { version: 1, rules: [{ tool: "bash", action: "deny" }] }, script: bash("echo kilo-observer-ran") });
      const ran = t.results.some((x) => x.includes("kilo-observer-ran"));
      const decisions = t.daemon.events.filter((e) => e.kind === "approval").length;
      return { status: !ran && decisions === 1 ? "pass" : "fail", finding: `ran ${ran}; tk decisions ${decisions}` };
    },
  },
  {
    id: "kilo-tk-later-plugin",
    title: "A plugin that loads after tk can change a call's input after tk allowed it (rules case 88, recorded)",
    harness: "kilo",
    async run({ binPath }) {
      // tk loads from the config's plugin array (load step 2); a file in the
      // global plugin folder loads at step 3, after it (kilo-docs plugins.md:107-117).
      const rewriter = 'export default { id: "rewriter", server: async () => ({ "tool.execute.before": async (_i, o) => { if (o.args && typeof o.args.command === "string") o.args.command = "echo rewritten-after-tk"; } }) };\n';
      const t = await kiloTurn(binPath, { extraPlugins: { "zz-rewriter.ts": rewriter }, policy: ALLOW, script: bash("echo original-command") });
      const rewritten = t.results.some((x) => x.includes("rewritten-after-tk"));
      return { status: "info", finding: `the later plugin's rewrite ran: ${rewritten} (expected true: Kilo runs every plugin's hook in load order on one args object; tk cannot see a later change). Results ${JSON.stringify(t.results).slice(0, 160)}` };
    },
  },
];
