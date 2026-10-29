#!/usr/bin/env node
// Records a remote MCP server's tools/list over streamable HTTP, or a stdio
// server's, into tests/measure/tools/<name>.json, so the stub server can
// replay real definitions without keys or network during the measurement.
//
//   node record-tools.mjs http <name> <url> [Header: value ...]
//   node record-tools.mjs stdio <name> -- <command> [args...]
//
// Writes only the tools array plus where and when it came from. No secret is
// written: headers given on the command line are not recorded.
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const [mode, name, ...rest] = process.argv.slice(2);
if (!mode || !name) {
  console.error("usage: record-tools.mjs http <name> <url> [H: v ...] | stdio <name> -- <cmd> [args]");
  process.exit(2);
}

const PROTOCOL = "2025-06-18";
const init = {
  jsonrpc: "2.0",
  id: 1,
  method: "initialize",
  params: { protocolVersion: PROTOCOL, capabilities: {}, clientInfo: { name: "tk-measure-recorder", version: "0.0.0" } },
};

function parseBody(text, contentType) {
  if ((contentType || "").includes("text/event-stream")) {
    const out = [];
    for (const block of text.split(/\r?\n\r?\n/)) {
      const data = block
        .split(/\r?\n/)
        .filter((l) => l.startsWith("data:"))
        .map((l) => l.slice(5).trimStart())
        .join("\n");
      if (data) {
        try {
          out.push(JSON.parse(data));
        } catch {}
      }
    }
    return out;
  }
  try {
    const j = JSON.parse(text);
    return Array.isArray(j) ? j : [j];
  } catch {
    return [];
  }
}

async function http(url, headerArgs) {
  const base = { "content-type": "application/json", accept: "application/json, text/event-stream" };
  for (const h of headerArgs) {
    const i = h.indexOf(":");
    if (i > 0) base[h.slice(0, i).trim()] = h.slice(i + 1).trim();
  }
  let session;
  async function post(msg) {
    const headers = { ...base };
    if (session) headers["mcp-session-id"] = session;
    if (msg.method !== "initialize") headers["mcp-protocol-version"] = PROTOCOL;
    const r = await fetch(url, { method: "POST", headers, body: JSON.stringify(msg) });
    session = r.headers.get("mcp-session-id") || session;
    const text = await r.text();
    return { status: r.status, msgs: parseBody(text, r.headers.get("content-type")), text };
  }
  const a = await post(init);
  if (a.status >= 400) throw new Error(`initialize: HTTP ${a.status}: ${a.text.slice(0, 300)}`);
  const info = a.msgs.find((m) => m.id === 1)?.result;
  await post({ jsonrpc: "2.0", method: "notifications/initialized" });
  const tools = [];
  let cursor;
  for (let id = 2; id < 50; id++) {
    const b = await post({ jsonrpc: "2.0", id, method: "tools/list", params: cursor ? { cursor } : {} });
    if (b.status >= 400) throw new Error(`tools/list: HTTP ${b.status}: ${b.text.slice(0, 300)}`);
    const res = b.msgs.find((m) => m.id === id);
    if (res?.error) throw new Error(`tools/list: ${JSON.stringify(res.error)}`);
    tools.push(...(res?.result?.tools || []));
    cursor = res?.result?.nextCursor;
    if (!cursor) break;
  }
  return { info, tools };
}

async function stdio(argv) {
  const child = spawn(argv[0], argv.slice(1), { stdio: ["pipe", "pipe", "inherit"] });
  let buf = "";
  const waiters = new Map();
  child.stdout.on("data", (d) => {
    buf += d;
    let i;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i);
      buf = buf.slice(i + 1);
      try {
        const m = JSON.parse(line);
        if (m.id != null && waiters.has(m.id)) waiters.get(m.id)(m);
      } catch {}
    }
  });
  const call = (msg) =>
    new Promise((res, rej) => {
      const t = setTimeout(() => rej(new Error(`timeout on ${msg.method}`)), 60000);
      waiters.set(msg.id, (m) => {
        clearTimeout(t);
        res(m);
      });
      child.stdin.write(JSON.stringify(msg) + "\n");
    });
  const a = await call(init);
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");
  const tools = [];
  let cursor;
  for (let id = 2; id < 50; id++) {
    const b = await call({ jsonrpc: "2.0", id, method: "tools/list", params: cursor ? { cursor } : {} });
    if (b.error) throw new Error(JSON.stringify(b.error));
    tools.push(...(b.result?.tools || []));
    cursor = b.result?.nextCursor;
    if (!cursor) break;
  }
  child.kill();
  return { info: a.result, tools };
}

let got, source;
if (mode === "http") {
  const [url, ...hs] = rest;
  got = await http(url, hs);
  source = { transport: "http", url };
} else if (mode === "stdio") {
  const argv = rest[0] === "--" ? rest.slice(1) : rest;
  got = await stdio(argv);
  source = { transport: "stdio", command: argv };
} else {
  console.error(`unknown mode ${mode}`);
  process.exit(2);
}

const out = {
  server: name,
  recordedAt: new Date().toISOString(),
  source,
  serverInfo: got.info?.serverInfo ?? null,
  protocolVersion: got.info?.protocolVersion ?? null,
  instructions: got.info?.instructions ?? null,
  tools: got.tools,
};
const file = join(here, "tools", `${name}.json`);
mkdirSync(dirname(file), { recursive: true });
writeFileSync(file, JSON.stringify(out, null, 2) + "\n");
console.log(`${name}: ${got.tools.length} tools, ${JSON.stringify(out).length} bytes -> ${file}`);
