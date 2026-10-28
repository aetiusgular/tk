// Namespace proof for `tk publish` (lane G, T3.3).
//
// The rules are the official registry's (read at 9c4a96c, live 1.8.6):
//   io.github.<login>/*  a GitHub login, or an org whose membership role is
//                        `admin` and state `active` (auth/github_at.go:89-222);
//                        from Actions, the OIDC `repository_owner`
//                        (auth/github_oidc.go:280-293)
//   <reverse domain>/*   a TXT record `v=MCPv1; k=<alg>; p=<base64>` on the
//                        domain's apex (DNS, which also covers subdomains) or
//                        the same line at https://<domain>/.well-known/mcp-registry-auth
//                        (HTTP, the domain only) (auth/dns.go:84-113,
//                        auth/http.go:190-279, auth/common.go:42, 221-240)
//   github.io and its subdomains never prove a domain (common.go:391-401);
//   IP literals and single-label names never do (common.go:403-415).
//
// What differs, and why: upstream signs a timestamp inside a 15 s window to
// mint a login token. A library pull request is reviewed later, so tk's
// publisher signs a statement that names the entry and its content hash with
// the same key; tk's CI checks that signature against the live record when the
// pull request is opened and again before merge.
import { createPrivateKey, createPublicKey, ECDH, sign as cryptoSign, verify as cryptoVerify, type KeyObject } from "node:crypto";
import { promises as dns } from "node:dns";
import { GuardError, guardedRequest, type GuardedRequest } from "../mirror/netguard.ts";

export type Namespace = { kind: "github"; owner: string } | { kind: "domain"; domain: string };

/** The registry's name patterns (internal/validators/validators.go:19-25). */
const NAME = /^([a-zA-Z0-9][a-zA-Z0-9.-]*[a-zA-Z0-9])\/([a-zA-Z0-9][a-zA-Z0-9._-]*[a-zA-Z0-9])$/;

export function namespaceOf(serverName: string): Namespace | { error: string } {
  const m = NAME.exec(serverName);
  if (!m || serverName.length > 200) return { error: "the name does not match the registry's `<namespace>/<name>` pattern" };
  const ns = m[1]!;
  if (ns.toLowerCase().startsWith("io.github.")) {
    const owner = ns.slice("io.github.".length);
    // A GitHub login: letters, digits and single hyphens, at most 39 characters.
    if (!/^[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$/.test(owner)) return { error: `io.github.${owner} is not a GitHub login` };
    return { kind: "github", owner };
  }
  const domain = ns.split(".").reverse().join(".").toLowerCase();
  if (!domain.includes(".")) return { error: "a single-label namespace proves no domain" };
  if (domain === "github.io" || domain.endsWith(".github.io")) return { error: "github.io never proves a namespace; use io.github.<login>" };
  if (/^\d+(\.\d+){3}$/.test(domain)) return { error: "an IP address proves no domain" };
  if (domain.length > 253 || domain.split(".").some((l) => l.length === 0 || l.length > 63 || !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(l))) return { error: `${domain} is not a valid domain` };
  return { kind: "domain", domain };
}

// ---- keys -----------------------------------------------------------------

export type Algorithm = "ed25519" | "ecdsap384";
export type ProofKey = { algorithm: Algorithm; key: KeyObject; raw: Buffer };

/** The registry's record pattern (common.go:42). */
export const RECORD = /v=MCPv1;\s*k=([^;]+);\s*p=([A-Za-z0-9+/=]+)/;

/** A public key from a `v=MCPv1` line, as the registry parses it (common.go:345-381). */
export function parseRecord(line: string): ProofKey | { error: string } {
  const m = RECORD.exec(line);
  if (!m) return { error: "no v=MCPv1 record" };
  const alg = m[1]!.trim();
  const raw = Buffer.from(m[2]!, "base64");
  if (alg === "ed25519") {
    if (raw.length !== 32) return { error: "an Ed25519 key is 32 bytes" };
    return { algorithm: "ed25519", raw, key: createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: raw.toString("base64url") }, format: "jwk" }) };
  }
  if (alg === "ecdsap384") {
    if (raw.length !== 49 || (raw[0] !== 2 && raw[0] !== 3)) return { error: "an ECDSA P-384 key is 49 bytes, compressed" };
    let full: Buffer;
    try {
      full = ECDH.convertKey(raw, "secp384r1", undefined, undefined, "uncompressed") as Buffer;
    } catch {
      return { error: "the ECDSA P-384 key is not on the curve" };
    }
    return { algorithm: "ecdsap384", raw, key: createPublicKey({ key: { kty: "EC", crv: "P-384", x: full.subarray(1, 49).toString("base64url"), y: full.subarray(49).toString("base64url") }, format: "jwk" }) };
  }
  return { error: `unsupported key algorithm ${JSON.stringify(alg.slice(0, 20))}` };
}

