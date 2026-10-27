// Contract checks for computer use (wave 3, lane C), run by
// tests/cli-contract/run.mjs. They answer one question per harness: does the
// stdio MCP server a harness starts see the two variables MonoCode sets on
// the harness process (MONOCODE_CU_ENDPOINT, MONOCODE_CU_TOKEN)? The bridge
// reads the chat's token from there, so a harness that drops them would leave
// computer use unreachable.
//
//
// The cu-shell checks ask the other way (security review X2): does the
// agent's own shell, the Bash tool or exec_command, see MONOCODE_CU_TOKEN?
// The fake model asks for one shell call that counts the variable's lines in
// `env`; the token's value is a dummy and is never printed.

import { spawn } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { codexHomeConfig, isolatedEnv, makeTempDirs } from "../lib/codexDriver.mjs";
import { ClaudeSession } from "../lib/claudeDriver.mjs";
import { CodexClient, userInput } from "../lib/codexDriver.mjs";
import { startFakeMessages, textBlock, toolUse, isSideRequest as claudeSide } from "../lib/fakeMessages.mjs";
import { startFakeResponses, text, call, isSideRequest as codexSide } from "../lib/fakeResponses.mjs";
import { codexShellEnvArgs } from "./otel.mjs";

const ENDPOINT = "MONOCODE_CU_ENDPOINT";
const TOKEN = "MONOCODE_CU_TOKEN";
const FAKE_ENDPOINT = "127.0.0.1:9";
const FAKE_TOKEN = "contract-check-token-not-a-secret";
const SERVER = "monocode_computer_use";

