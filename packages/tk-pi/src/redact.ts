// A best-effort redactor for secret-like text in tool results and in the
// approval card's preview. contracts/docker-args.md section 2 is the reading
// this follows: SECRET_LIKE (dockerArgs.ts:38 in the fork) matches property
// NAMES, so it is used for the value of a key-value pair whose key matches,
// and well-known token shapes are matched on their own.
//
// Token shapes are simplified from gitleaks v8.30.1 config/gitleaks.toml
// (https://raw.githubusercontent.com/gitleaks/gitleaks/v8.30.1/config/gitleaks.toml):
// github-pat :2159, github-fine-grained-pat :2145, github-oauth :2152,
// github-app-token :2131, github-refresh-token :2170, aws-access-token :206,
// anthropic-api-key :150, openai-api-key :2704, slack-bot-token :2971,
// gcp-api-key :612, private-key :2785, jwt :2424. Each is looser than the
// gitleaks rule (shorter minimum lengths, no trailing-context check), so it
// redacts more, not less. No pattern has nested quantifiers: each runs in
// linear time.
//
// Best-effort: a secret in a shape not listed, split across lines, or encoded
// passes through. Recorded in the lane A status doc.

export const SECRET_LIKE = /token|secret|password|passwd|apikey|api_key|private_key/i;

export const REDACTED = "[REDACTED by tk]";

interface Shape {
  name: string;
  re: RegExp;
}

const SHAPES: Shape[] = [
  {
    name: "private-key",
    re: /-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----[\s\S]*?(?:-----END[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----|$)/g,
  },
  { name: "github", re: /\b(?:gh[pousr]_[0-9A-Za-z]{20,255}|github_pat_\w{20,255})/g },
  { name: "aws-access-key-id", re: /\b(?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z2-7]{16}\b/g },
  { name: "anthropic", re: /\bsk-ant-[A-Za-z0-9_-]{20,}/g },
  { name: "openai", re: /\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,}/g },
  { name: "slack", re: /\bxox[abeoprs]-[A-Za-z0-9-]{10,}/g },
  { name: "google-api-key", re: /\bAIza[\w-]{35}/g },
  { name: "jwt", re: /\bey[A-Za-z0-9_-]{10,}\.ey[A-Za-z0-9_\/\\-]{10,}\.[A-Za-z0-9_\/\\-]*={0,2}/g },
  { name: "bearer", re: /\b(Bearer|Basic)\s+[A-Za-z0-9._~+\/=-]{12,}/gi },
];

// A key that matches SECRET_LIKE, then `=`, `:` or `": "`, then the value.
// The key is kept; the value (quoted or a bare run) is replaced.
const PAIR =
  /(["']?)([A-Za-z0-9_.-]{0,64}(?:token|secret|password|passwd|apikey|api_key|private_key)[A-Za-z0-9_.-]{0,64})\1([ \t]*[:=][ \t]*)("[^"\n]{1,4096}"|'[^'\n]{1,4096}'|[^\s"',;&)}\]]{1,4096})/gi;

export interface RedactResult {
  text: string;
  /** Shape names and "pair" for each kind found; never the secret. */
  kinds: string[];
}

export function redactText(text: string): RedactResult {
  const kinds = new Set<string>();
  let out = text.replace(PAIR, (_m, q: string, key: string, sep: string, value: string) => {
    // Already redacted (a bare value stops at the marker's first space).
    if (value.replace(/^["']/, "").startsWith("[REDACTED")) return `${q}${key}${q}${sep}${value}`;
    kinds.add("pair");
    const quote = value.startsWith('"') ? '"' : value.startsWith("'") ? "'" : "";
    return `${q}${key}${q}${sep}${quote}${REDACTED}${quote}`;
  });
  for (const { name, re } of SHAPES) {
    re.lastIndex = 0;
    out = out.replace(re, (m: string, scheme?: string) => {
      kinds.add(name);
      if (name === "bearer" && typeof scheme === "string") return `${scheme} ${REDACTED}`;
      return REDACTED;
    });
  }
  return { text: out, kinds: [...kinds] };
}

/** Redacts every string inside a JSON value, keys of secret-like name included. */
export function redactDeep(value: unknown, kinds: Set<string>, depth = 0): unknown {
  if (depth > 32) return REDACTED;
  if (typeof value === "string") {
    const r = redactText(value);
    for (const k of r.kinds) kinds.add(k);
    return r.text;
  }
  if (Array.isArray(value)) return value.map((v) => redactDeep(v, kinds, depth + 1));
  if (typeof value === "object" && value !== null) {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value)) {
      if (SECRET_LIKE.test(k) && (typeof v === "string" || typeof v === "number") && String(v) !== "") {
        kinds.add("pair");
        out[k] = REDACTED;
      } else {
        out[k] = redactDeep(v, kinds, depth + 1);
      }
    }
    return out;
  }
  return value;
}
