import { createServer, request as httpRequest, type IncomingMessage, type ServerResponse } from "node:http";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { connect } from "node:net";
import { GuardError, guardedRequest, isBlockedAddress, pinnedLookup, refuseUrl } from "./netguard.ts";
import { DISCOVER_BODY, classify, combine, probe, rpcIn } from "./liveness.ts";

// The guarded request dials over TLS to a public address. The tests swap in
// a request function that sends the same method, path, headers and body to a
// local HTTP server, and a resolver that answers with a public address, so
// the response handling runs for real while nothing leaves the machine.
type Handler = (req: IncomingMessage, body: string, res: ServerResponse) => void;
let handler: Handler = (_q, _b, res) => res.end();
let port = 0;
const seen: { method?: string; headers: Record<string, unknown>; body: string; pinned?: unknown }[] = [];
const server = createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    seen.push({ ...(req.method ? { method: req.method } : {}), headers: req.headers, body });
    handler(req, body, res);
  });
});
beforeAll(() => new Promise<void>((r) => server.listen(0, "127.0.0.1", () => ((port = (server.address() as { port: number }).port), r()))));
afterAll(() => new Promise<void>((r) => server.close(() => r())));

const publicLookup = (_h: string, cb: (e: null, a: { address: string; family: number }[]) => void) => cb(null, [{ address: "93.184.215.14", family: 4 }]);
let lastPinned: unknown;
const toLocal = ((o: Record<string, any>, cb: (res: IncomingMessage) => void) => {
  // Records the address the guard pinned the connection to.
  o["lookup"]("x", { all: true }, (_e: unknown, addrs: unknown) => {
    lastPinned = addrs;
  });
  return httpRequest({ host: "127.0.0.1", port, path: o["path"], method: o["method"], headers: o["headers"] }, cb);
}) as never;
const local = { lookup: publicLookup, requestImpl: toLocal };

