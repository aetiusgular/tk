// Outbound HTTPS to hosts a stranger named (lane G).
//
// The mirror probes the remotes of about 30,000 entries and the publish flow
// fetches a domain's `/.well-known/mcp-registry-auth`; both hosts come from
// someone else's data. Prior art is the official registry's well-known
// fetcher (`internal/api/handlers/v0/auth/http.go:37-229` at 9c4a96c): https
// only, no redirects, a time and size cap, and a dial that refuses loopback,
// private, link-local, multicast, unspecified, CGNAT and the IPv6 prefixes
// that embed an IPv4 address, resolved once and pinned so DNS rebinding
// cannot swap the address between the check and the dial. Here the same
// rules, plus the other IANA special-purpose IPv4 blocks, through Node's
// `net.BlockList` and a `lookup` that every connection uses. Every checked
// address may be dialled, in turn (box test w2, BW8).
import { lookup as dnsLookup, type LookupAddress } from "node:dns";
import { request as httpsRequest } from "node:https";
import { BlockList, isIP } from "node:net";

const BLOCKED = new BlockList();
for (const [net, prefix] of [
  ["0.0.0.0", 8], // "this network"
  ["10.0.0.0", 8],
  ["100.64.0.0", 10], // CGNAT
  ["127.0.0.0", 8],
  ["169.254.0.0", 16], // link-local, cloud metadata
  ["172.16.0.0", 12],
  ["192.0.0.0", 24],
  ["192.0.2.0", 24],
  ["192.168.0.0", 16],
  ["198.18.0.0", 15],
  ["198.51.100.0", 24],
  ["203.0.113.0", 24],
  ["224.0.0.0", 4], // multicast
  ["240.0.0.0", 4], // reserved and broadcast
] as const)
  BLOCKED.addSubnet(net, prefix, "ipv4");
for (const [net, prefix] of [
  ["::", 128], // unspecified
  ["::1", 128],
  ["64:ff9b::", 96], // NAT64
  ["64:ff9b:1::", 48],
  ["100::", 64], // discard
  ["2001:db8::", 32], // documentation
  ["2002::", 16], // 6to4
  ["fc00::", 7], // ULA
  ["fe80::", 10], // link-local
  ["fec0::", 10], // site-local
  ["ff00::", 8], // multicast
] as const)
  BLOCKED.addSubnet(net, prefix, "ipv6");

export function isBlockedAddress(address: string): boolean {
  const family = isIP(address);
  if (family === 0) return true;
  if (family === 4) return BLOCKED.check(address, "ipv4");
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/i.exec(address);
  if (mapped && BLOCKED.check(mapped[1]!, "ipv4")) return true;
  return BLOCKED.check(address, "ipv6");
}

export class GuardError extends Error {
  readonly reason: string;
  constructor(reason: string, message: string) {
    super(message);
    this.reason = reason;
  }
}

/** Why a URL is never fetched, or undefined when it may be. */
export function refuseUrl(raw: string): string | undefined {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    return "not a URL";
  }
  if (u.protocol !== "https:") return "not https";
  if (u.username || u.password) return "credentials in the URL";
  const host = u.hostname.replace(/^\[|\]$/g, "");
  if (isIP(host) !== 0) return "an IP literal";
  if (!host.includes(".")) return "a single-label host";
  if (/\.(localhost|local|internal|home\.arpa)$/i.test(host) || host.toLowerCase() === "localhost") return "a local name";
  if (/\{|\}|%7B|%7D/i.test(raw)) return "a templated URL";
  return undefined;
}

export type Lookup = (hostname: string, cb: (err: NodeJS.ErrnoException | null, addresses: LookupAddress[]) => void) => void;

const defaultLookup: Lookup = (hostname, cb) => dnsLookup(hostname, { all: true, verbatim: true }, cb);

export type GuardedResponse = { status: number; headers: Record<string, string>; body: string; truncated: boolean };

export type GuardedRequest = {
  method: "GET" | "HEAD" | "POST";
  headers?: Record<string, string>;
  body?: string;
  timeoutMs?: number;
  maxBytes?: number;
  /** For tests: a resolver, and the request function (an https.request look-alike). */
  lookup?: Lookup;
  requestImpl?: typeof httpsRequest;
};

