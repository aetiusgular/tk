// Best-effort redaction of secret-like values (contracts/docker-args.md,
// section 2). It finds values whose key looks like a secret and a few
// well-known token shapes. It cannot find an arbitrary secret in text and is
// never presented as doing so. Canonical copy: packages/tk-claude-plugin/src/core/.

/** The fork's property-name pattern (dockerArgs.ts:38 at 159ef3e). */
export const SECRET_LIKE = /token|secret|password|passwd|apikey|api_key|private_key/i;

export const REDACTED = "[redacted by tk]";

/**
 * Token shapes with a fixed, documented prefix. Sources are listed in
 * plans/parallel/tk_lane_F.md, Research. Each is matched as a whole word-ish
 * run so ordinary text is left alone.
 */
export const TOKEN_SHAPES: readonly RegExp[] = [
  /\bgh[pousr]_[A-Za-z0-9]{36,255}\b/g, // GitHub classic and app tokens
  /\bgithub_pat_[A-Za-z0-9_]{22,255}\b/g, // GitHub fine-grained personal access tokens
  /\b(?:AKIA|ASIA)[A-Z0-9]{16}\b/g, // AWS access key ids
  /\bsk-ant-[A-Za-z0-9_-]{20,}\b/g, // Anthropic API keys
  /\bxox[abposr]-[A-Za-z0-9-]{10,}\b/g, // Slack tokens
  /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----/g, // PEM private keys (RFC 7468)
];

const KEY = String.raw`[A-Za-z0-9_.-]*(?:token|secret|password|passwd|apikey|api_key|private_key)[A-Za-z0-9_.-]*`;

// "key": "value"  (JSON, also with single quotes)
const QUOTED_PAIR = new RegExp(String.raw`(["']${KEY}["']\s*:\s*)(["'])((?:\\.|(?!\2)[^\\])*)\2`, "gi");
// key=value, key: value, --key value-free forms; the value runs to whitespace, &, ;, quote or end.
const BARE_PAIR = new RegExp(String.raw`(\b${KEY}\s*[=:]\s*)(?!\[redacted)([^\s&;"',}]+)`, "gi");

export type Redaction = { text: string; count: number };

/** Redacts secret-like values in one string. */
export function redactText(text: string): Redaction {
  let count = 0;
  let out = text.replace(QUOTED_PAIR, (_m, head: string, q: string, value: string) => {
    if (value === "" || value === REDACTED) return `${head}${q}${value}${q}`;
    count++;
    return `${head}${q}${REDACTED}${q}`;
  });
  out = out.replace(BARE_PAIR, (m, head: string, value: string) => {
    // `max_tokens=1000` is a count, not a secret.
    if (/^[0-9]+$/.test(value)) return m;
    count++;
    return `${head}${REDACTED}`;
  });
  for (const shape of TOKEN_SHAPES) {
    out = out.replace(shape, () => {
      count++;
      return REDACTED;
    });
  }
  return { text: out, count };
}

/**
 * Redacts every string inside a JSON value. An object key that is
 * secret-like has its string value replaced whole. Returns a new value.
 */
export function redactValue(value: unknown): { value: unknown; count: number } {
  let count = 0;
  const walk = (v: unknown, keyIsSecret: boolean): unknown => {
    if (typeof v === "string") {
      if (keyIsSecret && v !== "" && v !== REDACTED) {
        count++;
        return REDACTED;
      }
      const r = redactText(v);
      count += r.count;
      return r.text;
    }
    if (Array.isArray(v)) return v.map((x) => walk(x, keyIsSecret));
    if (typeof v === "object" && v !== null) {
      const out: Record<string, unknown> = {};
      for (const [k, x] of Object.entries(v)) out[k] = walk(x, SECRET_LIKE.test(k));
      return out;
    }
    return v;
  };
  const out = walk(value, false);
  return { value: out, count };
}
