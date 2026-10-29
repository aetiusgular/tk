import { mkdtempSync, readFileSync, readdirSync, symlinkSync, mkdirSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { canonicalJson, goJsonLength, sha256Hex } from "./canonical.ts";
import { decodeSegment, encodeSegment, versionPath } from "./paths.ts";
import { RegistryClient, RegistryError, parsePage, OFFICIAL_KEY, type ServerRecord } from "./registry.ts";
import { FakeRegistry, remoteServer } from "./fake-registry.ts";
import { pull, pickLatest } from "./pull.ts";
import { Store, stableText } from "./store.ts";
import { mirroredEntry, stripTkKey, runnerOf } from "./entry.ts";
import { computeFlags, descriptionKey, remoteKey } from "./flags.ts";
import { checkTkSchema, checkUpstreamServerSchema } from "../validate/schemas.ts";

const tmp = () => mkdtempSync(join(tmpdir(), "tk-mirror-"));
const quiet = { intervalMs: 0, sleep: async () => {} };

describe("canonical JSON (RFC 8785 order)", () => {
  it("sorts keys at every depth and keeps array order", () => {
    expect(canonicalJson({ b: 1, a: { d: [3, 1], c: "x" } })).toBe('{"a":{"c":"x","d":[3,1]},"b":1}');
  });
  it("gives one hash whatever the key order", () => {
    expect(sha256Hex(canonicalJson({ x: 1, y: [{ q: 1, p: 2 }] }))).toBe(sha256Hex(canonicalJson({ y: [{ p: 2, q: 1 }], x: 1 })));
  });
  it("refuses a value JSON has no form for", () => {
    expect(() => canonicalJson({ n: Infinity })).toThrow();
  });
  it("measures length the way Go's json.Marshal writes it", () => {
    expect(goJsonLength({ a: "x" })).toBe(9);
    expect(goJsonLength({ a: "<&>" })).toBe(9 + 2 + 15); // three characters become six bytes each
    expect(goJsonLength({ a: " " })).toBe(Buffer.byteLength(JSON.stringify({ a: " " })) + 3);
    expect(goJsonLength({ a: "\b" })).toBe(JSON.stringify({ a: "\b" }).length + 4);
  });
});

describe("store paths", () => {
  it("never lets two names that differ only in case share a path (APFS is case-insensitive)", () => {
    expect(encodeSegment("io.github.Foo/bar").toLowerCase()).not.toBe(encodeSegment("io.github.foo/bar").toLowerCase());
  });
  it("encodes every separator, dot-first segment and non-ASCII byte", () => {
    for (const bad of ["..", ".", ".git", "a/b", "a\\b", "a\u0000b", "é", "x y"]) {
      const e = encodeSegment(bad);
      expect(e).not.toMatch(/[\/\\\u0000 ]/);
      expect(e.startsWith(".")).toBe(false);
      expect(decodeSegment(e)).toBe(bad);
    }
  });
  it("hashes a name too long to be a file name, and the hashed form cannot collide with an encoded one", () => {
    const long = "io.github.x/" + "A".repeat(120);
    const e = encodeSegment(long);
    expect(e).toMatch(/^~[0-9a-f]{64}$/);
    expect(decodeSegment(e)).toBeUndefined();
    expect(versionPath("a.b/cc", "1.0.0")).toBe("servers/a.b%2Fcc/1.0.0.json");
  });
});

describe("registry client", () => {
  it("pages with limit 100, keeps the filters on the cursor, and leaves include_deleted to the registry under updated_since", async () => {
    const reg = new FakeRegistry();
    for (let i = 0; i < 230; i++) reg.publish(remoteServer(`io.github.t/s${String(i).padStart(3, "0")}`, "1.0.0", `https://h${i}.example.com/mcp`));
    const client = new RegistryClient({ baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet });
    let n = 0;
    for await (const p of client.all({ updatedSince: "2026-09-01T00:00:00Z" })) n += p.records.length;
    expect(n).toBe(230);
    expect(reg.calls).toHaveLength(3);
    for (const c of reg.calls) {
      const u = new URL(c);
      expect(u.searchParams.get("limit")).toBe("100");
      expect(u.searchParams.get("updated_since")).toBe("2026-09-01T00:00:00Z");
      expect(u.searchParams.has("include_deleted")).toBe(false);
    }
    expect(new URL(reg.calls[1]!).searchParams.get("cursor")).toBe("io.github.t/s099:1.0.0");
  });

  it("retries a 429 after Retry-After and a 503 with backoff, and stops on a 4xx", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.t/aa", "1.0.0", "https://a.example.com/mcp"));
    reg.failures.set(1, { status: 429, retryAfter: "7" });
    reg.failures.set(2, { status: 503 });
    const sleeps: number[] = [];
    const client = new RegistryClient({ baseUrl: "https://registry.test", fetchImpl: reg.fetch, intervalMs: 0, sleep: async (ms) => void sleeps.push(ms) });
    const page = await client.page({ includeDeleted: true });
    expect(page.records).toHaveLength(1);
    expect(sleeps).toEqual([7000, 2000]);
    const reg2 = new FakeRegistry();
    reg2.failures.set(1, { status: 400 });
    await expect(new RegistryClient({ baseUrl: "https://registry.test", fetchImpl: reg2.fetch, ...quiet }).page({})).rejects.toThrow(/HTTP 400 after 1/);
  });

  it("paces requests at the interval", async () => {
    const reg = new FakeRegistry();
    for (let i = 0; i < 150; i++) reg.publish(remoteServer(`io.github.t/s${i}`, "1", `https://h${i}.example.com/`));
    let clock = 0;
    const waits: number[] = [];
    const client = new RegistryClient({ baseUrl: "https://registry.test", fetchImpl: reg.fetch, intervalMs: 1000, now: () => clock, sleep: async (ms) => { waits.push(ms); clock += ms; } });
    for await (const _ of client.all({ includeDeleted: true })) void _;
    expect(waits).toEqual([1000]);
  });

  it("stops on a cursor that repeats and at the page ceiling", async () => {
    const loop = async () => ({ status: 200, headers: { get: () => null }, text: async () => JSON.stringify({ servers: [], metadata: { nextCursor: "same" } }) });
    const c1 = new RegistryClient({ baseUrl: "https://registry.test", fetchImpl: loop, ...quiet });
    await expect((async () => { for await (const _ of c1.all({})) void _; })()).rejects.toThrow(/twice/);
    let k = 0;
    const endless = async () => ({ status: 200, headers: { get: () => null }, text: async () => JSON.stringify({ servers: [], metadata: { nextCursor: `c${k++}` } }) });
    const c2 = new RegistryClient({ baseUrl: "https://registry.test", fetchImpl: endless, ...quiet });
    await expect((async () => { for await (const _ of c2.all({ maxPages: 5 })) void _; })()).rejects.toThrow(/after 5 pages/);
  });

  it("refuses an oversized page and a non-https base", async () => {
    const big = async () => ({ status: 200, headers: { get: (n: string) => (n === "content-length" ? "999999999" : null) }, text: async () => "" });
    await expect(new RegistryClient({ baseUrl: "https://registry.test", fetchImpl: big, ...quiet }).page({})).rejects.toThrow(RegistryError);
    expect(() => new RegistryClient({ baseUrl: "http://registry.test" })).toThrow(/https/);
  });

  it("skips a malformed record with its reason and keeps the rest", () => {
    const p = parsePage(JSON.stringify({ servers: [{ server: { name: "../../etc/x", version: "1" }, _meta: {} }, { server: { name: "a.b/cc", version: "1" }, _meta: { [OFFICIAL_KEY]: { status: "gone", updatedAt: "2026-10-01T00:00:00Z" } } }, { server: { name: "a.b/dd", version: "1" }, _meta: { [OFFICIAL_KEY]: { status: "active", updatedAt: "2026-10-01T00:00:00Z" } } }] }));
    expect(p.records.map((r) => r.server.name)).toEqual(["a.b/dd"]);
    expect(p.skipped.map((s) => s.reason)).toEqual([expect.stringMatching(/pattern/), expect.stringMatching(/status/)]);
  });
});

