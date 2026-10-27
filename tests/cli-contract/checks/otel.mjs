// Wave 3, lane A6: does each harness export its OpenTelemetry log events to
// MonoCode's loopback receiver, with the chat's token, when started the way
// MonoCode starts it?
//
// The environment and flags mirror `src-tauri/src/otel_receiver/harness_env.rs`
// (`claude_env`, `headers_helper_script`, `settings_with_helper`,
// `codex_args`, `codex_shell_env_args`, `codex_env`); keep the two in step.
// The token never goes in an argument. The receiver here is a stand-in in Node that
// records each request; the real receiver's decoding is covered by its Rust
// tests, which also decode the payloads this check saves when
// OTEL_FIXTURES_DIR is set.
//
// Pass: a POST to /v1/logs carrying the token, with a tool result for the
// call the fake model made. A harness that sends nothing is "info, no
// export": MonoCode's adapter events stand for it. A harness that exports
// without the token fails: the receiver would refuse every request.

import http from "node:http";
import { randomBytes } from "node:crypto";
import { chmodSync, mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { startFakeMessages, textBlock, toolUse, isSideRequest as claudeSide } from "../lib/fakeMessages.mjs";
import { startFakeResponses, call, text, isSideRequest as codexSide } from "../lib/fakeResponses.mjs";
import { ClaudeSession } from "../lib/claudeDriver.mjs";
import { CodexClient, makeTempDirs, userInput } from "../lib/codexDriver.mjs";

const TOKEN_HEADER = "x-monocode-token";

/**
 * `claude_env` in harness_env.rs plus `claude_header_var`: the header
 * variable is now only the fallback where no helper script can be made.
 */
export function claudeEnv(endpoint, token, logPrompts) {
  const env = {
    CLAUDE_CODE_ENABLE_TELEMETRY: "1",
    OTEL_LOGS_EXPORTER: "otlp",
    OTEL_EXPORTER_OTLP_PROTOCOL: "http/json",
    OTEL_EXPORTER_OTLP_ENDPOINT: endpoint,
    OTEL_EXPORTER_OTLP_HEADERS: `${TOKEN_HEADER}=${token}`,
    OTEL_LOGS_EXPORT_INTERVAL: "1000",
    OTEL_LOG_TOOL_DETAILS: "1",
  };
  if (logPrompts) env.OTEL_LOG_USER_PROMPTS = "1";
  return env;
}

/**
 * `claude_env` with the header helper (Mac test 2.6): no header variable;
 * the token reaches Claude Code only through `otelHeadersHelper` in
 * `--settings`, a script that prints `{"x-monocode-token":"<token>"}`.
 */
export function claudeHelperEnv(endpoint, logPrompts) {
  const env = claudeEnv(endpoint, "", logPrompts);
  delete env.OTEL_EXPORTER_OTLP_HEADERS;
  return env;
}

/** The helper script, as `headers_helper_script` in harness_env.rs writes it. */
export function headersHelperScript(token) {
  return `#!/bin/sh\nprintf '%s\\n' '{"${TOKEN_HEADER}":"${token}"}'\n`;
}

/** `codex_args` in harness_env.rs. */
export function codexArgs(endpoint, logPrompts) {
  return [
    "-c",
    `otel.exporter={otlp-http={endpoint=${JSON.stringify(`${endpoint}/v1/logs`)},protocol="json"}}`,
    "-c",
    'otel.trace_exporter="none"',
    "-c",
    'otel.metrics_exporter="none"',
    "-c",
    `otel.log_user_prompt=${logPrompts}`,
  ];
}

/** `codex_shell_env_args` in harness_env.rs, for a home with no policy. */
export function codexShellEnvArgs() {
  return ["-c", `shell_environment_policy.exclude=${JSON.stringify([...CODEX_TOKEN_VARS, ...CODEX_SHELL_EXCLUDES]).replace(/,/g, ", ")}`];
}

const CODEX_TOKEN_VARS = ["OTEL_EXPORTER_OTLP_HEADERS", "OTEL_EXPORTER_OTLP_LOGS_HEADERS", "OTEL_EXPORTER_OTLP_METRICS_HEADERS", "OTEL_EXPORTER_OTLP_TRACES_HEADERS"];
/** `CODEX_SHELL_EXCLUDES` in harness_env.rs: the computer-use bridge's variables. */
const CODEX_SHELL_EXCLUDES = ["MONOCODE_CU_*"];

/** `codex_env` in harness_env.rs. */
export function codexEnv(token) {
  const header = `${TOKEN_HEADER}=${token}`;
  return { OTEL_EXPORTER_OTLP_HEADERS: header, OTEL_EXPORTER_OTLP_LOGS_HEADERS: header };
}

/** A recording OTLP receiver on loopback. */
export async function startStandInReceiver(token) {
  const requests = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const raw = Buffer.concat(chunks);
      const type = (req.headers["content-type"] ?? "").split(";")[0].trim();
      let json = null;
      if (type === "application/json") {
        try {
          json = JSON.parse(raw.toString("utf8"));
        } catch {}
      }
      const authorized = req.headers[TOKEN_HEADER] === token || req.headers.authorization === `Bearer ${token}`;
      requests.push({ method: req.method, path: req.url, type, authorized, bytes: raw.length, json, raw });
      res.writeHead(authorized ? 200 : 401, { "content-type": type === "application/x-protobuf" ? type : "application/json" });
      res.end(type === "application/x-protobuf" ? "" : "{}");
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return {
    endpoint: `http://127.0.0.1:${server.address().port}`,
    requests,
    close: () => new Promise((r) => server.close(r)),
  };
}

/** Every log record in the JSON requests received, flattened. */
export function logRecords(requests) {
  const out = [];
  for (const r of requests) {
    if (!r.json || !String(r.path).startsWith("/v1/logs")) continue;
    for (const rl of r.json.resourceLogs ?? []) {
      for (const sl of rl.scopeLogs ?? []) {
        for (const rec of sl.logRecords ?? []) {
          const attrs = {};
          for (const kv of rec.attributes ?? []) {
            const v = kv.value ?? {};
            attrs[kv.key] = v.stringValue ?? v.intValue ?? v.doubleValue ?? v.boolValue ?? (v.arrayValue || v.kvlistValue ? "<nested>" : null);
          }
          const name = attrs["event.name"] ?? rec.eventName ?? rec.body?.stringValue ?? "";
          out.push({ name, attrs, authorized: r.authorized });
        }
      }
    }
  }
  return out;
}

/** A launcher that sets and unsets variables, then runs the real CLI. */
function wrapper(dir, bin, set, unset) {
  const file = path.join(dir, "cli-with-env.sh");
  const q = (s) => `'${String(s).replace(/'/g, `'\\''`)}'`;
  const lines = ["#!/bin/sh", ...unset.map((n) => `unset ${n}`), ...Object.entries(set).map(([k, v]) => `export ${k}=${q(v)}`), `exec ${q(bin)} "$@"`];
  writeFileSync(file, `${lines.join("\n")}\n`);
  chmodSync(file, 0o700);
  return file;
}

function saveFixtures(name, requests) {
  const dir = process.env.OTEL_FIXTURES_DIR;
  if (!dir) return;
  mkdirSync(dir, { recursive: true });
  requests
    .filter((r) => String(r.path).startsWith("/v1/logs"))
    .forEach((r, i) => writeFileSync(path.join(dir, `${name}-${i}.${r.type === "application/json" ? "json" : "pb"}`), r.raw));
}

function waitUntil(predicate, ms) {
  return new Promise((resolve) => {
    const started = Date.now();
    const tick = () => {
      if (predicate() || Date.now() - started > ms) resolve();
      else setTimeout(tick, 200);
    };
    tick();
  });
}

function verdict(harness, requests, records, toolPredicate) {
  const logs = requests.filter((r) => String(r.path).startsWith("/v1/logs"));
  const names = [...new Set(records.map((r) => r.name))].sort();
  const tool = records.find((r) => r.authorized && toolPredicate(r));
  const unauthorized = requests.filter((r) => !r.authorized).length;
  // Off, both harnesses send a redaction mark in place of the text.
  const redacted = (v) => v == null || /^[<[]REDACTED[>\]]$/.test(String(v));
  const promptSeen = records.some((r) => !redacted(r.attrs.prompt) || !redacted(r.attrs.prompt_text));
  const types = [...new Set(logs.map((r) => r.type))];
  const paths = [...new Set(requests.map((r) => r.path))];
  const seen = { requests: requests.length, paths, types, unauthorized, names, toolAttributes: tool ? Object.keys(tool.attrs).sort() : [], promptSeen };
  if (requests.length === 0) {
    return { status: "info", finding: `${harness}: no export. Nothing reached the receiver; MonoCode's adapter events stand for this harness.`, seen, exports: false };
  }
  if (logs.length > 0 && logs.every((r) => !r.authorized)) {
    return { status: "fail", finding: `${harness}: exported ${logs.length} log request(s) without the token header from the environment; the receiver would refuse them all.`, seen, exports: true };
  }
  if (!tool) {
    return { status: logs.some((r) => r.authorized) ? "fail" : "info", finding: `${harness}: ${requests.length} request(s), none a tool result for the call made. Events: ${names.join(", ") || "none"}.`, seen, exports: true };
  }
  return {
    status: promptSeen ? "fail" : "pass",
    finding: `${harness}: ${logs.length} log request(s) with the token (${types.join(", ")}); tool result seen (${Object.keys(tool.attrs).length} attributes); prompt text ${promptSeen ? "PRESENT though switched off" : "absent with the switch off"}. Events: ${names.join(", ")}.${unauthorized ? ` ${unauthorized} request(s) without the token.` : ""}`,
    seen,
    exports: true,
  };
}

