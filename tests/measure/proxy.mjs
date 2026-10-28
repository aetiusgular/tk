#!/usr/bin/env node
// A loopback pass-through between a harness and the model server, recording
// what each model request carried: how many tools were declared, their bytes,
// deferred flags, betas, and the usage the server reported. Bodies are not
// stored (they hold the prompt); only counts, names and sizes.
//
//   node proxy.mjs --listen 8091 --upstream http://127.0.0.1:8080 --state <dir>
//
// Lines go to <dir>/requests.jsonl, tagged with the run id in <dir>/current.
import { createServer } from "node:http";
import { appendFileSync, readFileSync } from "node:fs";
import { join } from "node:path";

const arg = (k, d) => {
  const i = process.argv.indexOf(`--${k}`);
  return i > 0 ? process.argv[i + 1] : d;
};
const listen = Number(arg("listen", "8091"));
const upstream = new URL(arg("upstream", "http://127.0.0.1:8080"));
const state = arg("state");

function run() {
  try {
    return readFileSync(join(state, "current"), "utf8").trim().split(/\s+/)[1] || null;
  } catch {
    return null;
  }
}

function describe(path, body) {
  let j;
  try {
    j = JSON.parse(body);
  } catch {
    return { parse: "no" };
  }
  const tools = Array.isArray(j.tools) ? j.tools : [];
  const names = tools.map((t) => t.name || t.function?.name || t.type || "?");
  const deferred = tools.filter((t) => t.defer_loading === true).map((t) => t.name);
  const sys = typeof j.system === "string" ? j.system : JSON.stringify(j.system ?? "");
  const msgs = Array.isArray(j.messages) ? j.messages : [];
  const sysInMsgs = msgs.filter((m) => m.role === "system").map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content))).join("");
  let toolRefs = 0;
  const s = JSON.stringify(msgs);
  toolRefs = (s.match(/"tool_reference"/g) || []).length;
  return {
    model: j.model,
    stream: !!j.stream,
    messages: msgs.length,
    tools: tools.length,
    toolNames: names,
    toolBytes: JSON.stringify(tools).length,
    deferred: deferred.length,
    deferredNames: deferred,
    loadedNames: names.filter((n) => !deferred.includes(n)),
    systemBytes: sys.length + sysInMsgs.length,
    toolReferenceBlocks: toolRefs,
    thinking: j.thinking ?? j.reasoning_effort ?? null,
    maxTokens: j.max_tokens ?? j.max_completion_tokens ?? null,
  };
}

function usageFrom(text) {
  // Non-streamed JSON, or the last usage-bearing event of an SSE stream.
  const found = {};
  const take = (o) => {
    if (!o || typeof o !== "object") return;
    const u = o.usage || o.message?.usage;
    if (u) Object.assign(found, u);
    if (o.timings) found.timings = o.timings;
  };
  try {
    take(JSON.parse(text));
  } catch {
    for (const line of text.split(/\r?\n/)) {
      if (!line.startsWith("data:")) continue;
      try {
        take(JSON.parse(line.slice(5).trim()));
      } catch {}
    }
  }
  return found;
}

createServer((req, res) => {
  const chunks = [];
  req.on("data", (c) => chunks.push(c));
  req.on("end", async () => {
    const body = Buffer.concat(chunks);
    const started = Date.now();
    const rec = { at: new Date().toISOString(), run: run(), method: req.method, path: req.url, bytes: body.length, beta: req.headers["anthropic-beta"] || null };
    if (req.method === "POST") Object.assign(rec, describe(req.url, body.toString("utf8")));
    const headers = { ...req.headers, host: upstream.host };
    delete headers["content-length"];
    delete headers["accept-encoding"];
    let up;
    try {
      up = await fetch(new URL(req.url, upstream), { method: req.method, headers, body: req.method === "GET" || req.method === "HEAD" ? undefined : body, duplex: "half" });
    } catch (e) {
      rec.status = 502;
      rec.error = String(e);
      appendFileSync(join(state, "requests.jsonl"), JSON.stringify(rec) + "\n");
      res.writeHead(502, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: `tk-measure proxy: ${e}` } }));
      return;
    }
    const outHeaders = {};
    up.headers.forEach((v, k) => {
      if (!["content-length", "content-encoding", "transfer-encoding", "connection"].includes(k)) outHeaders[k] = v;
    });
    res.writeHead(up.status, outHeaders);
    let text = "";
    const reader = up.body?.getReader();
    if (reader) {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        res.write(value);
        text += Buffer.from(value).toString("utf8");
      }
    }
    res.end();
    rec.status = up.status;
    rec.ms = Date.now() - started;
    rec.usage = usageFrom(text);
    if (up.status >= 400) rec.errorBody = text.slice(0, 600);
    appendFileSync(join(state, "requests.jsonl"), JSON.stringify(rec) + "\n");
  });
}).listen(listen, "127.0.0.1", () => console.log(`tk-measure proxy on 127.0.0.1:${listen} -> ${upstream.href}`));
