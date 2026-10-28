#!/usr/bin/env node
// Live check: tk-pi loaded into a real Pi, against a fake model and a fake
// OTLP receiver. Lane A ran it in the cloud on Pi 1.4.0; the box test runs it
// on the box's Pi. Not a vitest file: it needs a Pi binary.
//
//   node packages/tk-pi/live/pi-live.mjs --pi /path/to/pi [--json]
//
// What it starts, all on 127.0.0.1 and all stopped at the end: a fake
// Anthropic Messages server (the SSE shape of the fork's
// tests/cli-contract/lib/fakeMessages.mjs, written again here so this file
// depends on no other lane), a fake OTLP receiver, and Pi itself, with HOME,
// PI_CODING_AGENT_DIR and the working folder in a fresh temp folder and
// PI_OFFLINE=1. The only commands the fake model asks Pi to run are `echo`
// lines and one `export` that the gate must refuse. Nothing is written
// outside the temp folder, which is removed at the end.

import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, chmodSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
const piIndex = args.indexOf("--pi");
if (piIndex < 0 || !args[piIndex + 1]) {
  console.error("usage: pi-live.mjs --pi <path to pi> [--json]");
  process.exit(2);
}
const PI = resolve(args[piIndex + 1]);
const JSON_OUT = args.includes("--json");
const PACKAGE = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TOKEN = "c".repeat(64);
const GH_TOKEN = `ghp_${"A1b2C3d4E5".repeat(3)}abcdef`;

// ------------------------------------------------------------ fake model

function sse(res, event, data) {
  res.write(`event: ${event}\ndata: ${JSON.stringify({ type: event, ...data })}\n\n`);
}

function toolResultsIn(body) {
  const out = [];
  for (const m of body?.messages ?? []) {
    for (const c of Array.isArray(m.content) ? m.content : []) {
      if (c?.type === "tool_result") out.push(typeof c.content === "string" ? c.content : JSON.stringify(c.content));
    }
  }
  return out;
}

async function fakeModel(toolCall) {
  const seen = [];
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (c) => (raw += c));
    req.on("end", () => {
      if (req.method !== "POST" || !/\/v1\/messages(\?|$)/.test(req.url ?? "")) {
        res.writeHead(404).end();
        return;
      }
      const body = JSON.parse(raw || "{}");
      const results = toolResultsIn(body);
      seen.push({ results });
      const blocks = results.length === 0 && toolCall ? [{ type: "tool_use", id: `toolu_${randomUUID().slice(0, 8)}`, name: toolCall.name, input: toolCall.input }] : [{ type: "text", text: "done" }];
      const stop = blocks[0].type === "tool_use" ? "tool_use" : "end_turn";
      res.writeHead(200, { "content-type": "text/event-stream" });
      sse(res, "message_start", { message: { id: "msg_1", type: "message", role: "assistant", model: body.model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 100, output_tokens: 1 } } });
      blocks.forEach((b, index) => {
        if (b.type === "text") {
          sse(res, "content_block_start", { index, content_block: { type: "text", text: "" } });
          sse(res, "content_block_delta", { index, delta: { type: "text_delta", text: b.text } });
        } else {
          sse(res, "content_block_start", { index, content_block: { type: "tool_use", id: b.id, name: b.name, input: {} } });
          sse(res, "content_block_delta", { index, delta: { type: "input_json_delta", partial_json: JSON.stringify(b.input) } });
        }
        sse(res, "content_block_stop", { index });
      });
      sse(res, "message_delta", { delta: { stop_reason: stop, stop_sequence: null }, usage: { output_tokens: 10 } });
      sse(res, "message_stop", {});
      res.end();
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return { port: server.address().port, seen, close: () =>
      new Promise((r) => {
        server.closeAllConnections();
        server.close(() => r());
      }) };
}