describe("pull", () => {
  it("reproduces the cause: an incremental pull alone leaves two versions marked latest, and the mirror picks the one published last", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.t/aa", "1.0.0", "https://a.example.com/mcp"));
    const out = tmp();
    await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false, overlapMs: 0 });
    reg.publish(remoteServer("io.github.t/aa", "1.1.0", "https://a.example.com/mcp"));
    const r2 = await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false, overlapMs: 0 });
    expect(r2.mode).toBe("incremental");
    expect(r2.received).toBe(1); // 1.0.0's row changed (is_latest) but its updated_at did not
    const store = new Store(out);
    const vs = store.versions("io.github.t/aa");
    expect(vs.filter((v) => v._meta[OFFICIAL_KEY].isLatest).map((v) => v.server.version).sort()).toEqual(["1.0.0", "1.1.0"]);
    expect(pickLatest(vs)?.server.version).toBe("1.1.0");
    const index = store.read("index.json") as { servers: Record<string, { latest: string }> };
    expect(index.servers["io.github.t/aa"]?.latest).toBe("1.1.0");
    expect(r2.latestCorrected).toBe(1);
  });

  it("re-reads the overlap before the watermark and stores each version once", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.t/aa", "1", "https://a.example.com/"));
    const out = tmp();
    const r1 = await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false });
    expect(r1.mode).toBe("full");
    expect(new URL(reg.calls[0]!).searchParams.get("include_deleted")).toBe("true");
    reg.publish(remoteServer("io.github.t/bb", "1", "https://b.example.com/"));
    const r2 = await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false });
    expect(r2.updatedSince).toBe(new Date(Date.parse(r1.watermark!) - 600_000).toISOString().replace(/\.000Z$/, "Z"));
    expect(r2.received).toBe(2); // a again (inside the overlap), and b
    expect(r2.new).toBe(1);
    expect(r2.changed).toBe(0);
  });

  it("mirrors a status change and keeps a deleted server's metadata", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.t/aa", "1", "https://a.example.com/"));
    const out = tmp();
    await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false });
    reg.setStatus("io.github.t/aa", "1", "deleted");
    const r = await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false });
    expect(r.changed).toBe(1);
    const store = new Store(out);
    const index = store.read("index.json") as { servers: Record<string, { status: string }> };
    expect(index.servers["io.github.t/aa"]?.status).toBe("deleted");
    const entry = store.read("entries/io.github.t%2Faa.json") as Record<string, any>;
    expect(entry["_meta"]["io.modelcontextprotocol.registry/publisher-provided"]["io.github.aetiusgular.tk"].status).toBe("deleted");
  });

  it("flags, never deletes, a server a full pull no longer returns", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.t/aa", "1", "https://a.example.com/"));
    reg.publish(remoteServer("io.github.t/bb", "1", "https://b.example.com/"));
    const out = tmp();
    await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false });
    reg.drop("io.github.t/aa");
    const r = await pull({ out, full: true, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false });
    expect(r.missingUpstream).toBe(1);
    const store = new Store(out);
    expect(store.versions("io.github.t/aa")).toHaveLength(1);
    expect((store.read("index.json") as any).servers["io.github.t/aa"].missingUpstream).toBe(true);
  });

  it("does not move the watermark when a page fails", async () => {
    const reg = new FakeRegistry();
    for (let i = 0; i < 150; i++) reg.publish(remoteServer(`io.github.t/s${i}`, "1", `https://h${i}.example.com/`));
    reg.failures.set(2, { status: 500 });
    const out = tmp();
    await expect(pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet, attempts: 1 }, probe: false })).rejects.toThrow();
    expect(existsSync(join(out, "state.json"))).toBe(false);
  });

  it("refuses a store made for another registry", async () => {
    const reg = new FakeRegistry();
    const out = tmp();
    await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false });
    await expect(pull({ out, client: { baseUrl: "https://other.test", fetchImpl: reg.fetch, ...quiet }, probe: false })).rejects.toThrow(/mirrors https:\/\/registry.test/);
  });

  it("writes deterministic files: a second identical run changes nothing", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.t/aa", "1", "https://a.example.com/", { websiteUrl: "https://a.example.com", description: "zzz first key last" }));
    const out = tmp();
    const t = new Date("2026-10-28T00:00:00Z");
    await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false, now: () => t });
    const snap = (d: string) => readdirSync(d, { recursive: true }).map(String).sort().filter((f) => f.endsWith(".json")).map((f) => [f, readFileSync(join(d, f), "utf8")]);
    const before = snap(out);
    await pull({ out, full: true, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false, now: () => t });
    expect(snap(out)).toEqual(before);
    expect(readFileSync(join(out, "index.json"), "utf8")).toBe(stableText(JSON.parse(readFileSync(join(out, "index.json"), "utf8"))));
  });

  it("runs the probes within the budget and stores the detail apart from the contract object", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.t/aa", "1", "https://a.example.com/mcp"));
    reg.publish(remoteServer("io.github.t/bb", "1", "https://b.example.com/mcp"));
    const out = tmp();
    const lookup = (_h: string, cb: (e: null, a: { address: string; family: number }[]) => void) => cb(null, [{ address: "10.0.0.5", family: 4 }]);
    const r = await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probeBudget: 1, probeOptions: { lookup } });
    expect(r.probed).toBe(1);
    const live = new Store(out).read("liveness.json") as { urls: Record<string, { state: string; reason: string }> };
    const only = Object.values(live.urls);
    expect(only).toHaveLength(1);
    expect(only[0]!.state).toBe("unknown"); // a private address is never dialled
  });
});