async function claudeExport({ claudePath, version }) {
  const token = randomBytes(32).toString("hex");
  const receiver = await startStandInReceiver(token);
  const dirs = makeTempDirs("otel-claude-");
  const fake = await startFakeMessages({
    script: (body, n) => {
      if (claudeSide(body)) return [textBlock("Telemetry check")];
      const hasResult = JSON.stringify(body.messages ?? []).includes("tool_result");
      return hasResult ? [textBlock("done")] : [toolUse("Bash", { command: "echo telemetry-check", description: "Print a word" })];
    },
  });
  // MonoCode sets none of the opt-out variables the shared driver sets.
  const launcher = wrapper(dirs.root, claudePath, claudeEnv(receiver.endpoint, token, false), ["DISABLE_TELEMETRY", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "DO_NOT_TRACK"]);
  const session = new ClaudeSession({ claudePath: launcher, dirs, baseUrl: fake.url ?? fake.baseUrl });
  try {
    await session.send("Run the check command.", 120000);
    await waitUntil(() => logRecords(receiver.requests).some((r) => r.name.endsWith("tool_result")), 15000);
  } finally {
    await session.close();
    await waitUntil(() => false, 1500);
    await fake.close?.();
    await receiver.close();
  }
  saveFixtures(`claude-${String(version ?? "").split(" ")[0]}`, receiver.requests);
  const records = logRecords(receiver.requests);
  return verdict("Claude Code", receiver.requests, records, (r) => r.name.endsWith("tool_result") && r.attrs.tool_name === "Bash");
}

