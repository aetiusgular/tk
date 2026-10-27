#!/usr/bin/env node
// A stdio MCP server with two tools, for contract tests.
//   fetch_url {url}   read-only in name, returns a fixed text
//   write_note {text} a write tool
// It logs every call to the file named by STUB_MCP_LOG, when set.
// STUB_MCP_INIT_DELAY_MS delays the `initialize` reply, for startup checks.
import { appendFileSync } from "node:fs";

const log = process.env.STUB_MCP_LOG || process.argv[2] || "";
const initDelay = Number(process.env.STUB_MCP_INIT_DELAY_MS) || 0;
const tools = [
  {
    name: "fetch_url",
    description: "Fetch a web page and return its text.",
    inputSchema: {
      type: "object",
      properties: { url: { type: "string", description: "The URL to fetch." } },
      required: ["url"],
    },
  },
  {
    name: "write_note",
    description: "Save a note.",
    inputSchema: {
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    },
  },
];

function send(obj) {
  process.stdout.write(`${JSON.stringify(obj)}\n`);
}

let buf = "";
process.stdin.on("data", (chunk) => {
  buf += chunk.toString();
  let nl;
  while ((nl = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, nl).trim();
    buf = buf.slice(nl + 1);
    if (!line) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      continue;
    }
    if (log) appendFileSync(log, `${line}\n`);
    if (msg.id === undefined) continue;
    switch (msg.method) {
      case "initialize":
        setTimeout(send, initDelay, {
          jsonrpc: "2.0",
          id: msg.id,
          result: {
            protocolVersion: msg.params?.protocolVersion ?? "2025-06-18",
            capabilities: { tools: {} },
            serverInfo: { name: "stub", version: "1.0.0" },
          },
        });
        break;
      case "tools/list":
        send({ jsonrpc: "2.0", id: msg.id, result: { tools } });
        break;
      case "tools/call":
        send({
          jsonrpc: "2.0",
          id: msg.id,
          result: {
            content: [{ type: "text", text: `stub ${msg.params?.name} ok: ${JSON.stringify(msg.params?.arguments ?? {})}` }],
          },
        });
        break;
      default:
        send({ jsonrpc: "2.0", id: msg.id, result: {} });
    }
  }
});
