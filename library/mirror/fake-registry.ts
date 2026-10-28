// A fake of the official registry's list API for the mirror's tests (lane G).
// It reproduces what the mirror depends on, read in the registry's source at
// 9c4a96c: records ordered by name then version as text (postgres.go:226-237),
// `updated_at > since` (postgres.go:137), deleted rows hidden unless
// `include_deleted` or `updated_since` (servers.go:43-50), `limit` 1 to 100
// (servers.go:73), a `name:version` cursor, a status change that moves
// `updated_at` (postgres.go:712-718) and a new version that clears the old
// one's `is_latest` without moving its `updated_at` (postgres.go:996).
import { createServer, type Server } from "node:http";
import type { FetchLike, Official, ServerRecord } from "./registry.ts";
import { OFFICIAL_KEY } from "./registry.ts";

type Row = { server: ServerRecord["server"]; official: Official };

export class FakeRegistry {
  rows: Row[] = [];
  clock = Date.parse("2026-10-01T00:00:00Z");
  calls: string[] = [];
  /** Answers to send before the real page, by request number (1-based). */
  failures = new Map<number, { status: number; retryAfter?: string }>();

  private tick(): string {
    this.clock += 1000;
    return new Date(this.clock).toISOString();
  }

  publish(server: Record<string, unknown> & { name: string; version: string }, opts: { at?: string } = {}): void {
    const at = opts.at ?? this.tick();
    for (const r of this.rows) if (r.server.name === server.name && r.official.isLatest) r.official.isLatest = false; // updatedAt NOT moved
    this.rows.push({ server, official: { status: "active", publishedAt: at, updatedAt: at, isLatest: true } });
  }

  setStatus(name: string, version: string, status: Official["status"]): void {
    const r = this.rows.find((x) => x.server.name === name && x.server.version === version);
    if (!r) throw new Error("no such row");
    r.official.status = status;
    r.official.updatedAt = this.tick();
    r.official.statusChangedAt = r.official.updatedAt;
  }

  /** Drops a row, as a registry data reset would. */
  drop(name: string): void {
    this.rows = this.rows.filter((r) => r.server.name !== name);
  }

  handle(url: string): { status: number; body: string; headers: Record<string, string> } {
    this.calls.push(url);
    const fail = this.failures.get(this.calls.length);
    if (fail) return { status: fail.status, body: "{}", headers: fail.retryAfter ? { "retry-after": fail.retryAfter } : {} };
    const u = new URL(url, "http://x");
    if (u.pathname !== "/v0.1/servers") return { status: 404, body: '{"error":"not found"}', headers: {} };
    const limitRaw = u.searchParams.get("limit") ?? "30";
    const limit = Number(limitRaw);
    if (!(limit >= 1 && limit <= 100)) return { status: 422, body: '{"error":"limit"}', headers: {} };
    const since = u.searchParams.get("updated_since");
    const inc = u.searchParams.get("include_deleted");
    if (since && inc === "false") return { status: 400, body: '{"error":"Cannot set include_deleted=false when using updated_since"}', headers: {} };
    const includeDeleted = since !== null || inc === "true";
    const cursor = u.searchParams.get("cursor");
    let rows = [...this.rows].sort((a, b) => (a.server.name < b.server.name ? -1 : a.server.name > b.server.name ? 1 : a.server.version < b.server.version ? -1 : a.server.version > b.server.version ? 1 : 0));
    if (since) rows = rows.filter((r) => Date.parse(r.official.updatedAt) > Date.parse(since));
    if (!includeDeleted) rows = rows.filter((r) => r.official.status !== "deleted");
    if (cursor) {
      const i = rows.findIndex((r) => `${r.server.name}:${r.server.version}` === cursor);
      rows = i < 0 ? [] : rows.slice(i + 1);
    }
    const page = rows.slice(0, limit);
    const more = rows.length > limit;
    const last = page.at(-1);
    return {
      status: 200,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        servers: page.map((r) => ({ server: r.server, _meta: { [OFFICIAL_KEY]: { ...r.official } } })),
        metadata: { count: page.length, ...(more && last ? { nextCursor: `${last.server.name}:${last.server.version}` } : {}) },
      }),
    };
  }

  fetch: FetchLike = async (url) => {
    const r = this.handle(url);
    return { status: r.status, headers: { get: (n: string) => r.headers[n.toLowerCase()] ?? null }, text: async () => r.body };
  };

  /** A real HTTP server on 127.0.0.1 for command-line tests. */
  listen(): Promise<{ server: Server; baseUrl: string }> {
    const server = createServer((req, res) => {
      const r = this.handle(req.url ?? "/");
      res.writeHead(r.status, { "content-type": "application/json", ...r.headers });
      res.end(r.body);
    });
    return new Promise((resolve) => server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      resolve({ server, baseUrl: `http://127.0.0.1:${typeof addr === "object" && addr ? addr.port : 0}` });
    }));
  }
}

export function remoteServer(name: string, version: string, url: string, extra: Record<string, unknown> = {}): Record<string, unknown> & { name: string; version: string } {
  return {
    $schema: "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json",
    name,
    description: `A test server named ${name} for the mirror's tests.`,
    version,
    remotes: [{ type: "streamable-http", url }],
    ...extra,
  };
}