describe("netguard", () => {
  it("blocks loopback, private, link-local, CGNAT, multicast and the IPv4-embedding IPv6 prefixes", () => {
    for (const a of ["127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "224.0.0.1", "255.255.255.255", "::1", "::", "fc00::1", "fe80::1", "::ffff:10.0.0.1", "64:ff9b::a00:1", "2002:a00:1::1", "ff02::1", "not-an-ip"])
      expect(isBlockedAddress(a), a).toBe(true);
    for (const a of ["93.184.215.14", "1.1.1.1", "2606:4700::1111"]) expect(isBlockedAddress(a), a).toBe(false);
  });

  it("refuses non-https, IP literals, single labels, local names, credentials and templated URLs", () => {
    expect(refuseUrl("http://a.example.com/")).toBe("not https");
    expect(refuseUrl("https://127.0.0.1/")).toBe("an IP literal");
    expect(refuseUrl("https://[::1]/")).toBe("an IP literal");
    expect(refuseUrl("https://intranet/")).toBe("a single-label host");
    expect(refuseUrl("https://printer.local/")).toBe("a local name");
    expect(refuseUrl("https://u:p@a.example.com/")).toBe("credentials in the URL");
    expect(refuseUrl("https://{tenant}.example.com/mcp")).toBe("a templated URL");
    expect(refuseUrl("https://a.example.com/mcp")).toBeUndefined();
  });

  it("dials nothing when any address the name resolves to is private (DNS rebinding with two records)", async () => {
    const mixed = (_h: string, cb: (e: null, a: { address: string; family: number }[]) => void) => cb(null, [{ address: "93.184.215.14", family: 4 }, { address: "127.0.0.1", family: 4 }]);
    const before = seen.length;
    await expect(guardedRequest("https://a.example.com/", { method: "GET", lookup: mixed, requestImpl: toLocal })).rejects.toMatchObject({ reason: "private-address" });
    expect(seen.length).toBe(before);
  });

  it("pins the connection to the address it checked", async () => {
    handler = (_q, _b, res) => res.end("ok");
    await guardedRequest("https://a.example.com/", { method: "GET", ...local });
    expect(lastPinned).toEqual([{ address: "93.184.215.14", family: 4 }]);
  });

  // Box test w2, BW8: https://0523.tw/mcp/public resolved to an IPv6 address
  // the box could not reach and an IPv4 one that answered in 0.6 s; the guard
  // pinned the first and the server was recorded dead. Every address was
  // checked, so every one may be dialled, in Node's Happy Eyeballs order
  // (Node 22 lib/net.js:1387-1403; doc/api/net.md: "connect to all obtained
  // IPv6 and IPv4 addresses, in sequence").
  it("offers every checked address of a dual-stack name to the dial, with family autoselection on", async () => {
    handler = (_q, _b, res) => res.end("ok");
    const dual = (_h: string, cb: (e: null, a: { address: string; family: number }[]) => void) =>
      cb(null, [{ address: "2606:4700::1111", family: 6 }, { address: "93.184.215.14", family: 4 }]);
    let opts: Record<string, unknown> = {};
    const capture = ((o: Record<string, any>, cb: (res: IncomingMessage) => void) => ((opts = o), (toLocal as any)(o, cb))) as never;
    await guardedRequest("https://a.example.com/", { method: "GET", lookup: dual, requestImpl: capture });
    expect(opts["autoSelectFamily"]).toBe(true);
    expect(lastPinned).toEqual([{ address: "2606:4700::1111", family: 6 }, { address: "93.184.215.14", family: 4 }]);
  });

  it("a dial falls through an address that does not answer to the next one (a real socket)", async () => {
    // A listener on 127.0.0.1 only; the first address offered is IPv6, which
    // refuses or has no route here. The guard never offers these two (both
    // are blocked); the lookup is tested alone.
    const lookup = pinnedLookup([{ address: "::1", family: 6 }, { address: "127.0.0.1", family: 4 }]);
    const ok = await new Promise<boolean>((resolve) => {
      const s = connect({ host: "dual.example.com", port, lookup: lookup as never, autoSelectFamily: true, autoSelectFamilyAttemptTimeout: 100 });
      s.once("connect", () => (s.destroy(), resolve(true)));
      s.once("error", () => resolve(false));
    });
    expect(ok).toBe(true);
  });

  it("returns a redirect, never follows it", async () => {
    handler = (_q, _b, res) => (res.writeHead(302, { location: "http://169.254.169.254/latest/meta-data/" }), res.end());
    const before = seen.length;
    const r = await guardedRequest("https://a.example.com/", { method: "GET", ...local });
    expect(r.status).toBe(302);
    expect(seen.length).toBe(before + 1);
  });

  it("caps the body and times out", async () => {
    handler = (_q, _b, res) => res.end("x".repeat(10_000));
    const r = await guardedRequest("https://a.example.com/", { method: "GET", maxBytes: 100, ...local });
    expect(r.truncated).toBe(true);
    expect(r.body.length).toBe(100);
    handler = () => {}; // never answers
    await expect(guardedRequest("https://a.example.com/", { method: "GET", timeoutMs: 100, ...local })).rejects.toMatchObject({ reason: "timeout" });
  });
});

describe("liveness", () => {
  it("sends server/discover with the headers the 2026-07-28 binding requires and no credential", async () => {
    handler = (_q, _b, res) => (res.writeHead(200, { "content-type": "application/json" }), res.end(JSON.stringify({ jsonrpc: "2.0", id: "tk-liveness", result: { resultType: "complete", supportedVersions: ["2026-07-28"], capabilities: {} } })));
    const d = await probe("https://a.example.com/mcp", local);
    expect(d).toMatchObject({ state: "alive", era: "modern", reason: "discover result" });
    const post = seen.at(-1)!;
    expect(post.method).toBe("POST");
    expect(post.body).toBe(DISCOVER_BODY);
    expect(post.headers["mcp-protocol-version"]).toBe("2026-07-28");
    expect(post.headers["mcp-method"]).toBe("server/discover");
    expect(String(post.headers["accept"])).toMatch(/application\/json.*text\/event-stream/);
    expect(post.headers["authorization"]).toBeUndefined();
    expect(JSON.parse(post.body).params._meta["io.modelcontextprotocol/protocolVersion"]).toBe(post.headers["mcp-protocol-version"]);
  });

  it("reads a result from an SSE body", async () => {
    handler = (_q, _b, res) => (res.writeHead(200, { "content-type": "text/event-stream" }), res.end('event: message\ndata: {"jsonrpc":"2.0","id":"tk-liveness","result":{}}\n\n'));
    expect((await probe("https://a.example.com/mcp", local)).state).toBe("alive");
  });

  it("calls a dead tunnel dead (corner case 58)", async () => {
    handler = (_q, _b, res) => (res.writeHead(404, { "content-type": "text/html" }), res.end("<html>Tunnel abc.ngrok.app not found ERR_NGROK_3200</html>"));
    expect((await probe("https://abc.ngrok.app/mcp", local)).state).toBe("dead");
  });

  it("never probes a templated or private remote, and says why", async () => {
    expect(await probe("https://{tenant}.example.com/mcp")).toMatchObject({ state: "unknown", reason: "not probed: a templated URL" });
    const priv = (_h: string, cb: (e: null, a: { address: string; family: number }[]) => void) => cb(null, [{ address: "192.168.0.9", family: 4 }]);
    expect((await probe("https://a.example.com/mcp", { lookup: priv, requestImpl: toLocal })).state).toBe("unknown");
  });

  const res = (status: number, body = "", headers: Record<string, string> = {}) => ({ status, body, headers, truncated: false });
  it.each([
    [res(405), res(400, JSON.stringify({ jsonrpc: "2.0", id: 1, error: { code: -32022, message: "Unsupported protocol version", data: { supported: ["2025-11-25"] } } })), "alive", "modern"],
    [res(405), res(404, JSON.stringify({ jsonrpc: "2.0", id: 1, error: { code: -32601, message: "Method not found" } })), "alive", "legacy"],
    [res(405), res(400, "Bad Request: missing session"), "alive", "legacy"],
    [res(405), res(401, "", { "www-authenticate": 'Bearer resource_metadata="https://a/.well-known/oauth-protected-resource"' }), "alive", undefined],
    [res(503), res(503, "upstream down"), "dead", undefined],
    [res(530), res(530, "error code: 1033"), "dead", undefined],
    [res(404, "<html>nope</html>"), res(404, "<html>nope</html>"), "dead", undefined],
    [res(405), new GuardError("timeout", "no answer"), "alive", undefined],
    [new GuardError("dns", "name did not resolve (ENOTFOUND)"), new GuardError("dns", "name did not resolve (ENOTFOUND)"), "dead", undefined],
  ] as const)("classifies HEAD %j and discover %j as %s", (head, discover, state, era) => {
    const c = classify(head as never, discover as never);
    expect(c.state).toBe(state);
    expect(c.era).toBe(era);
  });

  it("combines a server's remotes", () => {
    const d = (state: "alive" | "dead" | "unknown") => ({ url: "u", state, checkedAt: "t", reason: "r" });
    expect(combine([d("dead"), d("alive")])).toBe("alive");
    expect(combine([d("dead"), d("dead")])).toBe("dead");
    expect(combine([d("dead"), d("unknown")])).toBe("unknown");
    expect(combine([])).toBe("unknown");
    expect(rpcIn("not json")).toBeUndefined();
  });
});
