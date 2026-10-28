#!/usr/bin/env node
// A stdio MCP server that replays a recorded tools/list (tools/<server>.json)
// and answers tools/call from the current task in tasks.json. No network, no
// keys. Every call is appended to the call log, so scoring does not depend on
// what a harness reports.
//
//   node stub-server.mjs --server <name> --tools <recorded.json> \
//        --tasks <tasks.json> --state <dir>
//
// <dir>/current holds the running task's id and run id ("t09 run-17");
// <dir>/calls.jsonl gets one line per call.
import { appendFileSync, readFileSync } from "node:fs";
import { join } from "node:path";

const arg = (k) => {
  const i = process.argv.indexOf(`--${k}`);
  return i > 0 ? process.argv[i + 1] : undefined;
};
const server = arg("server");
const recorded = JSON.parse(readFileSync(arg("tools"), "utf8"));
const tasks = JSON.parse(readFileSync(arg("tasks"), "utf8")).tasks;
const state = arg("state");

function current() {
  try {
    const [task, run] = readFileSync(join(state, "current"), "utf8").trim().split(/\s+/);
    return { task, run };
  } catch {
    return { task: null, run: null };
  }
}

function matches(rule, args) {
  if (!rule) return true;
  for (const [k, want] of Object.entries(rule)) {
    const have = k === "*" ? JSON.stringify(args ?? {}) : String(args?.[k] ?? "");
    if (!have.toLowerCase().includes(String(want).toLowerCase())) return false;
  }
  return true;
}

function answer(tool, args) {
  const { task, run } = current();
  const t = tasks.find((x) => x.id === task);
  const r = t?.responses?.[`${server}.${tool}`];
  let text, hit;
  if (r && matches(r.match, args)) {
    text = r.text;
    hit = true;
  } else if (r) {
    text = "No results for that input.";
    hit = false;
  } else {
    text = "No results.";
    hit = false;
  }
  appendFileSync(
    join(state, "calls.jsonl"),
    JSON.stringify({ at: new Date().toISOString(), run, task, server, tool, args, hit }) + "\n",
  );
  return text;
}

const send = (m) => process.stdout.write(JSON.stringify(m) + "\n");
let buf = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i).trim();
    buf = buf.slice(i + 1);
    if (!line) continue;
    let m;
    try {
      m = JSON.parse(line);
    } catch {
      continue;
    }
    if (m.id == null) continue; // notifications
    const ok = (result) => send({ jsonrpc: "2.0", id: m.id, result });
    switch (m.method) {
      case "initialize":
        ok({
          protocolVersion: m.params?.protocolVersion || recorded.protocolVersion || "2025-06-18",
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: `${recorded.serverInfo?.name || server} (tk-measure stub)`, version: recorded.serverInfo?.version || "0" },
          ...(recorded.instructions ? { instructions: recorded.instructions } : {}),
        });
        break;
      case "ping":
        ok({});
        break;
      case "tools/list":
        ok({ tools: recorded.tools });
        break;
      case "tools/call":
        ok({ content: [{ type: "text", text: answer(m.params?.name, m.params?.arguments) }], isError: false });
        break;
      case "resources/list":
        ok({ resources: [] });
        break;
      case "resources/templates/list":
        ok({ resourceTemplates: [] });
        break;
      case "prompts/list":
        ok({ prompts: [] });
        break;
      default:
        send({ jsonrpc: "2.0", id: m.id, error: { code: -32601, message: `method not found: ${m.method}` } });
    }
  }
});
process.stdin.on("end", () => process.exit(0));