/** A `lookup` for the dial that answers only with addresses already
 *  checked, never a second DNS query (no rebinding). With `all` (Node's
 *  family autoselection asks for it) it gives every one, so a dual-stack
 *  name whose IPv6 path is broken still reaches its IPv4 address (box test
 *  w2, BW8; Node 22 `lib/net.js:1387-1403`, `doc/api/net.md` autoSelectFamily:
 *  the first AAAA, then the first A, then the second AAAA, and so on). */
export function pinnedLookup(addresses: LookupAddress[]) {
  const list = addresses.map((a) => ({ address: a.address, family: a.family }));
  return (_h: string, o: { all?: boolean } | number | undefined, cb: (...a: unknown[]) => void) => {
    if (typeof o === "object" && o !== null && o.all) cb(null, list.map((a) => ({ ...a })));
    else cb(null, list[0]!.address, list[0]!.family);
  };
}

/** One request. Every address the name resolves to must be public, or none
 *  is dialled; the connection dials only those addresses, each in turn
 *  until one answers (`pinnedLookup`). Redirects are returned, never
 *  followed. Rejects with a GuardError on refusal, a network error, the
 *  timeout or a body over `maxBytes` when `truncated` would hide a refusal. */
export function guardedRequest(url: string, opts: GuardedRequest): Promise<GuardedResponse> {
  const refused = refuseUrl(url);
  if (refused) return Promise.reject(new GuardError("refused", `not fetched: ${refused}`));
  const u = new URL(url);
  const lookup = opts.lookup ?? defaultLookup;
  const timeoutMs = opts.timeoutMs ?? 10_000;
  const maxBytes = opts.maxBytes ?? 64 * 1024;
  const request = opts.requestImpl ?? httpsRequest;
  return new Promise((resolve, reject) => {
    let settled = false;
    const done = (err: Error | undefined, res?: GuardedResponse) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (err) reject(err);
      else resolve(res!);
    };
    const timer = setTimeout(() => {
      req?.destroy();
      done(new GuardError("timeout", `no answer within ${timeoutMs} ms`));
    }, timeoutMs);
    let req: ReturnType<typeof httpsRequest> | undefined;
    lookup(u.hostname, (err, addresses) => {
      if (settled) return;
      if (err) return done(new GuardError("dns", `name did not resolve (${err.code ?? "error"})`));
      if (!addresses.length) return done(new GuardError("dns", "name resolved to no address"));
      const bad = addresses.find((a) => isBlockedAddress(a.address));
      if (bad) return done(new GuardError("private-address", "the name resolves to a private or special-purpose address"));
      const pinned = pinnedLookup(addresses);
      req = request(
        {
          protocol: "https:",
          hostname: u.hostname,
          port: u.port || 443,
          path: u.pathname + u.search,
          method: opts.method,
          headers: { "user-agent": "tk-library/0 (+https://github.com/aetiusgular/tk)", ...(opts.headers ?? {}) },
          servername: u.hostname,
          agent: false,
          // Every connection dials the addresses checked above, never a
          // second lookup; with autoselection on, each in turn.
          autoSelectFamily: true,
          lookup: pinned as never,
        },
        (res) => {
          const chunks: Buffer[] = [];
          let size = 0;
          let truncated = false;
          res.on("data", (c: Buffer) => {
            if (truncated) return;
            size += c.length;
            if (size > maxBytes) {
              truncated = true;
              chunks.push(c.subarray(0, maxBytes - (size - c.length)));
              res.destroy();
              finish();
            } else chunks.push(c);
          });
          const finish = () => {
            const headers: Record<string, string> = {};
            for (const [k, v] of Object.entries(res.headers)) if (v !== undefined) headers[k.toLowerCase()] = Array.isArray(v) ? v.join(", ") : v;
            done(undefined, { status: res.statusCode ?? 0, headers, body: Buffer.concat(chunks).toString("utf8"), truncated });
          };
          res.on("end", finish);
          res.on("error", (e) => (truncated ? finish() : done(new GuardError("network", `response failed (${(e as NodeJS.ErrnoException).code ?? e.message})`))));
        },
      );
      req.on("error", (e) => done(new GuardError("network", `request failed (${(e as NodeJS.ErrnoException).code ?? e.message})`)));
      if (opts.body !== undefined) req.write(opts.body);
      req.end();
    });
  });
}