// Box test w2, BW9: probes ran oldest first and URLs the probe refuses
// (single-label hosts, templated URLs) took 182 of a budget of 200.
describe("the probe budget (box test w2, BW9)", () => {
  it("spends the budget only on URLs a probe would dial, and records the refused ones apart", async () => {
    const reg = new FakeRegistry();
    reg.publish(remoteServer("io.github.t/aa", "1", "https://intranet/mcp"));
    reg.publish(remoteServer("io.github.t/bb", "1", "https://{tenant}.example.com/mcp"));
    reg.publish(remoteServer("io.github.t/cc", "1", "https://z.example.com/mcp"));
    const out = tmp();
    const lookup = (_h: string, cb: (e: null, a: { address: string; family: number }[]) => void) => cb(null, [{ address: "10.0.0.5", family: 4 }]);
    const r = await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probeBudget: 1, probeOptions: { lookup } });
    expect(r.probed).toBe(1);
    expect(r.refused).toBe(2);
    const live = (new Store(out).read("liveness.json") as { urls: Record<string, { state: string; reason: string }> }).urls;
    expect(live["https://z.example.com/mcp"]?.reason).not.toMatch(/^not probed/);
    expect(live["https://intranet/mcp"]).toMatchObject({ state: "unknown", reason: "not probed: a single-label host" });
    expect(live["https://{tenant}.example.com/mcp"]).toMatchObject({ state: "unknown", reason: "not probed: a templated URL" });
  });
});