/** The `v=MCPv1` line for a private key, to put in DNS or the well-known file. */
export function recordFor(privatePem: string): string {
  const priv = createPrivateKey(privatePem);
  const pub = createPublicKey(priv);
  const jwk = pub.export({ format: "jwk" }) as { kty: string; crv?: string; x?: string; y?: string };
  if (jwk.kty === "OKP" && jwk.crv === "Ed25519") return `v=MCPv1; k=ed25519; p=${Buffer.from(jwk.x!, "base64url").toString("base64")}`;
  if (jwk.kty === "EC" && jwk.crv === "P-384") {
    const y = Buffer.from(jwk.y!, "base64url");
    const prefix = (y[y.length - 1]! & 1) === 1 ? 3 : 2;
    return `v=MCPv1; k=ecdsap384; p=${Buffer.concat([Buffer.from([prefix]), Buffer.from(jwk.x!, "base64url")]).toString("base64")}`;
  }
  throw new Error("the key is neither Ed25519 nor ECDSA P-384, the two the registry takes");
}

// ---- the signed statement -------------------------------------------------

export type Statement = { name: string; version: string; contentSha256: string; signedAt: string };

/** The bytes a domain proof signs. A newline cannot occur in a server name,
 *  a version tk publishes, a hash or a time, so the fields cannot run together. */
export function statementBytes(s: Statement): Buffer {
  for (const v of Object.values(s)) if (/[\r\n]/.test(v)) throw new Error("a statement field holds a line break");
  return Buffer.from(`tk-publish:v1\n${s.name}\n${s.version}\n${s.contentSha256}\n${s.signedAt}`, "utf8");
}

/** Signs with Ed25519, or ECDSA P-384 over SHA-384 as raw r||s (the
 *  registry's encoding, common.go:191-219). Returns hex. */
export function signStatement(s: Statement, privatePem: string): string {
  const key = createPrivateKey(privatePem);
  const data = statementBytes(s);
  const kind = key.asymmetricKeyType;
  if (kind === "ed25519") return cryptoSign(null, data, key).toString("hex");
  if (kind === "ec" && (key.asymmetricKeyDetails?.namedCurve === "secp384r1" || key.asymmetricKeyDetails?.namedCurve === "P-384"))
    return cryptoSign("sha384", data, { key, dsaEncoding: "ieee-p1363" }).toString("hex");
  throw new Error("the key is neither Ed25519 nor ECDSA P-384");
}

export function verifyStatement(s: Statement, signatureHex: string, k: ProofKey): boolean {
  if (!/^[0-9a-f]+$/.test(signatureHex)) return false;
  const sig = Buffer.from(signatureHex, "hex");
  const data = statementBytes(s);
  try {
    if (k.algorithm === "ed25519") return sig.length === 64 && cryptoVerify(null, data, k.key, sig);
    return sig.length === 96 && cryptoVerify("sha384", data, { key: k.key, dsaEncoding: "ieee-p1363" }, sig);
  } catch {
    return false;
  }
}

// ---- finding the record ---------------------------------------------------

export type ResolveTxt = (name: string) => Promise<string[][]>;
export type FetchWellKnown = (domain: string) => Promise<string>;

const defaultResolveTxt: ResolveTxt = (n) => dns.resolveTxt(n);

/** The well-known file through the same guard the mirror's probes use: https
 *  only, public addresses only, no redirects, 10 s, 4,096 bytes (the
 *  registry's own cap, auth/http.go:19). */
