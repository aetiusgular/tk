// Contract checks for computer use, run by tests/cli-contract/run.mjs.
// Copied from the fork (wave 3, lane C), where they tested MONOCODE_CU_*;
// rewritten by integration w2 for tk (lane K's request 9): tk's relay
// (`tk-daemon cu-mcp`, crates/tk-daemon/src/computer_use/relay.rs) reads the
// chat token `tk run` puts on the harness, TK_CHAT_TOKEN (or the file
// TK_CHAT_TOKEN_FILE names), and finds the bridge in `cu.endpoint` beside the
// daemon's socket, so no endpoint variable is involved.
//
// cu-env-*: does the stdio MCP server a harness starts see TK_CHAT_TOKEN? A
// harness that drops it leaves computer use unreachable.
//
// cu-shell-*: does the agent's own shell see it? A chat token is the
// agent's too (rules case 66): under tk it is expected in Claude Code's Bash
// (Claude Code passes its whole environment and has no per-variable
// exclude), so that check reports info. Codex can exclude it from its shell
// with shell_environment_policy.exclude; the check tests that mechanism,
// which `tk run codex` does not set yet (wave 4 lane R). The token's value is
// a dummy and is never printed.

import { spawn } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { codexHomeConfig, isolatedEnv, makeTempDirs } from "../lib/codexDriver.mjs";
import { ClaudeSession } from "../lib/claudeDriver.mjs";
import { CodexClient, userInput } from "../lib/codexDriver.mjs";
import { startFakeMessages, textBlock, toolUse, isSideRequest as claudeSide } from "../lib/fakeMessages.mjs";
import { startFakeResponses, text, call, isSideRequest as codexSide } from "../lib/fakeResponses.mjs";
import { codexShellEnvArgs } from "./otel.mjs";

const TOKEN = "TK_CHAT_TOKEN";
const TK_SHELL_EXCLUDES = ["TK_CHAT_TOKEN", "TK_CHAT_TOKEN_FILE"];
const FAKE_TOKEN = "contract-check-token-not-a-secret";
// Claude Code reserves the name computer-use (lane C); tk's server is tk_cu.
const SERVER = "tk_cu";

// A stdio MCP server that writes whether it was given the chat token, and
// answers initialize and tools/list.
const PROBE = `
import { writeFileSync } from "node:fs";
const out = process.argv[2];
writeFileSync(out, JSON.stringify({
  token: process.env.${TOKEN} === ${JSON.stringify(FAKE_TOKEN)} ? "matches" : (process.env.${TOKEN} ? "differs" : null),
}));
let buf = "";
process.stdin.on("data", (chunk) => {
  buf += chunk;
  let nl;
  while ((nl = buf.indexOf("\\n")) >= 0) {
    const line = buf.slice(0, nl).trim();
    buf = buf.slice(nl + 1);
    if (!line) continue;
    let msg;
    try { msg = JSON.parse(line); } catch { continue; }
    if (msg.id === undefined) continue;
    const result = msg.method === "initialize"
      ? { protocolVersion: msg.params?.protocolVersion ?? "2025-06-18", capabilities: { tools: {} }, serverInfo: { name: "probe", version: "1" } }
      : msg.method === "tools/list" ? { tools: [] } : {};
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result }) + "\\n");
  }
});
`;

function waitForFile(file, ms) {
  return new Promise((resolve) => {
    const started = Date.now();
    const tick = () => {
      if (existsSync(file)) return resolve(JSON.parse(readFileSync(file, "utf8")));
      if (Date.now() - started > ms) return resolve(null);
      setTimeout(tick, 100);
    };
    tick();
  });
}