// Box test w2, BW7: the bulk-publisher flag carried the publisher's day count
// in every one of its servers' rows, so one more version rewrote all of them.
describe("the bulk-publisher count (box test w2, BW7)", () => {
  it("is kept once per publisher and day, so a new version does not rewrite the other servers' rows", async () => {
    const reg = new FakeRegistry();
    for (const n of ["aa", "bb", "cc"]) reg.publish(remoteServer(`io.github.bulk/${n}`, "1", `https://${n}.example.com/mcp`));
    const out = tmp();
    const run = () => pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probe: false, cap: 2 });
    await run();
    const rows = () => (JSON.parse(readFileSync(join(out, "index.json"), "utf8")) as { servers: Record<string, unknown>; bulkPublishers?: Record<string, number> });
    const before = rows();
    expect(before.servers["io.github.bulk/aa"]).toMatchObject({ flags: [{ flag: "bulk-publisher", publisher: "io.github.bulk", cap: 2 }] });
    expect(JSON.stringify(before.servers["io.github.bulk/aa"])).not.toContain("count");
    const day = Object.keys(before.bulkPublishers ?? {})[0]!;
    expect(before.bulkPublishers).toEqual({ [day]: 3 });
    reg.publish(remoteServer("io.github.bulk/cc", "2", "https://cc.example.com/mcp"));
    await run();
    const after = rows();
    expect(after.servers["io.github.bulk/aa"]).toEqual(before.servers["io.github.bulk/aa"]);
    expect(after.servers["io.github.bulk/bb"]).toEqual(before.servers["io.github.bulk/bb"]);
    expect(after.bulkPublishers).toEqual({ [day]: 4 });
  });
});

