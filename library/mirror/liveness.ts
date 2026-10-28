// Liveness probes for mirrored remotes (lane G, T3.2; corner case 58).
//
// Two requests per remote URL, through netguard (https only, public
// addresses only, no redirects followed, 10 s, 64 KB):
//   1. HEAD, which most MCP endpoints answer 405 or 404: it proves the host
//      answers, and is cheap;
//   2. POST `server/discover`, the request every 2026-07-28 server MUST
//      implement, with the headers the Streamable HTTP binding requires
//      (`Accept`, `MCP-Protocol-Version` equal to the body's version,
//      `Mcp-Method`). A modern server answers with a result or a JSON-RPC
//      error; a legacy one with a 4xx and no modern body.
// No credential is ever sent: a server that needs one answers 401, which
// proves it is there. Results use the contract's states (alive, dead,
// unknown); the detail goes in the mirror's own record.
import { GuardError, guardedRequest, refuseUrl, type GuardedRequest, type GuardedResponse } from "./netguard.ts";

export const PROTOCOL_VERSION = "2026-07-28";

export type ProbeDetail = {
  url: string;
  state: "alive" | "dead" | "unknown";
  checkedAt: string;
  /** Why, in a few words: "discover result", "401 with WWW-Authenticate", "dns", ... */
  reason: string;
  headStatus?: number;
  discoverStatus?: number;
  era?: "modern" | "legacy";
};

export const DISCOVER_BODY = JSON.stringify({
  jsonrpc: "2.0",
  id: "tk-liveness",
  method: "server/discover",
  params: {
    _meta: {
      "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
      "io.modelcontextprotocol/clientInfo": { name: "tk-library-mirror", version: "0" },
      "io.modelcontextprotocol/clientCapabilities": {},
    },
  },
});

export const DISCOVER_HEADERS: Record<string, string> = {
  accept: "application/json, text/event-stream",
  "content-type": "application/json",
  "mcp-protocol-version": PROTOCOL_VERSION,
  "mcp-method": "server/discover",
};

/** The JSON-RPC error codes that only a modern server sends (method not
 *  found is in every era, but with a 4xx on a POST it is a server speaking). */
const MODERN_CODES = new Set([-32020, -32021, -32022]);

/** A JSON-RPC response object in a JSON or SSE body, if there is one. */
export function rpcIn(body: string): { result?: unknown; error?: { code?: unknown } } | undefined {
  const tryParse = (t: string) => {
    try {
      const v = JSON.parse(t) as unknown;
      if (v && typeof v === "object" && !Array.isArray(v) && (v as Record<string, unknown>)["jsonrpc"] === "2.0") return v as { result?: unknown; error?: { code?: unknown } };
    } catch {
      // not JSON
    }
    return undefined;
  };
  const direct = tryParse(body.trim());
  if (direct) return direct;
  for (const line of body.split(/\r?\n/)) if (line.startsWith("data:")) {
    const v = tryParse(line.slice(5).trim());
    if (v) return v;
  }
  return undefined;
}

/** Tunnel and hosting error pages that mean the server behind them is gone
 *  (ngrok's ERR_NGROK_* pages, Cloudflare's 530 for a dead tunnel). */
const DEAD_PAGES = /ERR_NGROK_\d+|tunnel [^\n]{0,40}not found|no tunnel|error code: 1033|argo tunnel error/i;