/** One Codex app-server run with the probe as a server; what the probe saw. */
async function codexProbe(codexPath, withEnvVars) {
  const dirs = makeTempDirs("cu-codex-");
  const probe = path.join(dirs.root, "probe.mjs");
  const out = path.join(dirs.root, "seen.json");
  writeFileSync(probe, PROBE);
  const fake = await startFakeResponses({ script: () => [text("ok")] });
  writeFileSync(path.join(dirs.codexHome, "config.toml"), codexHomeConfig({ baseUrl: fake.baseUrl }));
  // tk's server entry for Codex (`tk connect codex`, an env_vars list).
  const args = [
    "app-server",
    "-c",
    `mcp_servers.${SERVER}.command=${JSON.stringify(process.execPath)}`,
    "-c",
    `mcp_servers.${SERVER}.args=${JSON.stringify([probe, out])}`,
  ];
  if (withEnvVars) args.push("-c", `mcp_servers.${SERVER}.env_vars=${JSON.stringify([TOKEN])}`);
  const child = spawn(codexPath, args, {
    cwd: dirs.work,
    env: isolatedEnv({ home: dirs.home, extra: { CODEX_HOME: dirs.codexHome, [TOKEN]: FAKE_TOKEN } }),
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (d) => (stderr = (stderr + d).slice(-4000)));
  let id = 1;
  const send = (method, params) => child.stdin.write(`${JSON.stringify(params === undefined ? { method } : { id: id++, method, params })}\n`);
  try {
    send("initialize", { clientInfo: { name: "tk", title: "tk contract check", version: "0.1.0" }, capabilities: { experimentalApi: true } });
    await new Promise((r) => setTimeout(r, 500));
    child.stdin.write(`${JSON.stringify({ method: "initialized" })}\n`);
    send("thread/start", { cwd: dirs.work });
    return { seen: await waitForFile(out, 20000), stderr };
  } finally {
    child.kill("SIGTERM");
    await fake.close?.();
  }
}

async function codexEnvCheck({ codexPath }) {
  const without = await codexProbe(codexPath, false);
  const withVars = await codexProbe(codexPath, true);
  const reached = withVars.seen?.token === "matches";
  const dropped = !without.seen?.token;
  return {
    status: reached ? "pass" : "fail",
    finding: withVars.seen
      ? `With env_vars, the server saw ${TOKEN}: ${withVars.seen.token ?? "no"}. ` +
        `Without env_vars it saw ${without.seen ? `${TOKEN}: ${without.seen.token ?? "no"}` : "nothing (did not start)"}: ` +
        `${dropped ? "Codex drops them unless named, so the flag is needed." : "Codex passes them on anyway."}`
      : `The probe server never started. Stderr: ${withVars.stderr.slice(-300)}`,
    seen: { with: withVars.seen, without: without.seen },
  };
}

async function claudeEnvCheck({ claudePath }) {
  const dirs = makeTempDirs("cu-claude-");
  const probe = path.join(dirs.root, "probe.mjs");
  const out = path.join(dirs.root, "seen.json");
  writeFileSync(probe, PROBE);
  const fake = await startFakeMessages({ script: () => [textBlock("ok")] });
  // A user-level stdio entry, as `tk connect claude-code` writes it.
  const config = JSON.stringify({ mcpServers: { [SERVER]: { type: "stdio", command: process.execPath, args: [probe, out] } } });
  const s = new ClaudeSession({
    claudePath,
    dirs,
    baseUrl: fake.baseUrl,
    args: ["--mcp-config", config],
    env: { [TOKEN]: FAKE_TOKEN },
  });
  try {
    let seen = await waitForFile(out, 8000);
    if (!seen) {
      await s.send("hello", 60000);
      seen = await waitForFile(out, 8000);
    }
    const reached = seen?.token === "matches";
    return {
      status: reached ? "pass" : "fail",
      finding: seen
        ? `A --mcp-config stdio server saw ${TOKEN}: ${seen.token ?? "no"}: Claude Code ${reached ? "passes" : "does not pass"} its environment on.`
        : `The probe server never started. Stderr: ${s.stderr.slice(-300)}`,
      seen,
    };
  } finally {
    try {
      await s.close();
    } catch {}
    await fake.close?.();
  }
}

// Prints a marker and how many lines of `env` name the token: 0 or 1.
const SHELL_COMMAND = `echo cu-shell-check; env | grep -c '^${TOKEN}=' || true`;
const SHELL_COUNT = /cu-shell-check\s*(?:\\n|\n)+\s*(\d+)/;

/** The number the shell printed, from the tool outputs the model was sent. */
function shellCount(outputs) {
  const joined = outputs.join("\n");
  const match = joined.match(SHELL_COUNT);
  return { count: match ? Number(match[1]) : null, leaked: joined.includes(FAKE_TOKEN) };
}

async function claudeShellCheck({ claudePath }) {
  const dirs = makeTempDirs("cu-shell-claude-");
  const fake = await startFakeMessages({
    script: (body) => {
      if (claudeSide(body)) return [textBlock("Shell check")];
      const hasResult = JSON.stringify(body.messages ?? []).includes("tool_result");
      return hasResult ? [textBlock("done")] : [toolUse("Bash", { command: SHELL_COMMAND, description: "Count a variable" })];
    },
  });
  // As `tk run claude-code` starts a chat: the chat token on the process.
  const s = new ClaudeSession({ claudePath, dirs, baseUrl: fake.baseUrl, env: { [TOKEN]: FAKE_TOKEN } });
  try {
    await s.send("Run the check command.", 120000);
  } finally {
    try {
      await s.close();
    } catch {}
    await fake.close?.();
  }
  const outputs = (fake.requests ?? []).flatMap((r) =>
    (r.body?.messages ?? []).flatMap((m) =>
      (Array.isArray(m.content) ? m.content : [])
        .filter((c) => c?.type === "tool_result")
        .map((c) => (typeof c.content === "string" ? c.content : JSON.stringify(c.content))),
    ),
  );
  const { count, leaked } = shellCount(outputs);
  const seen = leaked || (count ?? 0) > 0;
  return {
    status: count === null && !leaked ? "error" : "info",
    finding:
      count === null && !leaked
        ? "Could not read the Bash command's output."
        : seen
          ? `Claude Code's Bash tool sees ${TOKEN} (count ${count}${leaked ? ", value in the output" : ""}), as expected under tk: Claude Code passes its whole environment to Bash, and a chat token is the agent's too (rules case 66; it posts only into its own chat and an app grant still needs an approver's yes).`
          : `Claude Code's Bash tool does not see ${TOKEN}; Claude Code changed how it passes its environment, so recheck case 66's notes.`,
    seen: { count, leaked },
  };
}

/** One Codex turn that runs the shell command; what it printed. */
async function codexShellRun(codexPath, args) {
  const dirs = makeTempDirs("cu-shell-codex-");
  const fake = await startFakeResponses({
    script: (body) => {
      if (codexSide(body)) return [text("Shell check")];
      const hasOutput = (body.input ?? []).some((i) => i?.type === "function_call_output");
      return hasOutput ? [text("done")] : [call("exec_command", { cmd: SHELL_COMMAND }, "cu1")];
    },
  });
  // The chat token goes on Codex's process, as `tk run codex` sets it. The
  // driver starts Codex with its own environment, so a wrapper adds it.
  const launcher = path.join(dirs.root, "codex-with-env.sh");
  const q = (v) => `'${String(v).replace(/'/g, `'\\''`)}'`;
  writeFileSync(launcher, `#!/bin/sh\nexport ${TOKEN}=${q(FAKE_TOKEN)}\nexec ${q(codexPath)} "$@"\n`, { mode: 0o700 });
  const client = new CodexClient({ codexPath: launcher, dirs, baseUrl: fake.url ?? fake.baseUrl, args });
  try {
    await client.initialize();
    const started = await client.request("thread/start", { cwd: dirs.work, approvalPolicy: "never", sandbox: "danger-full-access" });
    await client.runTurn({ threadId: started.thread.id, input: userInput("Run the check command."), approvalPolicy: "never", sandboxPolicy: { type: "dangerFullAccess" } }, 120000);
  } finally {
    await client.close();
    await fake.close?.();
  }
  const outputs = (fake.requests ?? [])
    .flatMap((r) => r.body?.input ?? [])
    .filter((i) => i?.type === "function_call_output")
    .map((i) => (typeof i.output === "string" ? i.output : JSON.stringify(i.output)));
  return shellCount(outputs);
}

async function codexShellCheck({ codexPath }) {
  // The server's env_vars, and Codex's shell excludes with tk's two
  // variables added: the exclude wave 4 lane R is to set in `tk run codex`.
  const stub = path.join(path.dirname(new URL(import.meta.url).pathname), "..", "lib", "stubMcp.mjs");
  const serverArgs = [
    "-c",
    `mcp_servers.${SERVER}.command=${JSON.stringify(process.execPath)}`,
    "-c",
    `mcp_servers.${SERVER}.args=${JSON.stringify([stub])}`,
    "-c",
    `mcp_servers.${SERVER}.env_vars=${JSON.stringify([TOKEN])}`,
  ];
  const excludeArgs = codexShellEnvArgs();
  excludeArgs[1] = excludeArgs[1].replace(/\]$/, `, ${TK_SHELL_EXCLUDES.map((v) => JSON.stringify(v)).join(", ")}]`);
  const withExclude = await codexShellRun(codexPath, [...excludeArgs, ...serverArgs]);
  const without = await codexShellRun(codexPath, serverArgs);
  const seen = withExclude.leaked || (withExclude.count ?? 0) > 0;
  return {
    status: withExclude.count === null && !withExclude.leaked ? "error" : seen ? "fail" : "pass",
    finding:
      `With ${TOKEN} in shell_environment_policy.exclude, exec_command saw it: ${withExclude.count === null ? "unknown" : withExclude.count > 0 ? "yes" : "no"}. ` +
      `Without the exclude: ${without.count === null ? "unknown" : without.count > 0 ? "yes" : "no"}. tk run codex does not set the exclude yet (wave 4 lane R).`,
    seen: { withExclude, without },
  };
}

export const computerUseCodexChecks = [
  {
    id: "cu-shell-codex",
    title: "Computer use: Codex's shell can be kept from the chat token (shell_environment_policy.exclude)",
    flag: "computer-use shell excludes",
    run: codexShellCheck,
  },
  {
    id: "cu-env-codex",
    title: "Computer use: the chat's MCP server gets the chat token on Codex",
    flag: "computer-use env_vars",
    run: codexEnvCheck,
  },
];

export const computerUseClaudeChecks = [
  {
    id: "cu-shell-claude",
    title: "Computer use: Claude Code's Bash tool and the chat token (case 66)",
    flag: "computer-use env",
    run: claudeShellCheck,
  },
  {
    id: "cu-env-claude",
    title: "Computer use: the chat's MCP server gets the chat token on Claude Code",
    flag: "computer-use env",
    run: claudeEnvCheck,
  },
];