async function fakeReceiver() {
  const posts = [];
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (c) => (raw += c));
    req.on("end", () => {
      let body = null;
      try {
        body = JSON.parse(raw);
      } catch {}
      posts.push({ url: req.url, token: req.headers["x-tk-token"], body });
      res.writeHead(200, { "content-type": "application/json" }).end("{}");
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return { port: server.address().port, posts, close: () =>
      new Promise((r) => {
        server.closeAllConnections();
        server.close(() => r());
      }) };
}

// ------------------------------------------------------------ one Pi run

function home(policy, modelPort) {
  const dir = mkdtempSync(join(tmpdir(), "tk-pi-live-"));
  const agent = join(dir, "agent");
  mkdirSync(agent, { recursive: true });
  mkdirSync(join(dir, ".tk"), { mode: 0o700 });
  mkdirSync(join(dir, "work"));
  if (policy) {
    writeFileSync(join(dir, ".tk", "policy.json"), JSON.stringify(policy));
    chmodSync(join(dir, ".tk", "policy.json"), 0o600);
  }
  writeFileSync(
    join(agent, "models.json"),
    JSON.stringify({
      providers: {
        fake: {
          baseUrl: `http://127.0.0.1:${modelPort}`,
          api: "anthropic-messages",
          apiKey: "fake-key",
          models: [{ id: "fake-model", name: "fake-model", contextWindow: 200000, maxTokens: 4096, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
        },
      },
    }),
  );
  return dir;
}

function runPi(dir, mode, extraEnv, onLine) {
  const argv = ["--offline", "-ne", "-ns", "-np", "-nc", "--no-session", "--no-mcp", "-e", PACKAGE, "--model", "fake/fake-model"];
  if (mode === "json") argv.push("--mode", "json", "run the step");
  else argv.push("--mode", "rpc");
  const child = spawn(PI, argv, {
    cwd: join(dir, "work"),
    env: { PATH: process.env.PATH, HOME: dir, PI_CODING_AGENT_DIR: join(dir, "agent"), PI_OFFLINE: "1", PI_TELEMETRY: "0", ...extraEnv },
    stdio: ["pipe", "pipe", "pipe"],
  });
  // JSON mode reads piped stdin as part of the prompt (Pi docs/cli.md:26-30),
  // so it is closed at once; RPC mode keeps it for commands.
  if (mode === "json") child.stdin.end();
  let buf = "";
  let stderr = "";
  child.stdout.on("data", (d) => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i);
      buf = buf.slice(i + 1);
      let ev;
      try {
        ev = JSON.parse(line);
      } catch {
        continue;
      }
      onLine?.(ev, child);
    }
  });
  child.stderr.on("data", (d) => (stderr += d));
  const done = new Promise((resolveDone) => {
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      resolveDone({ code: "timeout", stderr });
    }, 60_000);
    child.on("exit", (code) => {
      clearTimeout(timer);
      resolveDone({ code, stderr });
    });
  });
  return { child, done };
}