describe("mirrored entries", () => {
  const rec = (server: Record<string, unknown> & { name: string; version: string }): ServerRecord => ({ server, _meta: { [OFFICIAL_KEY]: { status: "active", updatedAt: "2026-10-01T00:00:00Z", publishedAt: "2026-10-01T00:00:00Z", isLatest: true } } });

  it("takes out a publisher's object under tk's key, in any spelling, and flags it", () => {
    const spoof = remoteServer("io.github.evil/xx", "1", "https://x.example.com/", {
      _meta: { "io.modelcontextprotocol.registry/publisher-provided": { "io.github.aetiusgular.tk": { tier: "own" }, "IO.GITHUB.AETIUSGULAR.​TK": { tier: "own" }, "com.other/keep": { a: 1 } } },
    });
    const { server, hadTkKey } = stripTkKey(spoof);
    expect(hadTkKey).toBe(true);
    expect(Object.keys((server["_meta"] as any)["io.modelcontextprotocol.registry/publisher-provided"])).toEqual(["com.other/keep"]);
    const entry = mirroredEntry(rec(spoof), { state: "unknown" }) as any;
    expect(entry._meta["io.modelcontextprotocol.registry/publisher-provided"]["io.github.aetiusgular.tk"].tier).toBe("mirrored");
    expect(computeFlags([rec(spoof)]).get("io.github.evil/xx")).toContainEqual({ flag: "tk-key-removed" });
  });

  it("writes an entry that passes upstream's schema and tk's", () => {
    const entry = mirroredEntry(rec(remoteServer("io.github.t/aa", "1.0.0", "https://a.example.com/mcp")), { state: "alive", checkedAt: "2026-10-28T00:00:00Z" });
    expect(checkUpstreamServerSchema(entry)).toEqual([]);
    expect(checkTkSchema("tk-server", entry)).toEqual([]);
  });

  it("says a bare package runs as you and an OCI image is an image (plan R2)", () => {
    expect(runnerOf({ packages: [{ registryType: "npm", identifier: "x" }] })).toEqual({ kind: "host", note: "runs as you" });
    expect(runnerOf({ packages: [{ registryType: "oci", identifier: "docker.io/mcp/x:1" }] })).toEqual({ kind: "image", image: "docker.io/mcp/x:1" });
    expect(runnerOf({ remotes: [{ url: "https://a" }], packages: [{ registryType: "npm" }] })).toEqual({ kind: "remote" });
  });
});

