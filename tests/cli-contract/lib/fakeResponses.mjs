// A fake OpenAI Responses API server for contract tests. Loopback only.
//
// Every POST to /v1/responses (or /responses) is saved, then answered from a
// script: a function that receives the parsed request and the number of
// model requests seen so far, and returns a list of output items. The server
// streams them as Server-Sent Events in the shape Codex reads.
//
// Output item helpers:
//   text("hi")                               an assistant message
//   call("exec_command", { cmd: "ls" })      a function call
//   custom("apply_patch", "*** Begin Patch") a freeform (custom) tool call
//   reasoning("I should edit it.")           a reasoning item, no text

import http from "node:http";
import { randomUUID } from "node:crypto";

export function text(value) {
  return {
    type: "message",
    role: "assistant",
    id: `msg_${randomUUID().slice(0, 8)}`,
    status: "completed",
    content: [{ type: "output_text", text: value, annotations: [] }],
  };
}

export function call(name, args, callId = `call_${randomUUID().slice(0, 8)}`) {
  return {
    type: "function_call",
    id: `fc_${randomUUID().slice(0, 8)}`,
    status: "completed",
    name,
    arguments: typeof args === "string" ? args : JSON.stringify(args),
    call_id: callId,
  };
}

export function custom(name, input, callId = `call_${randomUUID().slice(0, 8)}`) {
  return {
    type: "custom_tool_call",
    id: `ctc_${randomUUID().slice(0, 8)}`,
    status: "completed",
    name,
    input,
    call_id: callId,
  };
}

/**
 * A reasoning item and nothing else, as a local model's last step can be:
 * a summary and the raw reasoning text, no encrypted content (Mac 3.7).
 */
export function reasoning(value) {
  return {
    type: "reasoning",
    id: `rs_${randomUUID().slice(0, 8)}`,
    summary: [{ type: "summary_text", text: value }],
    content: [{ type: "reasoning_text", text: value }],
    encrypted_content: null,
  };
}

/** Text of every message in a request's `input`, joined, for searching. */
export function inputText(body) {
  const parts = [];
  if (typeof body?.instructions === "string") parts.push(body.instructions);
  for (const item of body?.input ?? []) {
    if (typeof item?.content === "string") parts.push(item.content);
    for (const c of Array.isArray(item?.content) ? item.content : []) {
      if (typeof c?.text === "string") parts.push(c.text);
    }
    if (typeof item?.output === "string") parts.push(item.output);
  }
  return parts.join("\n");
}

/** The function call outputs in a request, keyed by call id. */
export function callOutputs(body) {
  const out = {};
  for (const item of body?.input ?? []) {
    if (
      (item?.type === "function_call_output" ||
        item?.type === "custom_tool_call_output") &&
      item.call_id
    ) {
      out[item.call_id] =
        typeof item.output === "string" ? item.output : JSON.stringify(item.output);
    }
  }
  return out;
}

/** Is this request one of Codex's own side calls (title, compaction, memory)? */
export function isSideRequest(body) {
  const t = inputText(body);
  return /generate a (concise )?title|summari[sz]e the conversation|CONTEXT CHECKPOINT/i.test(
    t.slice(-4000),
  );
}

function sse(res, event, data) {
  res.write(`event: ${event}\ndata: ${JSON.stringify({ type: event, ...data })}\n\n`);
}

export async function startFakeResponses({ script, model = "fake-model", port: wantPort = 0, keep = 500 } = {}) {
  const requests = [];
  let modelRequests = 0;
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (chunk) => (raw += chunk));
    req.on("end", () => {
      const url = req.url ?? "";
      if (req.method === "GET" && /\/models/.test(url)) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ object: "list", data: [{ id: model, object: "model" }], models: [] }));
        return;
      }
      if (req.method !== "POST" || !/\/responses(\?|$)/.test(url)) {
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { message: `no route ${req.method} ${url}` } }));
        return;
      }
      let body = null;
      try {
        body = JSON.parse(raw);
      } catch {
        body = { unparsed: raw.slice(0, 2000) };
      }
      const entry = { at: Date.now(), url, headers: req.headers, body };
      requests.push(entry);
      if (requests.length > keep) requests.shift();
      const n = modelRequests++;
      let items;
      try {
        items = script ? script(body, n, requests) : [text("ok")];
      } catch (error) {
        items = [text(`script error: ${error?.message ?? error}`)];
      }
      entry.reply = items;
      const id = `resp_${randomUUID().slice(0, 12)}`;
      res.writeHead(200, {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        connection: "keep-alive",
      });
      sse(res, "response.created", { response: { id, object: "response", status: "in_progress", model } });
      items.forEach((item, index) => {
        sse(res, "response.output_item.added", { output_index: index, item: { ...item, status: "in_progress" } });
        if (item.type === "message") {
          const value = item.content[0].text;
          sse(res, "response.output_text.delta", { output_index: index, item_id: item.id, content_index: 0, delta: value });
        }
        sse(res, "response.output_item.done", { output_index: index, item });
      });
      sse(res, "response.completed", {
        response: {
          id,
          object: "response",
          status: "completed",
          model,
          output: items,
          usage: {
            input_tokens: 100,
            input_tokens_details: { cached_tokens: 0 },
            output_tokens: 10,
            output_tokens_details: { reasoning_tokens: 0 },
            total_tokens: 110,
          },
        },
      });
      res.end();
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(wantPort, "127.0.0.1", resolve);
  });
  const { port } = server.address();
  return {
    port,
    baseUrl: `http://127.0.0.1:${port}/v1`,
    requests,
    /** Requests that went to the model for a turn, without Codex's side calls. */
    turnRequests: () => requests.filter((r) => !isSideRequest(r.body)),
    close: () => new Promise((resolve) => server.close(() => resolve())),
  };
}
