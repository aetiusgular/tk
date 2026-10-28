// A reader for the official MCP registry's list API (lane G, T3.2).
//
// `GET /v0.1/servers` with `cursor`, `limit` (1 to 100), `updated_since` and
// `include_deleted` (`internal/api/handlers/v0/servers.go:73-77` at 9c4a96c;
// the live registry ran 1.8.6, `5bfb002`, on 28 Oct 2026). One record per
// server version, ordered by name then version; `metadata.nextCursor` is
// opaque and must be followed with the same filters (`openapi.yaml:24-37`).
// With `updated_since`, deleted servers are always returned (`servers.go:43-50`).
//
// Pace: the bulk-list ingress allows 600 requests a minute per source IP per
// replica, other routes 180 (`deploy/pkg/k8s/registry.go:330-336, 403-407`).
// The default here is one request a second, a tenth of that; a 429 or 5xx is
// retried after `Retry-After` or a doubling backoff.
import { isRecord } from "../validate/types.ts";

export const OFFICIAL_REGISTRY = "https://registry.modelcontextprotocol.io";
export const OFFICIAL_KEY = "io.modelcontextprotocol.registry/official";
export const PAGE_LIMIT = 100;

/** The registry metadata of one version (openapi.yaml:1025-1060). */
export type Official = {
  status: "active" | "deprecated" | "deleted";
  statusMessage?: string;
  statusChangedAt?: string;
  publishedAt?: string;
  updatedAt: string;
  isLatest?: boolean;
};

/** One `ServerResponse`: the publisher's server.json and the registry's metadata. */
export type ServerRecord = {
  server: Record<string, unknown> & { name: string; version: string };
  _meta: { [OFFICIAL_KEY]: Official } & Record<string, unknown>;
};

export type FetchLike = (url: string, init?: { headers?: Record<string, string>; signal?: AbortSignal }) => Promise<{
  status: number;
  headers: { get(name: string): string | null };
  text(): Promise<string>;
}>;

export type ClientOptions = {
  baseUrl?: string;
  fetchImpl?: FetchLike;
  /** Minimum time between two requests, in ms (default 1,000). */
  intervalMs?: number;
  /** Attempts per page before the pull stops (default 6). */
  attempts?: number;
  /** A page body over this many bytes is refused (default 16 MB). */
  maxPageBytes?: number;
  /** For tests: a clock and a sleep. */
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  /** Called with a line for the run log. */
  log?: (line: string) => void;
};

export class RegistryError extends Error {}

const STATUSES = new Set(["active", "deprecated", "deleted"]);
const RFC3339 = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;

/** Checks one record against the fields the mirror relies on. A record that
 *  fails is reported and skipped, never stored. */
export function checkRecord(r: unknown): string | undefined {
  if (!isRecord(r)) return "not an object";
  const s = r["server"];
  if (!isRecord(s)) return "no server object";
  if (typeof s["name"] !== "string" || !/^[a-zA-Z0-9][a-zA-Z0-9.-]*[a-zA-Z0-9]\/[a-zA-Z0-9][a-zA-Z0-9._-]*[a-zA-Z0-9]$/.test(s["name"]) || s["name"].length > 200)
    return "server.name does not match the registry's pattern";
  if (typeof s["version"] !== "string" || s["version"].length < 1 || s["version"].length > 255) return "server.version missing or over 255 characters";
  const m = r["_meta"];
  if (!isRecord(m) || !isRecord(m[OFFICIAL_KEY])) return `no _meta["${OFFICIAL_KEY}"]`;
  const o = m[OFFICIAL_KEY];
  if (typeof o["status"] !== "string" || !STATUSES.has(o["status"])) return "official.status is not active, deprecated or deleted";
  if (typeof o["updatedAt"] !== "string" || !RFC3339.test(o["updatedAt"])) return "official.updatedAt is not an RFC 3339 time";
  if (o["isLatest"] !== undefined && typeof o["isLatest"] !== "boolean") return "official.isLatest is not a boolean";
  return undefined;
}

export type Page = { records: ServerRecord[]; skipped: { index: number; reason: string; name?: string }[]; nextCursor?: string };

export class RegistryClient {
  private readonly base: string;
  private readonly fetchImpl: FetchLike;
  private readonly intervalMs: number;
  private readonly attempts: number;
  private readonly maxPageBytes: number;
  private readonly now: () => number;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly log: (line: string) => void;
  private last = -Infinity;
  requests = 0;