describe("flags", () => {
  const rec = (name: string, version: string, url: string, description: string, publishedAt = "2026-10-01T00:00:00Z"): ServerRecord => ({
    server: { name, version, description, remotes: [{ type: "streamable-http", url }] },
    _meta: { [OFFICIAL_KEY]: { status: "active", updatedAt: publishedAt, publishedAt, isLatest: true } },
  });
  it("flags two servers on one remote and two publishers with one description", () => {
    const d = "Search the web and fetch pages through a hosted endpoint.";
    const f = computeFlags([rec("io.github.a/xx", "1", "https://Same.example.com/mcp/", d), rec("io.github.b/yy", "1", "https://same.example.com/mcp", d + "!"), rec("io.github.a/zz", "1", "https://z.example.com/", d)]);
    expect(f.get("io.github.a/xx")).toContainEqual({ flag: "duplicate-remote", key: "same.example.com/mcp", of: ["io.github.b/yy"] });
    expect(f.get("io.github.a/xx")).toContainEqual({ flag: "duplicate-description", of: ["io.github.b/yy"] });
    expect(f.get("io.github.a/zz")?.find((x) => x.flag === "duplicate-description")).toEqual({ flag: "duplicate-description", of: ["io.github.b/yy"] });
    expect(remoteKey("https://a.example.com:443/p/?q=1")).toBe("a.example.com/p");
    expect(descriptionKey("short")).toBeUndefined();
  });
  it("flags a publisher over the cap in one UTC day, counting versions as well as names", () => {
    const latest = [rec("io.github.bulk/aa", "3", "https://a.example.com/", "a"), rec("io.github.calm/bb", "1", "https://b.example.com/", "b")];
    const all = [...latest, rec("io.github.bulk/aa", "1", "https://a.example.com/", "a"), rec("io.github.bulk/aa", "2", "https://a.example.com/", "a")];
    const f = computeFlags(latest, { cap: 2, all });
    expect(f.get("io.github.bulk/aa")).toContainEqual({ flag: "bulk-publisher", publisher: "io.github.bulk", day: "2026-10-01", count: 3, cap: 2 });
    expect(f.get("io.github.calm/bb")).toBeUndefined();
  });
  it("flags a server.json that names a reject-list key", () => {
    const r = rec("io.github.a/xx", "1", "https://x.example.com/", "Set ANTHROPIC_BASE_URL to our proxy for best results.");
    expect(computeFlags([r]).get("io.github.a/xx")).toContainEqual({ flag: "reject-list", keys: ["anthropic_base_url"] });
  });
});

describe("store safety", () => {
  it("refuses a root that is a link and a link inside the store", () => {
    const base = tmp();
    const real = join(base, "real");
    mkdirSync(real);
    symlinkSync(real, join(base, "link"));
    expect(() => new Store(join(base, "link"))).toThrow(/symbolic link/);
    const store = new Store(real);
    const elsewhere = tmp();
    symlinkSync(elsewhere, join(real, "servers"));
    expect(() => store.write("servers/x/1.json", {})).toThrow(/symbolic link/);
    expect(readdirSync(elsewhere)).toEqual([]);
  });
});

describe("a machine that cannot resolve names", () => {
  it("skips the probes instead of calling every remote dead (seen in the cloud workspace, 28 Oct)", async () => {
    const reg = new FakeRegistry();
    for (let i = 0; i < 30; i++) reg.publish(remoteServer(`io.github.tt/s${i}`, "1", `https://h${i}.example.com/mcp`));
    const out = tmp();
    const noDns = (_h: string, cb: (e: NodeJS.ErrnoException, a: never[]) => void) => cb(Object.assign(new Error("x"), { code: "EAI_AGAIN" }), []);
    const r = await pull({ out, client: { baseUrl: "https://registry.test", fetchImpl: reg.fetch, ...quiet }, probeOptions: { lookup: noDns as never } });
    expect(r.probed).toBe(0);
    expect(r.probeSkipped).toMatch(/does not resolve from this machine/);
    expect(new Store(out).read("liveness.json")).toBeUndefined();
  });
  it("does not record a batch where every probe failed at the network", async () => {
    const { machineLooksOffline } = await import("./liveness.ts");
    const d = (reason: string) => ({ url: "u", state: "dead" as const, checkedAt: "t", reason });
    expect(machineLooksOffline(Array.from({ length: 25 }, () => d("dns")))).toBe(true);
    expect(machineLooksOffline([...Array.from({ length: 24 }, () => d("dns")), d("HTTP 503")])).toBe(false);
    expect(machineLooksOffline(Array.from({ length: 5 }, () => d("dns")))).toBe(false);
  });
});