/** Classify one URL's two answers. Exported for the tests. */
export function classify(head: GuardedResponse | GuardError, discover: GuardedResponse | GuardError): Omit<ProbeDetail, "url" | "checkedAt"> {
  const hs = head instanceof GuardError ? undefined : head.status;
  const ds = discover instanceof GuardError ? undefined : discover.status;
  const base = { ...(hs !== undefined ? { headStatus: hs } : {}), ...(ds !== undefined ? { discoverStatus: ds } : {}) };
  if (discover instanceof GuardError) {
    if (discover.reason === "refused" || discover.reason === "private-address") return { ...base, state: "unknown", reason: discover.message };
    // The host answered HEAD but not the POST in time (a stream held open, a slow cold start).
    if (hs !== undefined && hs < 500 && !(head instanceof GuardError) && !DEAD_PAGES.test(head.body)) return { ...base, state: "alive", reason: `HEAD ${hs}; discover ${discover.reason}` };
    return { ...base, state: "dead", reason: discover.reason };
  }
  const rpc = rpcIn(discover.body);
  if (discover.status >= 200 && discover.status < 300 && rpc && "result" in rpc) return { ...base, state: "alive", reason: "discover result", era: "modern" };
  if (rpc && rpc.error && typeof rpc.error.code === "number") {
    return { ...base, state: "alive", reason: `JSON-RPC error ${rpc.error.code}`, era: MODERN_CODES.has(rpc.error.code) ? "modern" : "legacy" };
  }
  if (discover.status === 401 || discover.status === 403) {
    if (DEAD_PAGES.test(discover.body)) return { ...base, state: "dead", reason: "a tunnel's error page" };
    return { ...base, state: "alive", reason: `${discover.status}${discover.headers["www-authenticate"] ? " with WWW-Authenticate" : ""}` };
  }
  if (discover.status >= 500 || DEAD_PAGES.test(discover.body)) return { ...base, state: "dead", reason: DEAD_PAGES.test(discover.body) ? "a tunnel's error page" : `HTTP ${discover.status}` };
  if (discover.status >= 300 && discover.status < 400) return { ...base, state: "alive", reason: `redirect ${discover.status}, not followed` };
  if (discover.status === 404 || discover.status === 410) return { ...base, state: "dead", reason: `HTTP ${discover.status} with no JSON-RPC body` };
  if (discover.status >= 400) return { ...base, state: "alive", reason: `HTTP ${discover.status}, no modern body`, era: "legacy" };
  return { ...base, state: "alive", reason: `HTTP ${discover.status}` };
}

export type ProbeOptions = Pick<GuardedRequest, "lookup" | "requestImpl" | "timeoutMs"> & { now?: () => Date };

function iso(d: Date): string {
  return d.toISOString().replace(/\.\d{3}Z$/, "Z");
}

export async function probe(url: string, o: ProbeOptions = {}): Promise<ProbeDetail> {
  const checkedAt = iso((o.now ?? (() => new Date()))());
  const refused = refuseUrl(url);
  if (refused) return { url, state: "unknown", checkedAt, reason: `not probed: ${refused}` };
  const common = { ...(o.lookup ? { lookup: o.lookup } : {}), ...(o.requestImpl ? { requestImpl: o.requestImpl } : {}), timeoutMs: o.timeoutMs ?? 10_000, maxBytes: 64 * 1024 };
  const head = await guardedRequest(url, { ...common, method: "HEAD" }).catch((e: unknown) => (e instanceof GuardError ? e : new GuardError("network", String(e))));
  // A refusal on the first request is final: the second would be refused too.
  if (head instanceof GuardError && (head.reason === "refused" || head.reason === "private-address")) return { url, state: "unknown", checkedAt, reason: head.message };
  const discover = await guardedRequest(url, { ...common, method: "POST", headers: DISCOVER_HEADERS, body: DISCOVER_BODY }).catch((e: unknown) =>
    e instanceof GuardError ? e : new GuardError("network", String(e)),
  );
  return { url, checkedAt, ...classify(head, discover) };
}

/** The state of a server from its remotes: alive when any URL is, dead when
 *  every probed URL is, unknown otherwise. */
export function combine(details: ProbeDetail[]): "alive" | "dead" | "unknown" {
  if (details.some((d) => d.state === "alive")) return "alive";
  if (details.length > 0 && details.every((d) => d.state === "dead")) return "dead";
  return "unknown";
}

/** Run `tasks` with at most `limit` at once. */
export async function pool<T, R>(items: T[], limit: number, task: (t: T) => Promise<R>): Promise<R[]> {
  const out: R[] = new Array(items.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(limit, items.length) }, async () => {
    for (;;) {
      const i = next++;
      if (i >= items.length) return;
      out[i] = await task(items[i]!);
    }
  });
  await Promise.all(workers);
  return out;
}
