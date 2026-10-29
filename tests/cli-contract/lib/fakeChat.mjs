// A fake OpenAI chat-completions server, loopback only. Written by lane I for
// its checks (Kilo, Hermes, oh-my-pi, OpenClaw) as checks/I/fakeChat.js;
// moved here by integration w2 for wave 4's lanes (OpenClaude through
// OPENAI_BASE_URL, Mistral Vibe, Crush, Cline, Kimi and others with an
// OpenAI-compatible route; note AQ section 4b). It answers POST <any>/chat/completions, streamed (SSE chunks, then
// [DONE]) when the request asks for a stream, else one JSON completion; and
// GET <any>/models with one model. A request that offers tools is a turn,
// answered from `script(body, n)`: `{ text }` or `{ tool: name, args }`. A
// request with no tools (a title, a summary) gets a short text.

import http from "node:http";

export function startFakeChat({ script, model = "fake-model" } = {}) {
  const requests = [];
  let turn = 0;
  const server = http.createServer((req, res) => {
    let raw = "";
    req.on("data", (d) => (raw += d));
    req.on("end", () => {
      const url = req.url ?? "";
      if (req.method === "GET" && /\/models\/?(\?|$)/.test(url)) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ object: "list", data: [{ id: model, object: "model", owned_by: "fake" }] }));
        return;
      }
      if (req.method !== "POST" || !/\/chat\/completions\/?(\?|$)/.test(url)) {
        requests.push({ kind: "other", url, method: req.method });
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ error: { message: "not found" } }));
        return;
      }
      let body = {};
      try {
        body = JSON.parse(raw);
      } catch {
        body = {};
      }
      const isTurn = Array.isArray(body.tools) && body.tools.length > 0;
      const reply = isTurn ? script(body, turn++) : { text: "tk" };
      requests.push({ kind: isTurn ? "turn" : "side", url, body });
      const id = `chatcmpl-${requests.length}`;
      const created = Math.floor(Date.now() / 1000);
      const message = reply.tool
        ? { role: "assistant", content: null, tool_calls: [{ id: `call_${requests.length}`, type: "function", function: { name: reply.tool, arguments: JSON.stringify(reply.args ?? {}) } }] }
        : { role: "assistant", content: reply.text ?? "done" };
      const finish = reply.tool ? "tool_calls" : "stop";
      if (body.stream) {
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
        const chunk = (delta, finishReason = null) => `data: ${JSON.stringify({ id, object: "chat.completion.chunk", created, model, choices: [{ index: 0, delta, finish_reason: finishReason }] })}\n\n`;
        let out = chunk({ role: "assistant", content: "" });
        if (reply.tool) out += chunk({ tool_calls: [{ index: 0, ...message.tool_calls[0] }] });
        else out += chunk({ content: message.content });
        out += chunk({}, finish);
        out += `data: ${JSON.stringify({ id, object: "chat.completion.chunk", created, model, choices: [], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } })}\n\n`;
        out += "data: [DONE]\n\n";
        res.end(out);
        return;
      }
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ id, object: "chat.completion", created, model, choices: [{ index: 0, message, finish_reason: finish }], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } }));
    });
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({
        baseUrl: `http://127.0.0.1:${port}/v1`,
        requests,
        turns: () => requests.filter((r) => r.kind === "turn"),
        close: () => new Promise((r) => server.close(() => r())),
      });
    });
  });
}

/** Every tool result message the requests carried, oldest first. */
export function toolResults(fake) {
  const out = [];
  for (const r of fake.turns()) for (const m of r.body.messages ?? []) if (m.role === "tool") out.push(typeof m.content === "string" ? m.content : JSON.stringify(m.content));
  return [...new Set(out)];
}