const POLICY = {
  version: 1,
  askTimeoutSeconds: 2,
  rules: [
    { tool: "bash", action: "allow" },
    { tool: "write", action: "ask" },
  ],
};

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok: Boolean(ok), detail });
  if (!JSON_OUT) console.log(`${ok ? "pass" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
}

async function jsonScenario(name, toolCall, expect, opts = {}) {
  const model = await fakeModel(toolCall);
  const receiver = await fakeReceiver();
  const dir = home(opts.policy === undefined ? POLICY : opts.policy, model.port);
  try {
    const env = opts.noTelemetry ? {} : { TK_CHAT_TOKEN: TOKEN, TK_OTLP_ENDPOINT: `http://127.0.0.1:${receiver.port}` };
    const { done } = runPi(dir, "json", env);
    const exit = await done;
    await new Promise((r) => setTimeout(r, 300));
    const toolResult = model.seen.flatMap((s) => s.results).join("\n");
    expect({ toolResult, exit, posts: receiver.posts, dir });
  } catch (err) {
    check(name, false, String(err));
  } finally {
    await model.close();
    await receiver.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

async function rpcScenario(name, toolCall, answer, expect) {
  const model = await fakeModel(toolCall);
  const dir = home(POLICY, model.port);
  const requests = [];
  try {
    const { child, done } = runPi(dir, "rpc", {}, (ev, c) => {
      if (ev.type === "extension_ui_request" && ev.method === "confirm") {
        requests.push(ev);
        if (answer !== "never") c.stdin.write(`${JSON.stringify({ type: "extension_ui_response", id: ev.id, ...answer })}\n`);
      }
      if (ev.type === "agent_settled") c.stdin.end();
    });
    child.stdin.write(`${JSON.stringify({ id: "p1", type: "prompt", message: "run the step" })}\n`);
    const exit = await done;
    const toolResult = model.seen.flatMap((s) => s.results).join("\n");
    expect({ toolResult, exit, requests });
  } catch (err) {
    check(name, false, String(err));
  } finally {
    await model.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

// ------------------------------------------------------------ scenarios

await jsonScenario("allowed bash runs", { name: "bash", input: { command: "echo tk-live-ok" } }, ({ toolResult }) =>
  check("allowed bash runs", toolResult.includes("tk-live-ok") && !toolResult.includes("Blocked by tk"), toolResult.slice(0, 120)),
);

await jsonScenario("reject list blocks bash", { name: "bash", input: { command: "export ANTHROPIC_BASE_URL=http://203.0.113.9" } }, ({ toolResult }) =>
  check("reject list blocks bash", toolResult.includes("Blocked by tk: reject list: anthropic_base_url"), toolResult.slice(0, 160)),
);

await jsonScenario("look-alike spelling is blocked", { name: "bash", input: { command: "export АNTHROPIC_BASE_URL=x" } }, ({ toolResult }) =>
  check("look-alike spelling is blocked (case 21)", toolResult.includes("Blocked by tk: reject list"), toolResult.slice(0, 160)),
);

await jsonScenario("ask with no UI refuses", { name: "write", input: { path: "a.txt", content: "x" } }, ({ toolResult, dir }) =>
  check("ask in json mode refuses at once (case 63)", toolResult.includes("no UI to ask in"), toolResult.slice(0, 160)),
);

await jsonScenario("no policy file asks, then refuses", { name: "bash", input: { command: "echo nope" } }, ({ toolResult }) =>
  check("no policy file: ask, refused without UI", toolResult.includes("no rule (no policy file)"), toolResult.slice(0, 160)),
  { policy: null },
);

await jsonScenario("tool result is redacted", { name: "bash", input: { command: `echo GITHUB_TOKEN=${GH_TOKEN}` } }, ({ toolResult }) =>
  check("tool result is redacted before the model", toolResult.includes("[REDACTED by tk]") && !toolResult.includes(GH_TOKEN), toolResult.slice(0, 160)),
);

await jsonScenario("emitter posts pi.* records", { name: "bash", input: { command: "echo tel" } }, ({ posts }) => {
  const recs = posts.flatMap((p) => p.body?.resourceLogs?.[0]?.scopeLogs?.[0]?.logRecords ?? []);
  const names = recs.map((r) => r.body?.stringValue);
  const tokenOk = posts.length > 0 && posts.every((p) => p.token === TOKEN && p.url === "/v1/logs");
  const decision = recs.find((r) => r.body?.stringValue === "pi.tool_decision");
  const result = recs.find((r) => r.body?.stringValue === "pi.tool_result");
  const id = (r) => r?.attributes?.find((a) => a.key === "tool_use_id")?.value?.stringValue;
  check("emitter posts to the loopback receiver with x-tk-token", tokenOk, `${posts.length} posts`);
  check("emitter sends user_prompt, tool_decision, tool_result, api_request", ["pi.user_prompt", "pi.tool_decision", "pi.tool_result", "pi.api_request"].every((n) => names.includes(n)), names.join(","));
  check("decision and result join by tool_use_id", id(decision) !== undefined && id(decision) === id(result), `${id(decision)} / ${id(result)}`);
});

await jsonScenario("no token, no post", { name: "bash", input: { command: "echo quiet" } }, ({ posts, toolResult }) =>
  check("no TK_CHAT_TOKEN: nothing is posted, the call still runs", posts.length === 0 && toolResult.includes("quiet"), `${posts.length} posts`),
  { noTelemetry: true },
);

await rpcScenario("rpc: ask answered yes", { name: "write", input: { path: "a.txt", content: "hello" } }, { confirmed: true }, ({ toolResult, requests }) =>
  check("rpc: ask answered yes runs the call", requests.length === 1 && requests[0].title === "tk: allow write?" && !toolResult.includes("Blocked by tk"), `${requests.length} dialogs; ${toolResult.slice(0, 100)}`),
);

await rpcScenario("rpc: ask answered no", { name: "write", input: { path: "a.txt", content: "hello" } }, { confirmed: false }, ({ toolResult }) =>
  check("rpc: ask answered no blocks", toolResult.includes("Blocked by tk: not approved"), toolResult.slice(0, 120)),
);

await rpcScenario("rpc: ask cancelled", { name: "write", input: { path: "a.txt", content: "hello" } }, { cancelled: true }, ({ toolResult }) =>
  check("rpc: a cancelled dialog blocks", toolResult.includes("Blocked by tk: not approved"), toolResult.slice(0, 120)),
);

await rpcScenario("rpc: ask never answered", { name: "write", input: { path: "a.txt", content: "hello" } }, "never", ({ toolResult, requests }) =>
  check("rpc: no answer times out to deny", toolResult.includes("Blocked by tk: ask timed out") && requests[0]?.timeout === 2000, `${toolResult.slice(0, 100)}; timeout ${requests[0]?.timeout}`),
);

const failed = results.filter((r) => !r.ok).length;
if (JSON_OUT) console.log(JSON.stringify({ pi: PI, results, failed }, null, 2));
else console.log(`\n${results.length - failed} of ${results.length} passed`);
process.exit(failed === 0 ? 0 : 1);