export function wellKnownFetcher(o: Pick<GuardedRequest, "lookup" | "requestImpl"> = {}): FetchWellKnown {
  return async (domain) => {
    const r = await guardedRequest(`https://${domain}/.well-known/mcp-registry-auth`, { method: "GET", headers: { accept: "text/plain" }, maxBytes: 4096, timeoutMs: 10_000, ...o });
    if (r.status !== 200) throw new GuardError("status", `HTTP ${r.status}`);
    if (r.truncated) throw new GuardError("size", "the file is over 4,096 bytes");
    return r.body.trim();
  };
}

export type DomainProof = { method: "dns" | "http-well-known"; domain: string; key: ProofKey };

/** The keys that may sign for `domain`: DNS on the domain and each parent
 *  down to two labels (a DNS record covers subdomains), then the well-known
 *  file on the domain itself (HTTP covers only that domain). */
export async function domainKeys(domain: string, deps: { resolveTxt?: ResolveTxt; fetchWellKnown?: FetchWellKnown } = {}): Promise<{ keys: DomainProof[]; notes: string[] }> {
  const resolveTxt = deps.resolveTxt ?? defaultResolveTxt;
  const keys: DomainProof[] = [];
  const notes: string[] = [];
  const labels = domain.split(".");
  for (let i = 0; i + 2 <= labels.length; i++) {
    const d = labels.slice(i).join(".");
    if (d === "github.io" || d.endsWith(".github.io")) continue;
    let records: string[][] = [];
    try {
      records = await resolveTxt(d);
    } catch (e) {
      notes.push(`dns ${d}: ${(e as NodeJS.ErrnoException).code ?? "lookup failed"}`);
      continue;
    }
    for (const parts of records) {
      const line = parts.join("");
      if (!RECORD.test(line)) continue;
      const k = parseRecord(line);
      if ("error" in k) notes.push(`dns ${d}: ${k.error}`);
      else keys.push({ method: "dns", domain: d, key: k });
    }
  }
  {
    const fetchWellKnown = deps.fetchWellKnown ?? wellKnownFetcher();
    try {
      const body = await fetchWellKnown(domain);
      for (const line of body.split(/\r?\n/)) {
        if (!RECORD.test(line)) continue;
        const k = parseRecord(line);
        if ("error" in k) notes.push(`well-known ${domain}: ${k.error}`);
        else keys.push({ method: "http-well-known", domain, key: k });
      }
    } catch (e) {
      notes.push(`well-known ${domain}: ${(e as Error).message}`);
    }
  }
  return { keys, notes };
}

// ---- GitHub ---------------------------------------------------------------

/** Runs `gh` with an argument list and returns stdout; never a shell. */
export type Gh = (args: string[]) => Promise<string>;

/** Whether the logged-in GitHub user may publish under `owner`: the user, or
 *  an active admin of the org, as the registry decides (github_at.go:149-222).
 *  The token stays inside `gh`. */
export async function githubMayPublish(owner: string, gh: Gh): Promise<{ ok: boolean; login?: string; how?: string; note?: string }> {
  let login: string;
  try {
    login = (await gh(["api", "user", "--jq", ".login"])).trim();
  } catch {
    return { ok: false, note: "gh is not logged in (`gh auth login`)" };
  }
  if (!/^[A-Za-z0-9-]{1,39}$/.test(login)) return { ok: false, note: "gh returned no login" };
  if (login.toLowerCase() === owner.toLowerCase()) return { ok: true, login, how: "the logged-in user" };
  try {
    const m = JSON.parse(await gh(["api", `user/memberships/orgs/${owner}`])) as { role?: string; state?: string };
    if (m.role === "admin" && m.state === "active") return { ok: true, login, how: `an owner of ${owner}` };
    return { ok: false, login, note: `${login} is ${m.state ?? "not"} ${m.role ?? "a member"} of ${owner}; the registry grants an org's namespace to its owners only` };
  } catch {
    return { ok: false, login, note: `${login} is not ${owner}, and gh could not read a membership in ${owner} (a token needs read:org)` };
  }
}