// A stdio MCP server that writes which of the two variables it was given,
// and answers initialize and tools/list.
const PROBE = `
import { writeFileSync } from "node:fs";
const out = process.argv[2];
writeFileSync(out, JSON.stringify({
  endpoint: process.env.${ENDPOINT} ?? null,
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
  // The flags MonoCode passes for the chat's computer-use server
  // (codexGatewayConfigArgs, plus env_vars under the [w3-c] hook in codex.ts).
  const args = [
    "app-server",
    "-c",
    `mcp_servers.${SERVER}.command=${JSON.stringify(process.execPath)}`,
    "-c",
    `mcp_servers.${SERVER}.args=${JSON.stringify([probe, out])}`,
  ];
  if (withEnvVars) args.push("-c", `mcp_servers.${SERVER}.env_vars=${JSON.stringify([ENDPOINT, TOKEN])}`);
  const child = spawn(codexPath, args, {
    cwd: dirs.work,
    env: isolatedEnv({ home: dirs.home, extra: { CODEX_HOME: dirs.codexHome, [ENDPOINT]: FAKE_ENDPOINT, [TOKEN]: FAKE_TOKEN } }),
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (d) => (stderr = (stderr + d).slice(-4000)));
  let id = 1;
  const send = (method, params) => child.stdin.write(`${JSON.stringify(params === undefined ? { method } : { id: id++, method, params })}\n`);
  try {
    send("initialize", { clientInfo: { name: "monocode", title: "MonoCode", version: "0.1.0" }, capabilities: { experimentalApi: true } });
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
  const reached = withVars.seen?.endpoint === FAKE_ENDPOINT && withVars.seen?.token === "matches";
  const dropped = !without.seen?.endpoint && !without.seen?.token;
  return {
    status: reached ? "pass" : "fail",
    finding: withVars.seen
      ? `With env_vars, the server saw endpoint ${withVars.seen.endpoint ? "yes" : "no"} and token ${withVars.seen.token ?? "no"}. ` +
        `Without env_vars it saw ${without.seen ? `endpoint ${without.seen.endpoint ? "yes" : "no"}, token ${without.seen.token ?? "no"}` : "nothing (did not start)"}: ` +
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
  // The --mcp-config MonoCode builds (claudeGatewayMcpConfig).
  const config = JSON.stringify({ mcpServers: { [SERVER]: { type: "stdio", command: process.execPath, args: [probe, out] } } });
  const s = new ClaudeSession({
    claudePath,
    dirs,
    baseUrl: fake.baseUrl,
    args: ["--mcp-config", config],
    env: { [ENDPOINT]: FAKE_ENDPOINT, [TOKEN]: FAKE_TOKEN },
  });
  try {
    let seen = await waitForFile(out, 8000);
    if (!seen) {
      await s.send("hello", 60000);
      seen = await waitForFile(out, 8000);
    }
    const reached = seen?.endpoint === FAKE_ENDPOINT && seen?.token === "matches";
    return {
      status: reached ? "pass" : "fail",
      finding: seen
        ? `A --mcp-config stdio server saw endpoint ${seen.endpoint ? "yes" : "no"} and token ${seen.token ?? "no"}: Claude Code ${reached ? "passes" : "does not pass"} its environment on.`
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
  // As MonoCode starts a chat with computer use: the two variables on the
  // Claude Code process (computer_use.rs `configure_child`).
  const s = new ClaudeSession({ claudePath, dirs, baseUrl: fake.baseUrl, env: { [ENDPOINT]: FAKE_ENDPOINT, [TOKEN]: FAKE_TOKEN } });
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
    status: count === null && !leaked ? "error" : seen ? "fail" : "pass",
    finding:
      count === null && !leaked
        ? "Could not read the Bash command's output."
        : seen
          ? `Claude Code's Bash tool sees ${TOKEN} (count ${count}${leaked ? ", value in the output" : ""}). Claude Code passes its whole environment to Bash; MonoCode has no per-variable exclude for it.`
          : `Claude Code's Bash tool does not see ${TOKEN}.`,
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
  // The variables go on Codex's process, as MonoCode sets them. The driver
  // starts Codex with its own environment, so a wrapper adds them.
  const launcher = path.join(dirs.root, "codex-with-env.sh");
  const q = (v) => `'${String(v).replace(/'/g, `'\\''`)}'`;
  writeFileSync(launcher, `#!/bin/sh\nexport ${ENDPOINT}=${q(FAKE_ENDPOINT)}\nexport ${TOKEN}=${q(FAKE_TOKEN)}\nexec ${q(codexPath)} "$@"\n`, { mode: 0o700 });
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
  // MonoCode's flags: the server's env_vars and the shell excludes
  // (`codex_shell_env_args`, which now holds MONOCODE_CU_*).
  const stub = path.join(path.dirname(new URL(import.meta.url).pathname), "..", "lib", "stubMcp.mjs");
  const serverArgs = [
    "-c",
    `mcp_servers.${SERVER}.command=${JSON.stringify(process.execPath)}`,
    "-c",
    `mcp_servers.${SERVER}.args=${JSON.stringify([stub])}`,
    "-c",
    `mcp_servers.${SERVER}.env_vars=${JSON.stringify([ENDPOINT, TOKEN])}`,
  ];
  const withExclude = await codexShellRun(codexPath, [...codexShellEnvArgs(), ...serverArgs]);
  const without = await codexShellRun(codexPath, serverArgs);
  const seen = withExclude.leaked || (withExclude.count ?? 0) > 0;
  return {
    status: withExclude.count === null && !withExclude.leaked ? "error" : seen ? "fail" : "pass",
    finding:
      `With MonoCode's shell excludes, exec_command saw ${TOKEN}: ${withExclude.count === null ? "unknown" : withExclude.count > 0 ? "yes" : "no"}. ` +
      `Without them: ${without.count === null ? "unknown" : without.count > 0 ? "yes" : "no"}.`,
    seen: { withExclude, without },
  };
}

export const computerUseCodexChecks = [
  {
    id: "cu-shell-codex",
    title: "Computer use: Codex's shell commands do not see the chat's bridge token",
    flag: "computer-use shell excludes",
    run: codexShellCheck,
  },
  {
    id: "cu-env-codex",
    title: "Computer use: the chat's MCP server gets MonoCode's endpoint and token on Codex",
    flag: "computer-use env_vars",
    run: codexEnvCheck,
  },
];

export const computerUseClaudeChecks = [
  {
    id: "cu-shell-claude",
    title: "Computer use: Claude Code's Bash tool and the chat's bridge token",
    flag: "computer-use env",
    run: claudeShellCheck,
  },
  {
    id: "cu-env-claude",
    title: "Computer use: the chat's MCP server gets MonoCode's endpoint and token on Claude Code",
    flag: "computer-use env",
    run: claudeEnvCheck,
  },
];
