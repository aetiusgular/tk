// A fake Gemini API server for contract tests (integration w1, for wave 2
// lane I's Gemini checks). Loopback only.
//
// Gemini CLI reaches it with API-key auth and GOOGLE_GEMINI_BASE_URL, which
// the CLI allows over plain HTTP for 127.0.0.1 (its configuration reference
// at 0.65.0). Run against Gemini CLI 0.65.0 on 27 Oct 2026, it saw:
//   POST /v1beta/models/<model>:generateContent       side calls (the model
//        router, and other utility roles) asking for JSON by a schema
//   POST /v1beta/models/<model>:streamGenerateContent?alt=sse   the turns
//   POST /v1beta/models/<model>:countTokens           answered with a count
// A side call with a JSON schema (`generationConfig.responseJsonSchema` or
// `responseSchema`) is answered with an object built from that schema; every
// streamed turn is answered from `script(body, n, requests)`, which returns
// parts: `text("hi")` or `functionCall("run_shell_command", {command})`.
//
// The CLI needs, besides the URL: GEMINI_API_KEY (any value),
// `security.auth.selectedType = "gemini-api-key"` in ~/.gemini/settings.json,
// and GEMINI_CLI_TRUST_WORKSPACE=true (or --skip-trust) for a headless run.

import http from "node:http";

export function text(value) {
  return { text: value };
}

export function functionCall(name, args) {
  return { functionCall: { name, args } };
}

/** The text of the last user turn that holds text. */
export function lastUserText(body) {
  const users = (body?.contents ?? []).filter((c) => c.role === "user" && (c.parts ?? []).some((p) => typeof p.text === "string"));
  return (users.at(-1)?.parts ?? []).map((p) => p.text ?? "").join("\n");
}

/** Every functionResponse part in the request, oldest first. */
export function functionResponses(body) {
  const out = [];
  for (const c of body?.contents ?? []) for (const p of c.parts ?? []) if (p?.functionResponse) out.push(p.functionResponse);
  return out;
}

/** The function names the request offers the model. */
export function toolNames(body) {
  return (body?.tools ?? []).flatMap((t) => (t.functionDeclarations ?? []).map((f) => f.name));
}

/** A value that satisfies a JSON schema (types, enums, required properties). */
export function fromSchema(schema, depth = 0) {
  if (!schema || typeof schema !== "object" || depth > 8) return null;
  if (Array.isArray(schema.enum) && schema.enum.length) return schema.enum[0];
  const type = Array.isArray(schema.type) ? schema.type[0] : schema.type;
  switch (String(type ?? "").toLowerCase()) {
    case "object": {
      const out = {};
      for (const [k, v] of Object.entries(schema.properties ?? {})) out[k] = fromSchema(v, depth + 1);
      return out;
    }
    case "array":
      return [];
    case "number":
    case "integer":
      return typeof schema.minimum === "number" ? schema.minimum : 10;
    case "boolean":
      return false;
    case "string":
      return "fake";
    default:
      return schema.properties ? fromSchema({ ...schema, type: "object" }, depth) : null;
  }
}

const usage = { promptTokenCount: 10, candidatesTokenCount: 2, totalTokenCount: 12 };

export async function startFakeGemini({ script } = {}) {
  const requests = [];
  let turns = 0;
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
      const model = /models\/([^:/?]+)/.exec(url)?.[1] ?? "";
      const json = (code, obj) => {
        res.writeHead(code, { "content-type": "application/json" });
        res.end(JSON.stringify(obj));
      };
      if (req.method === "POST" && /:countTokens(\?|$)/.test(url)) {
        requests.push({ at: Date.now(), url, model, body, kind: "countTokens" });
        return json(200, { totalTokens: 10 });
      }
      if (req.method === "POST" && /:generateContent(\?|$)/.test(url)) {
        const schema = body?.generationConfig?.responseJsonSchema ?? body?.generationConfig?.responseSchema;
        const reply = schema ? JSON.stringify(fromSchema(schema)) : "ok";
        requests.push({ at: Date.now(), url, model, body, kind: "side", reply });
        return json(200, { candidates: [{ content: { role: "model", parts: [{ text: reply }] }, finishReason: "STOP", index: 0 }], usageMetadata: usage, modelVersion: model });
      }
      if (req.method === "POST" && /:streamGenerateContent(\?|$)/.test(url)) {
        const entry = { at: Date.now(), url, model, headers: req.headers, body, kind: "turn" };
        requests.push(entry);
        const n = turns++;
        let parts;
        try {
          parts = script ? script(body, n, requests) : [text("ok")];
        } catch (error) {
          parts = [text(`script error: ${error?.message ?? error}`)];
        }
        entry.reply = parts;
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
        res.write(`data: ${JSON.stringify({ candidates: [{ content: { role: "model", parts }, finishReason: "STOP", index: 0 }], usageMetadata: usage, modelVersion: model })}\r\n\r\n`);
        res.end();
        return;
      }
      requests.push({ at: Date.now(), url, method: req.method, kind: "other" });
      json(404, { error: { code: 404, message: `no route ${req.method} ${url}`, status: "NOT_FOUND" } });
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  return {
    port,
    baseUrl: `http://127.0.0.1:${port}`,
    requests,
    turnRequests: () => requests.filter((r) => r.kind === "turn"),
    close: () => new Promise((resolve) => (server.closeAllConnections(), server.close(() => resolve()))),
  };
}