async function claudeHelperExport({ claudePath, version }) {
  const token = randomBytes(32).toString("hex");
  const receiver = await startStandInReceiver(token);
  const dirs = makeTempDirs("otel-claude-helper-");
  const fake = await startFakeMessages({
    script: (body) => {
      if (claudeSide(body)) return [textBlock("Telemetry check")];
      const hasResult = JSON.stringify(body.messages ?? []).includes("tool_result");
      return hasResult ? [textBlock("done")] : [toolUse("Bash", { command: "echo telemetry-check", description: "Print a word" })];
    },
  });
  // A space in the path, as in macOS's "Application Support".
  const helperDir = path.join(dirs.root, "app data", "otel-helper");
  mkdirSync(path.dirname(helperDir), { recursive: true });
  mkdirSync(helperDir, { mode: 0o700 });
  const helper = path.join(helperDir, "headers.sh");
  writeFileSync(helper, headersHelperScript(token), { mode: 0o700, flag: "wx" });
  const launcher = wrapper(dirs.root, claudePath, claudeHelperEnv(receiver.endpoint, false), ["DISABLE_TELEMETRY", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "DO_NOT_TRACK", "OTEL_EXPORTER_OTLP_HEADERS"]);
  // As the app does it (`claude_settings_with_helper`): MonoCode's own
  // --settings stays, and a second, merged copy with the helper goes last.
  // Claude Code reads only the last --settings.
  const merged = JSON.stringify({ env: { CLAUDE_CODE_ENABLE_TODO_TOOLS: "1" }, otelHeadersHelper: helper });
  const session = new ClaudeSession({ claudePath: launcher, dirs, baseUrl: fake.url ?? fake.baseUrl, args: ["--settings", merged] });
  let bashSawToken = null;
  try {
    const { events } = await session.send("Run the check command.", 120000);
    // Does the agent's own shell see a header variable? It must not.
    const out = JSON.stringify(events);
    bashSawToken = out.includes(token);
    await waitUntil(() => logRecords(receiver.requests).some((r) => r.name.endsWith("tool_result")), 15000);
  } finally {
    await session.close();
    await waitUntil(() => false, 1500);
    await fake.close?.();
    await receiver.close();
  }
  const records = logRecords(receiver.requests);
  const result = verdict("Claude Code (headers helper)", receiver.requests, records, (r) => r.name.endsWith("tool_result") && r.attrs.tool_name === "Bash");
  if (bashSawToken) {
    result.status = "fail";
    result.finding += " The token appeared in the session's output.";
  }
  if (!result.exports) {
    result.status = "fail";
    result.finding += ` otelHeadersHelper from --settings did not reach the receiver (${String(version ?? "").split(" ")[0]}).`;
  }
  return result;
}

