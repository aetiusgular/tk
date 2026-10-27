// A fake Anthropic Messages API server for contract tests. Loopback only.
//
// Every POST to /v1/messages is saved and answered from a script: a function
// that gets the parsed request and the count of model requests so far, and
// returns content blocks. Replies stream as Server-Sent Events in the
// Messages API shape. /v1/messages/count_tokens returns a fixed count.
//
//   textBlock("hi")                        a text block
//   toolUse("Write", { file_path, content }) a tool_use block

import http from "node:http";
import { randomUUID } from "node:crypto";

export function textBlock(value) {
  return { type: "text", text: value };
}

export function toolUse(name, input, id = `toolu_${randomUUID().replace(/-/g, "").slice(0, 20)}`) {
  return { type: "tool_use", id, name, input };
}

/** The system prompt of a request as one string. */
export function systemText(body) {
  const s = body?.system;
  if (typeof s === "string") return s;
  if (Array.isArray(s)) return s.map((b) => b?.text ?? "").join("\n");
  return "";
}

/** Every text in the request's messages, joined. */
export function messagesText(body) {
  const parts = [];
  for (const m of body?.messages ?? []) {
    if (typeof m.content === "string") parts.push(m.content);
    for (const c of Array.isArray(m.content) ? m.content : []) {
      if (typeof c?.text === "string") parts.push(c.text);
      if (c?.type === "tool_result") {
        parts.push(typeof c.content === "string" ? c.content : JSON.stringify(c.content));
      }
    }
  }
  return parts.join("\n");
}

/** The last user message's text. */
export function lastUserText(body) {
  const users = (body?.messages ?? []).filter((m) => m.role === "user");
  const m = users.at(-1);
  if (!m) return "";
  if (typeof m.content === "string") return m.content;
  return (m.content ?? []).map((c) => c?.text ?? "").join("\n");
}

/**
 * Claude Code's own side calls: title or topic detection, summaries, and the
 * like. They use a small model, carry no tools, or ask for a summary.
 */
export function isSideRequest(body) {
  const tools = body?.tools ?? [];
  const sys = systemText(body);
  if (/summar|title|topic|Analyze if this message/i.test(sys) && tools.length === 0) return true;
  return false;
}

function sse(res, event, data) {
  res.write(`event: ${event}\ndata: ${JSON.stringify({ type: event, ...data })}\n\n`);
}

export async function startFakeMessages({ script } = {}) {
  const requests = [];
  let modelRequests = 0;
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (chunk) => (raw += chunk));
    req.on("end", () => {
      const url = req.url ?? "";
      let body = null;
      try {
        body = raw ? JSON.parse(raw) : null;
      } catch {
        body = { unparsed: raw.slice(0, 2000) };
      }
      if (req.method === "POST" && /\/v1\/messages\/count_tokens/.test(url)) {
        requests.push({ at: Date.now(), url, body, kind: "count_tokens" });
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ input_tokens: 1000 }));
        return;
      }
      if (req.method !== "POST" || !/\/v1\/messages(\?|$)/.test(url)) {
        requests.push({ at: Date.now(), url, method: req.method, kind: "other" });
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ type: "error", error: { type: "not_found_error", message: `no route ${req.method} ${url}` } }));
        return;
      }
      const entry = { at: Date.now(), url, headers: req.headers, body, kind: "messages", side: isSideRequest(body) };
      requests.push(entry);
      const n = modelRequests++;
      let blocks;
      try {
        blocks = script ? script(body, n, requests) : [textBlock("ok")];
      } catch (error) {
        blocks = [textBlock(`script error: ${error?.message ?? error}`)];
      }
      entry.reply = blocks;
      const id = `msg_${randomUUID().replace(/-/g, "").slice(0, 24)}`;
      const model = body?.model ?? "claude-fake";
      const stop = blocks.some((b) => b.type === "tool_use") ? "tool_use" : "end_turn";
      if (body?.stream === false) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ id, type: "message", role: "assistant", model, content: blocks, stop_reason: stop, stop_sequence: null, usage: { input_tokens: 100, output_tokens: 10 } }));
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
      sse(res, "message_start", {
        message: { id, type: "message", role: "assistant", model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 100, output_tokens: 1, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 } },
      });
      blocks.forEach((block, index) => {
        if (block.type === "text") {
          sse(res, "content_block_start", { index, content_block: { type: "text", text: "" } });
          sse(res, "content_block_delta", { index, delta: { type: "text_delta", text: block.text } });
        } else if (block.type === "tool_use") {
          sse(res, "content_block_start", { index, content_block: { type: "tool_use", id: block.id, name: block.name, input: {} } });
          sse(res, "content_block_delta", { index, delta: { type: "input_json_delta", partial_json: JSON.stringify(block.input) } });
        }
        sse(res, "content_block_stop", { index });
      });
      sse(res, "message_delta", { delta: { stop_reason: stop, stop_sequence: null }, usage: { output_tokens: 10 } });
      sse(res, "message_stop", {});
      res.end();
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  return {
    port,
    baseUrl: `http://127.0.0.1:${port}`,
    requests,
    /** Model requests for turns, without count_tokens or side calls. */
    turnRequests: () => requests.filter((r) => r.kind === "messages" && !r.side),
    close: () => new Promise((resolve) => server.close(() => resolve())),
  };
}
