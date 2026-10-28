// Lane I: contract checks for the tk Gemini CLI extension (T2.4) against the
// real CLI. The extension is installed the way a user would
// (`gemini extensions install <path> --consent`) into a scratch HOME, then
// `gemini -p` runs against the fake Gemini API (../../lib/fakeGemini.mjs)
// with a fake tk daemon. Every run is headless with `--approval-mode yolo`:
// Orca's full-access flag for Gemini is `--yolo` (note AQ section 8, rules
// case 83), and it is the only headless mode that offers run_shell_command at
// all (default mode drops it from the tool list; run 28 Oct).
//
//   node tests/cli-contract/run.mjs --cli gemini=/path/to/gemini --only gemini-tk-deny,...

import { spawn, execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { functionCall, functionResponses, startFakeGemini, text } from "../../lib/fakeGemini.mjs";
import { startFakeDaemon } from "../../../../packages/tk-claude-plugin/src/testing/fakeDaemon.ts";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const EXTENSION = path.resolve(HERE, "..", "..", "..", "..", "packages", "tk-gemini-extension");

function run(bin, args, env, cwd, seconds = 120, input) {
  return new Promise((resolve) => {
    const child = spawn(bin, args, { env, cwd, stdio: [input === undefined ? "ignore" : "pipe", "pipe", "pipe"] });
    if (input !== undefined) child.stdin.end(input);
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
 * A scratch HOME with the extension installed. `edit` may change the copy
 * before it is installed (a shorter timeout for the fail-open check).
 */
async function setup(bin, { settings = {}, policy, edit } = {}) {
  const root = mkdtempSync(path.join(process.env.CLI_CONTRACT_TMP ?? tmpdir(), "gemini-tk-"));
  const home = path.join(root, "home");
  const work = path.join(root, "work");
  mkdirSync(path.join(home, ".gemini"), { recursive: true });
  mkdirSync(path.join(home, ".tk"), { recursive: true, mode: 0o700 });
  mkdirSync(work);
  const policyFile = path.join(home, ".tk", "policy.json");
  if (policy) writeFileSync(policyFile, JSON.stringify(policy), { mode: 0o600 });
  writeFileSync(path.join(home, ".gemini", "settings.json"), JSON.stringify({ security: { auth: { selectedType: "gemini-api-key" } }, ...settings }));
  let source = EXTENSION;
  if (edit) {
    source = path.join(root, "ext-src");
    cpSync(EXTENSION, source, { recursive: true, filter: (p) => !p.includes("node_modules") && !p.endsWith(".test.ts") });
    edit(source);
  }
  const env = { PATH: process.env.PATH, HOME: home, NO_COLOR: "1", GEMINI_CLI_NO_RELAUNCH: "true" };
  // Gemini 0.66.0 asks whether to trust the source folder before it installs
  // from a path, even with --consent; with no answer it installs nothing and
  // still exits 0 (run 28 Oct). A user answers y; so does this.
  const install = await run(bin, ["extensions", "install", source, "--consent"], env, work, 120, "y\n");
  return { root, home, work, policyFile, install, installedAt: path.join(home, ".gemini", "extensions", "tk") };
}

async function turn(bin, s, fake, extraEnv = {}) {
  const env = {
    PATH: process.env.PATH,
    HOME: s.home,
    GEMINI_API_KEY: "fake-key",
    GOOGLE_GEMINI_BASE_URL: fake.baseUrl,
    GEMINI_CLI_TRUST_WORKSPACE: "true",
    GEMINI_CLI_NO_RELAUNCH: "true",
    NO_COLOR: "1",
    ...extraEnv,
  };
  return await run(bin, ["-p", "run the step", "--approval-mode", "yolo", "--output-format", "json"], env, s.work);
}

/** One scripted tool call, then a plain answer. */
function oneCall(name, args) {
  return (_body, n) => (n === 0 ? [functionCall(name, args)] : [text("done")]);
}

function responsesOf(fake) {
  return fake.turnRequests().flatMap((t) => functionResponses(t.body));
}

/** `script` is a fake-model script, or a function of the scratch setup that returns one. */
async function withRun(bin, opts, script, fn) {
  const s = await setup(bin, opts);
  const fake = await startFakeGemini({ script: script.length === 1 ? script(s) : script });
  const daemon = await startFakeDaemon({ approve: opts.approve ?? "none", root: "/tmp" });
  try {
    if (s.install.code !== 0 || !existsSync(path.join(s.installedAt, "hooks", "hooks.json"))) {
      return { status: "fail", finding: `extension install failed: exit ${s.install.code}; ${s.install.err.slice(-300)}` };
    }
    const daemonEnv = opts.noDaemon ? {} : opts.daemonEnv ? opts.daemonEnv(daemon, s) : daemon.env();
    const r = await turn(bin, s, fake, { TK_POLICY: s.policyFile, ...daemonEnv, ...(opts.env ?? {}) });
    return await fn({ r, fake, daemon, s, responses: responsesOf(fake) });
  } finally {
    await fake.close();
    await daemon.close();
    rmSync(s.root, { recursive: true, force: true });
  }
}

const SHELL_ALLOW = { version: 1, askTimeoutSeconds: 5, rules: [{ tool: "run_shell_command", action: "allow" }, { tool: "write_file", action: "allow" }] };

export const checks = [
  {
    id: "gemini-tk-install",
    title: "`gemini extensions install <path> --consent` installs tk with its hooks and policies",
    harness: "gemini",
    async run({ binPath }) {
      const s = await setup(binPath, {});
      try {
        const files = ["gemini-extension.json", "hooks/hooks.json", "policies/tk.toml", "bin/tk-hook.mjs", "src/hook.ts", "src/core/gate.ts"].map((f) => [f, existsSync(path.join(s.installedAt, f))]);
        // `extensions list` prints to stderr at 0.66.0 (run 28 Oct).
        const l = await run(binPath, ["extensions", "list"], { PATH: process.env.PATH, HOME: s.home, NO_COLOR: "1", GEMINI_CLI_NO_RELAUNCH: "true" }, s.work, 60);
        const listed = /extensions[\\/]tk\b/.test(l.out + l.err);
        const ok = s.install.code === 0 && files.every(([, e]) => e) && listed;
        return { status: ok ? "pass" : "fail", finding: `install exit ${s.install.code}; files ${JSON.stringify(Object.fromEntries(files))}; listed: ${listed}${ok ? "" : `; ${(l.out + l.err).slice(-300)}`}` };
      } finally {
        rmSync(s.root, { recursive: true, force: true });
      }
    },
  },
  {
    id: "gemini-tk-allow",
    title: "An allowed shell call runs and the daemon gets the decision and the result record",
    harness: "gemini",
    run: ({ binPath }) =>
      withRun(binPath, { policy: SHELL_ALLOW }, oneCall("run_shell_command", { command: "echo tk-allowed-ran" }), ({ r, responses, daemon }) => {
        const ran = JSON.stringify(responses).includes("tk-allowed-ran");
        const kinds = daemon.events().map((e) => `${e.kind}:${e.harness}`);
        const ok = r.code === 0 && ran && kinds.includes("approval:gemini") && kinds.includes("tool.call:gemini");
        return { status: ok ? "pass" : "fail", finding: `exit ${r.code}; ran ${ran}; daemon records ${JSON.stringify(kinds)}` };
      }),
  },
  {
    id: "gemini-tk-deny",
    title: "A denied shell call does not run, under --yolo; the model gets tk's reason",
    harness: "gemini",
    run: ({ binPath }) =>
      withRun(binPath, { policy: { version: 1, rules: [{ tool: "run_shell_command", action: "deny" }] } }, oneCall("run_shell_command", { command: "echo tk-denied-ran" }), ({ r, responses }) => {
        const text = JSON.stringify(responses);
        const ran = text.includes("tk-denied-ran\\n") || /Output: tk-denied-ran/.test(text);
        const reason = text.includes("tk: rule run_shell_command");
        return { status: !ran && reason ? "pass" : "fail", finding: `exit ${r.code}; ran ${ran}; reason reached the model ${reason}; ${text.slice(0, 200)}` };
      }),
  },
  {
    id: "gemini-tk-reject",
    title: "The reject list blocks git -c core.fsmonitor under an allow rule",
    harness: "gemini",
    run: ({ binPath }) =>
      withRun(binPath, { policy: SHELL_ALLOW }, oneCall("run_shell_command", { command: "git -c core.fsmonitor=/tmp/x status" }), ({ responses }) => {
        const text = JSON.stringify(responses);
        const ok = text.includes("reject list: core.fsmonitor") && !text.includes("Output:");
        return { status: ok ? "pass" : "fail", finding: text.slice(0, 240) };
      }),
  },
  {
    id: "gemini-tk-ask-approver",
    title: "An ask goes to the tk approver; a yes runs the call",
    harness: "gemini",
    run: ({ binPath }) =>
      withRun(binPath, { approve: "yes", policy: { version: 1, askTimeoutSeconds: 5, rules: [{ tool: "run_shell_command", action: "ask" }] } }, oneCall("run_shell_command", { command: "echo tk-approved-ran" }), ({ responses, daemon }) => {
        const ran = JSON.stringify(responses).includes("tk-approved-ran");
        const asked = daemon.approvals().filter((a) => a.harness === "gemini").length;
        return { status: ran && asked === 1 ? "pass" : "fail", finding: `approvals ${asked}; ran ${ran}` };
      }),
  },
  {
    id: "gemini-tk-ask-headless",
    title: "An ask with no approver goes to Gemini's own prompt, which a headless run does not pass",
    harness: "gemini",
    run: ({ binPath }) =>
      withRun(binPath, { approve: "none", policy: { version: 1, askTimeoutSeconds: 5, rules: [{ tool: "run_shell_command", action: "ask" }] } }, oneCall("run_shell_command", { command: "echo tk-unapproved-ran" }), ({ r, responses }) => {
        const ran = JSON.stringify(responses).includes("tk-unapproved-ran");
        return { status: ran ? "fail" : "pass", finding: `exit ${r.code}; ran ${ran}; responses ${JSON.stringify(responses).slice(0, 160)}` };
      }),
  },
  {
    id: "gemini-tk-redacted-env",
    title: "With Gemini's environment redaction on, the chat token file still reaches the hook through GEMINI_CLI_TK_CHAT_TOKEN_FILE",
    harness: "gemini",
    run: ({ binPath }) =>
      withRun(
        binPath,
        {
          approve: "yes",
          settings: { security: { auth: { selectedType: "gemini-api-key" }, environmentVariableRedaction: { enabled: true } } },
          policy: { version: 1, askTimeoutSeconds: 5, rules: [{ tool: "run_shell_command", action: "ask" }] },
          daemonEnv: (daemon, s) => {
            const file = path.join(s.home, ".tk", "chat.token");
            writeFileSync(file, `${daemon.chatToken}\n`, { mode: 0o600 });
            return { TK_DAEMON_SOCKET: daemon.socket, TK_CHAT_TOKEN_FILE: file, GEMINI_CLI_TK_CHAT_TOKEN_FILE: file };
          },
        },
        oneCall("run_shell_command", { command: "echo tk-redacted-env-ran" }),
        ({ responses, daemon }) => {
          const ran = JSON.stringify(responses).includes("tk-redacted-env-ran");
          const asked = daemon.approvals().length;
          return { status: ran && asked === 1 ? "pass" : "fail", finding: `approvals ${asked}; ran ${ran}` };
        },
      ),
  },
  {
    id: "gemini-tk-second-layer",
    title: "With the hook switched off, the extension's policy still denies a write into .gemini/",
    harness: "gemini",
    async run({ binPath }) {
      return await withRun(
        binPath,
        { policy: SHELL_ALLOW, settings: { security: { auth: { selectedType: "gemini-api-key" } }, hooksConfig: { disabled: ["tk-gate"] } } },
        // Inside the workspace: Gemini refuses a write outside it on its own, which would prove nothing here.
        (s) => oneCall("write_file", { file_path: path.join(s.work, ".gemini", "settings.json"), content: '{"hooksConfig":{"enabled":false}}' }),
        async ({ responses, daemon, s }) => {
          const text = JSON.stringify(responses);
          const changed = existsSync(path.join(s.work, ".gemini", "settings.json"));
          const hookRan = daemon.events().some((e) => e.kind === "approval");
          return { status: !changed && !hookRan && /denied|refused|tk:/i.test(text) ? "pass" : "fail", finding: `policy changed ${changed}; hook ran ${hookRan}; ${text.slice(0, 240)}` };
        },
      );
    },
  },
  {
    id: "gemini-tk-user-policy",
    title: "The printed user-tier TOML loads in Gemini and enforces tk's rules with the hook switched off",
    harness: "gemini",
    async run({ binPath }) {
      const policy = { version: 1, rules: [{ tool: "run_shell_command", action: "deny" }, { tool: "read_file", action: "allow" }] };
      return await withRun(
        binPath,
        { policy, settings: { security: { auth: { selectedType: "gemini-api-key" } }, hooksConfig: { disabled: ["tk-gate"] } } },
        (s) => {
          const printed = execFileSync(process.execPath, ["--no-warnings", path.join(EXTENSION, "bin", "tk-gemini-policy.mjs")], { env: { PATH: process.env.PATH, HOME: s.home, TK_POLICY: s.policyFile }, encoding: "utf8" });
          mkdirSync(path.join(s.home, ".gemini", "policies"), { recursive: true });
          writeFileSync(path.join(s.home, ".gemini", "policies", "tk.toml"), printed);
          return oneCall("run_shell_command", { command: "echo tk-user-policy-ran" });
        },
        ({ r, responses }) => {
          const text = JSON.stringify(responses);
          const ran = /Output: tk-user-policy-ran/.test(text);
          const offered = text.includes("not found") ? "the tool was dropped from the list (a global deny)" : "the call was denied";
          const loadError = /policy|toml/i.test(r.err) && /error|invalid/i.test(r.err);
          return { status: !ran && !loadError ? "pass" : "fail", finding: `ran ${ran}; ${offered}; policy load errors on stderr ${loadError}; ${text.slice(0, 200)}` };
        },
      );
    },
  },
  {
    id: "gemini-tk-fail-open",
    title: "A hung gate lets a shell call through (Gemini fails open, case 70); the extension's policy still denies a control-file write",
    harness: "gemini",
    async run({ binPath }) {
      // A FIFO as the policy file: the hook's read blocks until Gemini's timeout (1 s in this copy).
      const s = await setup(binPath, {
        edit: (dir) => {
          const f = path.join(dir, "hooks", "hooks.json");
          const h = JSON.parse(readFileSync(f, "utf8"));
          h.hooks.BeforeTool[0].hooks[0].timeout = 1000;
          writeFileSync(f, JSON.stringify(h));
        },
      });
      const fake = await startFakeGemini({
        script: (_b, n) => (n === 0 ? [functionCall("run_shell_command", { command: "echo tk-fail-open-ran" }), functionCall("write_file", { file_path: path.join(s.work, ".gemini", "x.json"), content: "{}" })] : [text("done")]),
      });
      try {
        const fifo = path.join(s.root, "policy.fifo");
        execFileSync("mkfifo", [fifo]);
        const r = await turn(binPath, s, fake, { TK_POLICY: fifo });
        const text = JSON.stringify(responsesOf(fake));
        const shellRan = text.includes("tk-fail-open-ran");
        const wrote = existsSync(path.join(s.work, ".gemini", "x.json"));
        const byPolicy = /policy|refused by the tk extension/i.test(text);
        return {
          status: shellRan && !wrote && byPolicy ? "info" : "fail",
          finding: `exit ${r.code}; shell call ran with the gate hung: ${shellRan} (expected true: Gemini fails open, no switch); control-file write blocked: ${!wrote}, by the extension's policy: ${byPolicy}. ${text.slice(0, 200)}`,
        };
      } finally {
        await fake.close();
        rmSync(s.root, { recursive: true, force: true });
      }
    },
  },
  {
    id: "gemini-tk-beside-observer",
    title: "Beside another BeforeTool hook that answers {} (Orca's kind, case 75), a deny still wins and tk's gate runs once",
    harness: "gemini",
    async run({ binPath }) {
      const observer = path.join(tmpdir(), `tk-observer-${process.pid}.mjs`);
      writeFileSync(observer, 'let s="";for await (const c of process.stdin) s+=c; process.stdout.write("{}"); \n');
      try {
        return await withRun(
          binPath,
          {
            policy: { version: 1, rules: [{ tool: "run_shell_command", action: "deny" }] },
            settings: { security: { auth: { selectedType: "gemini-api-key" } }, hooks: { BeforeTool: [{ matcher: ".*", hooks: [{ name: "observer", type: "command", command: `node ${observer}`, timeout: 5000 }] }] } },
          },
          oneCall("run_shell_command", { command: "echo tk-observer-ran" }),
          ({ responses, daemon }) => {
            const text = JSON.stringify(responses);
            const ran = /Output: tk-observer-ran/.test(text);
            const decisions = daemon.events().filter((e) => e.kind === "approval").length;
            return { status: !ran && decisions === 1 ? "pass" : "fail", finding: `ran ${ran}; tk decisions ${decisions}; ${text.slice(0, 160)}` };
          },
        );
      } finally {
        rmSync(observer, { force: true });
      }
    },
  },
];
