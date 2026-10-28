#!/usr/bin/env node
// Cross-lane checks for wave 1 (integration w1). Each lane tested its client
// against a fake; this starts lane B's real daemon and drives the other
// lanes' clients into it:
//
//   - lane D's `tk run` starting a real Pi with lane A's package (run.start,
//     Pi's OTLP records, run.stop, all in one chat);
//   - lane F's Claude Code hook (approve through a real approver, the
//     adapter record, and no fallback to the daemon's own token);
//   - lane C's `tk connect --write` logging `config.write`;
//   - lane C's `tk connect` reading lane E's catalog entries;
//   - a chat token revoked at `run.stop`.
//
//   node tests/integration/w1.mjs --pi /path/to/pi [--bin target/release] [--json]
//
// Everything runs under a fresh temp folder (HOME, XDG_RUNTIME_DIR,
// XDG_DATA_HOME) on 127.0.0.1 and a Unix socket, and is removed at the end.
// Needs Node 22.18 or later (the hook is TypeScript run by type stripping)
// and `script` from util-linux for the one `--write`.

import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : dflt;
};
const PI = opt("--pi") ? resolve(opt("--pi")) : undefined;
const BIN = resolve(ROOT, opt("--bin", "target/release"));
const JSON_OUT = args.includes("--json");
const TK = join(BIN, "tk");
const DAEMON = join(BIN, "tk-daemon");
for (const b of [TK, DAEMON]) {
  if (!existsSync(b)) {
    console.error(`missing ${b}: run cargo build --release first, or pass --bin`);
    process.exit(2);
  }
}

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok: Boolean(ok), detail });
  if (!JSON_OUT) console.log(`${ok ? "pass" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
}
function info(name, detail) {
  results.push({ name, ok: true, info: true, detail });
  if (!JSON_OUT) console.log(`info  ${name}  (${detail})`);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, ms, step = 50) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = fn();
    if (v) return v;
    await sleep(step);
  }
  return fn();
}

// ------------------------------------------------------------ the sandbox

const top = mkdtempSync(join(tmpdir(), "tk-int-w1-"));
const HOME = join(top, "home");
const RUN = join(top, "run");
const DATA = join(top, "data");
const WORK = join(HOME, "work");
for (const d of [HOME, RUN, DATA, WORK]) mkdirSync(d, { recursive: true, mode: 0o700 });
chmodSync(RUN, 0o700);
const SOCK_DIR = join(RUN, "tk");
const SOCK = join(SOCK_DIR, "daemon.sock");
const baseEnv = { PATH: process.env.PATH, HOME, XDG_RUNTIME_DIR: RUN, XDG_DATA_HOME: DATA, LANG: "C.UTF-8" };

const daemon = spawn(DAEMON, ["--foreground"], { env: baseEnv, stdio: ["ignore", "ignore", "pipe"] });
let daemonErr = "";
daemon.stderr.on("data", (d) => (daemonErr += d));

function cleanup() {
  try {
    daemon.kill("SIGTERM");
  } catch {}
  rmSync(top, { recursive: true, force: true });
}

// ------------------------------------------------------------ socket client

class Conn {
  constructor(onPush) {
    this.lines = [];
    this.waiters = [];
    this.onPush = onPush;
    this.sock = net.createConnection({ path: SOCK });
    let buf = "";
    this.sock.on("data", (c) => {
      buf += c;
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        if (!line.trim()) continue;
        const msg = JSON.parse(line);
        if (msg.event || msg.approve) {
          this.onPush?.(msg, this);
          continue;
        }
        const w = this.waiters.shift();
        if (w) w(msg);
        else this.lines.push(msg);
      }
    });
    this.sock.on("error", () => {});
  }
  send(obj) {
    return new Promise((resolveReply) => {
      this.waiters.push(resolveReply);
      this.sock.write(`${JSON.stringify(obj)}\n`);
    });
  }
  close() {
    this.sock.destroy();
  }
}

async function hello(token, client, onPush) {
  const c = new Conn(onPush);
  await new Promise((r, j) => {
    c.sock.once("connect", r);
    c.sock.once("error", j);
  });
  const reply = await c.send({ op: "hello", token, client });
  return { c, reply };
}

// ------------------------------------------------------------ fake model

function sse(res, event, data) {
  res.write(`event: ${event}\ndata: ${JSON.stringify({ type: event, ...data })}\n\n`);
}
async function fakeModel(toolCall) {
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (c) => (raw += c));
    req.on("end", () => {
      if (req.method !== "POST" || !/\/v1\/messages(\?|$)/.test(req.url ?? "")) return res.writeHead(404).end();
      const body = JSON.parse(raw || "{}");
      const hasResult = (body.messages ?? []).some((m) => Array.isArray(m.content) && m.content.some((c) => c?.type === "tool_result"));
      const b = !hasResult ? { type: "tool_use", id: `toolu_${randomUUID().slice(0, 8)}`, name: toolCall.name, input: toolCall.input } : { type: "text", text: "done" };
      res.writeHead(200, { "content-type": "text/event-stream" });
      sse(res, "message_start", { message: { id: "msg_1", type: "message", role: "assistant", model: body.model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 1 } } });
      if (b.type === "text") {
        sse(res, "content_block_start", { index: 0, content_block: { type: "text", text: "" } });
        sse(res, "content_block_delta", { index: 0, delta: { type: "text_delta", text: b.text } });
      } else {
        sse(res, "content_block_start", { index: 0, content_block: { type: "tool_use", id: b.id, name: b.name, input: {} } });
        sse(res, "content_block_delta", { index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify(b.input) } });
      }
      sse(res, "content_block_stop", { index: 0 });
      sse(res, "message_delta", { delta: { stop_reason: b.type === "text" ? "end_turn" : "tool_use", stop_sequence: null }, usage: { output_tokens: 5 } });
      sse(res, "message_stop", {});
      res.end();
    });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return { port: server.address().port, close: () => new Promise((r) => (server.closeAllConnections(), server.close(() => r()))) };
}

// Async on purpose: the approver and the fake model live in this process,
// and spawnSync would freeze them while the child waits on them.
function runCmd(cmd, argv, env, input) {
  return new Promise((resolveRun) => {
    const child = spawn(cmd, argv, { env, cwd: WORK, stdio: ["pipe", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    const timer = setTimeout(() => child.kill("SIGKILL"), 90_000);
    child.on("close", (code, signal) => {
      clearTimeout(timer);
      resolveRun({ code, signal, out, err });
    });
    child.stdin.end(input ?? "");
  });
}

// ------------------------------------------------------------ main

let model;
try {
  const up = await until(() => existsSync(SOCK) && existsSync(join(SOCK_DIR, "otlp.endpoint")) && existsSync(join(SOCK_DIR, "daemon.token")), 10_000);
  check("daemon starts and writes its socket, token and endpoint", up, up ? SOCK_DIR : daemonErr.slice(0, 300));
  if (!up) throw new Error("daemon did not start");
  const DTOKEN = readFileSync(join(SOCK_DIR, "daemon.token"), "utf8").trim();

  // A live tail and an approver, both daemon scope.
  const events = [];
  const tail = await hello(DTOKEN, "int tail", (m) => m.event && events.push(m.event));
  check("daemon-scope hello", tail.reply.ok === true && tail.reply.scope === "daemon", JSON.stringify(tail.reply));
  check("tail opens", (await tail.c.send({ op: "tail" })).ok === true);
  const asked = [];
  const appr = await hello(DTOKEN, "int approver", (m, conn) => {
    if (!m.approve) return;
    asked.push(m.approve);
    conn.sock.write(`${JSON.stringify({ op: "answer", id: m.approve.id, answer: "yes" })}\n`);
  });
  check("approver registers", (await appr.c.send({ op: "approver" })).ok === true);

  const policyDir = join(HOME, ".tk");
  mkdirSync(policyDir, { recursive: true, mode: 0o700 });
  const POLICY = join(policyDir, "policy.json");
  writeFileSync(POLICY, JSON.stringify({ version: 1, askTimeoutSeconds: 5, rules: [{ tool: "bash", action: "ask" }, { tool: "read", action: "allow" }] }), { mode: 0o600 });

  // ---- 1. tk run (lane D) + real Pi + tk-pi (lane A) -> daemon (lane B)
  if (!PI) {
    info("tk run with Pi", "not run: no --pi given");
  } else {
    model = await fakeModel({ name: "bash", input: { command: "echo tk-int-ok" } });
    const agent = join(HOME, ".pi", "agent");
    mkdirSync(agent, { recursive: true });
    writeFileSync(
      join(agent, "models.json"),
      JSON.stringify({ providers: { fake: { baseUrl: `http://127.0.0.1:${model.port}`, api: "anthropic-messages", apiKey: "fake-key", models: [{ id: "fake-model", name: "fake-model", contextWindow: 200000, maxTokens: 4096, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] } } }),
    );
    // Pi's own ask has no UI in JSON mode, so bash is allowed for this run.
    writeFileSync(POLICY, JSON.stringify({ version: 1, rules: [{ tool: "bash", action: "allow" }] }), { mode: 0o600 });
    const env = { ...baseEnv, PATH: `${dirname(PI)}${delimiter}${process.env.PATH}`, PI_CODING_AGENT_DIR: agent, PI_OFFLINE: "1", PI_TELEMETRY: "0" };
    const piArgs = ["--offline", "-ne", "-ns", "-np", "-nc", "--no-session", "--no-mcp", "-e", join(ROOT, "packages", "tk-pi"), "--model", "fake/fake-model", "--mode", "json", "run the step"];
    const chat = "int-pi-1";
    const r = await runCmd(TK, ["run", "pi", "--boundary", "none", "--chat", chat, "--cwd", WORK, "--", ...piArgs], env);
    check("tk run pi --boundary none exits 0", r.code === 0, `code ${r.code}; ${r.err.split("\n").filter(Boolean).slice(-2).join(" | ")}`);
    await until(() => events.some((e) => e.chatId === chat && e.kind === "run.stop"), 5000);
    const mine = events.filter((e) => e.chatId === chat);
    const start = mine.find((e) => e.source === "run" && e.kind === "run.start");
    const stop = mine.find((e) => e.source === "run" && e.kind === "run.stop");
    check("run.start reaches the daemon with runsAs you", start && start.detail?.runsAs === "you", start ? JSON.stringify(start.detail).slice(0, 160) : "none");
    check("run.stop reaches the daemon with the exit code", stop && stop.detail?.exitCode === 0, stop ? JSON.stringify(stop.detail) : "none");
    await until(() => mine.length < 0 || events.some((e) => e.chatId === chat && e.source === "otel" && /tool_result/.test(e.kind + JSON.stringify(e.detail))), 4000);
    const otel = events.filter((e) => e.chatId === chat && e.source === "otel");
    const kinds = [...new Set(otel.map((e) => e.kind))].sort();
    check("Pi's OTLP records land in the run's chat, harness pi", otel.length > 0 && otel.every((e) => e.harness === "pi"), `${otel.length} records, kinds ${kinds.join(",")}`);
    const call = otel.find((e) => e.kind === "tool.call");
    check("the Pi tool call is one record joined by tool_use_id", Boolean(call), call ? `${call.id} ${call.summary ?? ""}`.slice(0, 120) : `kinds ${kinds.join(",")}`);
    await model.close();
    model = undefined;
  }

  // A chat token for the hook checks, as `tk run` would mint it.
  const mint = await tail.c.send({ op: "token.mint", chat: "int-cc-1", harness: "claude-code" });
  check("token.mint for a claude-code chat", mint.ok === true && /^[0-9a-f]{64}$/.test(mint.token ?? ""));
  writeFileSync(POLICY, JSON.stringify({ version: 1, askTimeoutSeconds: 5, rules: [{ tool: "bash", action: "ask" }, { tool: "read", action: "allow" }] }), { mode: 0o600 });

  // ---- 2. lane F's Claude Code hook -> real daemon
  const hookBin = join(ROOT, "packages", "tk-claude-plugin", "bin", "tk-hook.mjs");
  const hookEnv = (extra) => ({ ...baseEnv, TK_POLICY: POLICY, ...extra });
  const pre = (id) => JSON.stringify({ hook_event_name: "PreToolUse", session_id: "s-int", tool_name: "Bash", tool_input: { command: "ls" }, tool_use_id: id, permission_mode: "default", cwd: WORK });
  const reason = (out) => {
    try {
      return JSON.parse(out).hookSpecificOutput?.permissionDecisionReason ?? "";
    } catch {
      return out.slice(0, 120);
    }
  };
  const decision = (out) => {
    try {
      const o = JSON.parse(out).hookSpecificOutput;
      return o ? `${o.permissionDecision}` : undefined;
    } catch {
      return undefined;
    }
  };
  const before = asked.length;
  let h = await runCmd(process.execPath, ["--no-warnings", hookBin, "pre"], hookEnv({ TK_CHAT_TOKEN: mint.token, TK_CHAT_ID: "int-cc-1" }), pre("toolu_int1"));
  check("hook ask with a chat token reaches the real approver and allows", decision(h.out) === "allow" && asked.length === before + 1 && asked.at(-1)?.chat === "int-cc-1", `decision ${decision(h.out)}; asked ${asked.length - before}; chat ${asked.at(-1)?.chat}; ${reason(h.out)} ${h.err.trim().slice(0, 120)}`);
  h = await runCmd(process.execPath, ["--no-warnings", hookBin, "post"], hookEnv({ TK_CHAT_TOKEN: mint.token, TK_CHAT_ID: "int-cc-1" }), JSON.stringify({ hook_event_name: "PostToolUse", session_id: "s-int", tool_name: "Bash", tool_use_id: "toolu_int1", tool_input: { command: "ls" }, tool_response: { stdout: "a", stderr: "", interrupted: false, isImage: false } }));
  const rec = await until(() => events.find((e) => e.chatId === "int-cc-1" && e.source === "adapter" && e.kind === "tool.call"), 3000);
  check("hook's adapter record lands in the token's chat", Boolean(rec), rec ? `${rec.id} by ${rec.harness}` : `post exit ${h.code}`);

  // No chat token: the hook must not borrow the daemon's own token (rules
  // section 5, item 7: an adapter is not the daemon). Claude Code's own
  // prompt answers instead, and no approver is asked.
  const before2 = asked.length;
  h = await runCmd(process.execPath, ["--no-warnings", hookBin, "pre"], hookEnv({}), pre("toolu_int2"));
  check("hook without a chat token does not use the daemon's token", asked.length === before2 && decision(h.out) === "ask", `decision ${decision(h.out)}; approver asked ${asked.length - before2} time(s); ${reason(h.out)}`);

  // A chat token in a file that `tk run` wrote (TK_CHAT_TOKEN_FILE), for
  // harnesses that strip secret-named variables (rules case 76).
  const tokDir = join(top, "chat-token");
  mkdirSync(tokDir, { mode: 0o700 });
  writeFileSync(join(tokDir, "token"), `${mint.token}\n`, { mode: 0o600 });
  const before3 = asked.length;
  h = await runCmd(process.execPath, ["--no-warnings", hookBin, "pre"], hookEnv({ TK_CHAT_TOKEN_FILE: join(tokDir, "token"), TK_CHAT_ID: "int-cc-1" }), pre("toolu_int3"));
  check("hook reads the chat token from TK_CHAT_TOKEN_FILE", decision(h.out) === "allow" && asked.length === before3 + 1, `decision ${decision(h.out)}; asked ${asked.length - before3}; ${reason(h.out)}`);

  // ---- 3. lane C's tk connect --write -> daemon log
  const entry = join(top, "entry.json");
  writeFileSync(entry, readFileSync(join(ROOT, "contracts", "fixtures", "entry", "server.json"), "utf8"));
  const cmd = `${TK} connect pi --from ${entry} --write`;
  const connectEnv = PI ? { ...baseEnv, PATH: `${dirname(PI)}${delimiter}${process.env.PATH}` } : { ...baseEnv };
  const w = await runCmd("script", ["-qec", cmd, "/dev/null"], connectEnv, "y\n");
  const wrote = existsSync(join(HOME, ".pi", "agent", "mcp.json"));
  const cw = await until(() => events.find((e) => e.kind === "config.write"), 3000);
  check("tk connect --write in a terminal writes and logs config.write to the daemon", wrote && Boolean(cw), `wrote ${wrote}; event ${cw ? `${cw.source}/${cw.kind}` : "none"}; exit ${w.code}; ${w.out.split("\n").filter(Boolean).slice(-1)[0] ?? ""}`.slice(0, 220));
  check("no fallback ~/.tk/connect.log while the daemon runs", !existsSync(join(HOME, ".tk", "connect.log")));

  // ---- 4. lane E's catalog through lane C's tk connect (dry run)
  const catalog = join(ROOT, "library", "catalog");
  const outcomes = [];
  for (const id of readdirSync(catalog).sort()) {
    const f = join(catalog, id, "server.json");
    if (!existsSync(f)) continue;
    for (const harness of ["pi", "claude-code", "codex"]) {
      const r = await runCmd(TK, ["connect", harness, "--from", f], { ...baseEnv });
      outcomes.push({ id, harness, code: r.code, why: (r.err.split("\n").find((l) => l.trim()) ?? "").slice(0, 140) });
    }
  }
  // `registryType: "host"` is tk's own label for a local command (upstream's
  // registryType is a free string); tk connect writes only npm and pypi
  // packages in wave 1 and refuses the rest. The gateway is connected with
  // `tk connect --gateway docker`, computer use waits for wave 2's `tk cu`.
  const HOST_ONLY = new Set(["docker-mcp-gateway", "tk-computer-use"]);
  const refused = outcomes.filter((o) => o.code !== 0 && !HOST_ONLY.has(o.id));
  const hostRight = outcomes.filter((o) => HOST_ONLY.has(o.id)).every((o) => o.code !== 0 && /registry type Some\("host"\)/.test(o.why));
  check("every catalog server dry-runs through tk connect for pi, claude-code and codex", refused.length === 0, refused.length ? refused.map((o) => `${o.id}/${o.harness}: ${o.why}`).join(" ; ") : `${outcomes.filter((o) => o.code === 0).length} dry runs`);
  check("the two host-command entries are refused by name, not by accident", hostRight, [...HOST_ONLY].join(", "));
  // The variable a header reads is the one the export names (lane E) and tk
  // connect writes (lane C): one key, set once, serves both.
  const exa = await runCmd(TK, ["connect", "claude-code", "--from", join(catalog, "exa", "server.json")], { ...baseEnv });
  check("tk connect reads exa's key from EXA_API_KEY, as the export does", /EXA_API_KEY/.test(exa.out) && !/exa_api_key/.test(exa.out), exa.out.split("\n").find((l) => /api.key/i.test(l))?.trim());

  // ---- 5. chat token revoked at run.stop (lane D's request 2)
  const fakeBin = join(top, "fakebin");
  mkdirSync(fakeBin);
  const dump = join(top, "dump.txt");
  writeFileSync(join(fakeBin, "pi"), `#!/bin/sh\nprintf '%s' "$TK_CHAT_TOKEN" > '${dump}'\nexit 0\n`, { mode: 0o755 });
  const r5 = await runCmd(TK, ["run", "pi", "--boundary", "none", "--chat", "int-revoke", "--cwd", WORK], { ...baseEnv, PATH: `${fakeBin}${delimiter}${process.env.PATH}` });
  const leaked = existsSync(dump) ? readFileSync(dump, "utf8").trim() : "";
  await until(() => events.some((e) => e.chatId === "int-revoke" && e.kind === "run.stop"), 3000);
  if (!/^[0-9a-f]{64}$/.test(leaked)) {
    check("chat token revoked at run.stop", false, `fake harness saw no token (exit ${r5.code}; ${r5.err.trim().slice(0, 120)})`);
  } else {
    const late = await hello(leaked, "int late");
    late.c.close();
    check("chat token revoked at run.stop", late.reply.ok !== true, JSON.stringify(late.reply));
  }

  tail.c.close();
  appr.c.close();
} catch (e) {
  check("script error", false, String(e?.stack ?? e).slice(0, 400));
} finally {
  if (model) await model.close();
  cleanup();
}

const failed = results.filter((r) => !r.ok);
if (JSON_OUT) console.log(JSON.stringify({ results }, null, 2));
else console.log(`\n${results.filter((r) => !r.info).length - failed.length} of ${results.filter((r) => !r.info).length} passed`);
process.exit(failed.length ? 1 : 0);