async function codexExport({ codexPath }) {
  const token = randomBytes(32).toString("hex");
  const receiver = await startStandInReceiver(token);
  const dirs = makeTempDirs("otel-codex-");
  const fake = await startFakeResponses({
    script: (body, n) => {
      if (codexSide(body)) return [text("Telemetry check")];
      const hasOutput = (body.input ?? []).some((i) => i?.type === "function_call_output");
      return hasOutput ? [text("done")] : [call("exec_command", { cmd: "echo telemetry-check; env | grep -ci '^OTEL_EXPORTER_OTLP_[A-Z_]*HEADERS=' || true" }, "otel1")];
    },
  });
  const launcher = wrapper(dirs.root, codexPath, codexEnv(token), []);
  // OTEL_CHECK_NO_EXCLUDE=1 drops the excludes, to see the check fail
  // (0.161.0: "A shell command saw 2 header variable(s)").
  const client = new CodexClient({ codexPath: launcher, dirs, baseUrl: fake.url ?? fake.baseUrl, args: [...codexArgs(receiver.endpoint, false), ...(process.env.OTEL_CHECK_NO_EXCLUDE ? [] : codexShellEnvArgs())] });
  try {
    await client.initialize();
    const started = await client.request("thread/start", { cwd: dirs.work, approvalPolicy: "on-request", sandbox: "danger-full-access", approvalsReviewer: "user" });
    await client.runTurn({ threadId: started.thread.id, input: userInput("Run the check command."), approvalPolicy: "on-request", approvalsReviewer: "user", sandboxPolicy: { type: "dangerFullAccess" } }, 120000);
    await waitUntil(() => logRecords(receiver.requests).some((r) => r.name.endsWith("tool_result")), 15000);
  } finally {
    await client.close();
    await waitUntil(() => false, 1500);
    await fake.close?.();
    await receiver.close();
  }
  saveFixtures("codex", receiver.requests);
  const records = logRecords(receiver.requests);
  const result = verdict("Codex", receiver.requests, records, (r) => r.name.endsWith("tool_result"));
  // The command's output, as the model got it: how many header variables
  // its shell saw. Mac test 2.6: none.
  const outputs = (fake.requests ?? [])
    .flatMap((r) => r.body?.input ?? [])
    .filter((i) => i?.type === "function_call_output")
    .map((i) => (typeof i.output === "string" ? i.output : JSON.stringify(i.output)));
  const shellSawToken = outputs.some((o) => o.includes(token));
  const counted = outputs.join("\n").match(/telemetry-check\s*(?:\\n|\n)+\s*(\d+)/);
  result.seen.shellHeaderVars = counted ? Number(counted[1]) : null;
  if (shellSawToken || (counted && Number(counted[1]) > 0)) {
    result.status = "fail";
    result.finding += ` A shell command saw ${counted ? counted[1] : "the"} header variable(s).`;
  } else if (counted) {
    result.finding += " Shell commands saw no header variable.";
  } else {
    result.finding += " Could not read the command's output to check its environment.";
  }
  if (!result.exports) result.finding += " The npm build may lack the `otel` feature, or app-server mode does not export.";
  return result;
}

export const otelCodexChecks = [
  { id: "otel-codex", title: "Codex exports log events to the receiver with the chat's token", flag: "harness telemetry (A3)", run: codexExport },
];

export const otelClaudeChecks = [
  { id: "otel-claude", title: "Claude Code exports log events to the receiver with the chat's token", flag: "harness telemetry (A2)", run: claudeExport },
  { id: "otel-claude-helper", title: "Claude Code sends the token from otelHeadersHelper in --settings, with no header variable", flag: "harness telemetry (Mac 2.6)", run: claudeHelperExport },
];