  constructor(o: ClientOptions = {}) {
    this.base = (o.baseUrl ?? OFFICIAL_REGISTRY).replace(/\/+$/, "");
    if (!/^https:\/\//.test(this.base) && !/^http:\/\/127\.0\.0\.1(:\d+)?$/.test(this.base)) throw new RegistryError("the registry base URL must be https (or http://127.0.0.1 for a test server)");
    this.fetchImpl = o.fetchImpl ?? (globalThis.fetch as unknown as FetchLike);
    this.intervalMs = o.intervalMs ?? 1000;
    this.attempts = o.attempts ?? 6;
    this.maxPageBytes = o.maxPageBytes ?? 16 * 1024 * 1024;
    this.now = o.now ?? Date.now;
    this.sleep = o.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
    this.log = o.log ?? (() => {});
  }

  listUrl(q: { cursor?: string; updatedSince?: string; includeDeleted?: boolean }): string {
    const p = new URLSearchParams();
    p.set("limit", String(PAGE_LIMIT));
    if (q.updatedSince) p.set("updated_since", q.updatedSince);
    // With updated_since the registry includes deleted servers itself and refuses `false`.
    else if (q.includeDeleted) p.set("include_deleted", "true");
    if (q.cursor) p.set("cursor", q.cursor);
    return `${this.base}/v0.1/servers?${p.toString()}`;
  }

  async page(q: { cursor?: string; updatedSince?: string; includeDeleted?: boolean }): Promise<Page> {
    const url = this.listUrl(q);
    let delay = 2000;
    for (let attempt = 1; ; attempt++) {
      const wait = this.last + this.intervalMs - this.now();
      if (wait > 0) await this.sleep(wait);
      this.last = this.now();
      this.requests++;
      let status = 0;
      let text = "";
      let retryAfter: string | null = null;
      try {
        const res = await this.fetchImpl(url, { headers: { accept: "application/json", "user-agent": "tk-library-mirror/0 (+https://github.com/aetiusgular/tk)" } });
        status = res.status;
        retryAfter = res.headers.get("retry-after");
        const len = Number(res.headers.get("content-length") ?? "0");
        if (len > this.maxPageBytes) throw new RegistryError(`page is ${len} bytes, over the ${this.maxPageBytes} cap`);
        text = await res.text();
        if (Buffer.byteLength(text) > this.maxPageBytes) throw new RegistryError(`page over the ${this.maxPageBytes} byte cap`);
      } catch (e) {
        if (e instanceof RegistryError) throw e;
        status = 0; // network error: retried like a 5xx
      }
      if (status === 200) return parsePage(text);
      const retryable = status === 0 || status === 429 || status >= 500;
      if (!retryable || attempt >= this.attempts) throw new RegistryError(`GET ${url} failed: ${status === 0 ? "network error" : `HTTP ${status}`} after ${attempt} attempt(s)`);
      const ra = retryAfter !== null && /^\d{1,5}$/.test(retryAfter) ? Number(retryAfter) * 1000 : undefined;
      const ms = Math.min(ra ?? delay, 5 * 60_000);
      this.log(`registry answered ${status === 0 ? "with a network error" : status}; retry ${attempt} in ${Math.round(ms / 1000)} s`);
      await this.sleep(ms);
      // The backoff doubles only when it was the wait used.
      if (ra === undefined) delay = Math.min(delay * 2, 5 * 60_000);
    }
  }

  /** Every record from the first page to the last. Stops at `maxPages`
   *  (a guard against a cursor that never ends), reporting it. */
  async *all(q: { updatedSince?: string; includeDeleted?: boolean; maxPages?: number }): AsyncGenerator<Page> {
    let cursor: string | undefined;
    const seen = new Set<string>();
    const max = q.maxPages ?? 2000;
    for (let n = 0; n < max; n++) {
      const page = await this.page({ ...(cursor ? { cursor } : {}), ...(q.updatedSince ? { updatedSince: q.updatedSince } : {}), ...(q.includeDeleted ? { includeDeleted: true } : {}) });
      yield page;
      if (!page.nextCursor) return;
      if (seen.has(page.nextCursor)) throw new RegistryError(`the registry returned cursor ${JSON.stringify(page.nextCursor)} twice; stopping`);
      seen.add(page.nextCursor);
      cursor = page.nextCursor;
    }
    throw new RegistryError(`stopped after ${max} pages with a cursor still open`);
  }
}

export function parsePage(text: string): Page {
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    throw new RegistryError("the registry's page is not JSON");
  }
  if (!isRecord(body) || !Array.isArray(body["servers"])) throw new RegistryError("the registry's page has no servers array");
  const records: ServerRecord[] = [];
  const skipped: Page["skipped"] = [];
  body["servers"].forEach((r: unknown, index: number) => {
    const reason = checkRecord(r);
    if (reason) {
      const name = isRecord(r) && isRecord(r["server"]) && typeof r["server"]["name"] === "string" ? String(r["server"]["name"]).slice(0, 200) : undefined;
      skipped.push({ index, reason, ...(name !== undefined ? { name } : {}) });
    } else records.push(r as ServerRecord);
  });
  const md = body["metadata"];
  const next = isRecord(md) && typeof md["nextCursor"] === "string" && md["nextCursor"] !== "" ? md["nextCursor"] : undefined;
  return { records, skipped, ...(next !== undefined ? { nextCursor: next } : {}) };
}
