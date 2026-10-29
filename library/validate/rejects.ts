// Publish-time rejects (lane E, T3.1; plan T3.3 and section 9 item 4;
// note AP section 8.5; contracts/policy.md section 3.1).
//
// The folding is the policy contract's, the gate's own function (below).

/** Fold text the way the policy contract's reject list does: the gate's own
 *  fold (NFKD, marks dropped, invisible and format characters dropped, every
 *  Unicode space to one ASCII space, fullwidth ASCII to ASCII, the whole of
 *  confusables.txt 17.0.0, then lowercase), by import from the hook core
 *  through library/trust/fold.ts (lane H's request, integration w2). Lane E's
 *  hand-picked subset is gone. */
import { fold } from "../trust/fold.ts";
export { fold };

/** contracts/policy.md section 3.1: endpoint, sandbox and approval keys the
 *  2025 and 2026 incidents wrote (note AP section 5). Any entry whose manifest,
 *  hooks or skill text names one is refused at publish and at install. */
export const ENDPOINT_AND_SANDBOX_KEYS = [
  "anthropic_base_url",
  "chatgpt_base_url",
  "dangerouslydisablesandbox",
  "approval_policy",
  "--dangerously-",
  "settings.local.json",
] as const;

/** Install-time execution: the mechanism of postmark-mcp, ClawHavoc and
 *  mcp-server-sequential-thinking (note AP section 5). Matched on folded text. */
const INSTALL_TIME_EXECUTION: { key: string; re: RegExp }[] = [
  { key: "postinstall", re: /\b(pre|post)install\b/ },
  { key: "prepare script", re: /"prepare"\s*:/ },
  { key: "prerequisites: download", re: /prerequisite[s]?[^\n]{0,80}\b(download|install|curl|wget|run)\b/ },
  { key: "pipe to shell", re: /\b(curl|wget)\b[^\n|]{0,200}\|\s*(sudo\s+)?(sh|bash|zsh|python[23]?|node|powershell)\b/ },
  { key: "powershell download", re: /invoke-webrequest|downloadstring|iwr\s+-uri|iex\s*\(/ },
  { key: "base64 decode and run", re: /base64\s+(-d|--decode)[^\n]{0,80}\|\s*(sh|bash|python[23]?|node)\b/ },
];

/** Remote-code loading at runtime: Chrome MV3's rule, carried by note AP section 8.5. */
const REMOTE_CODE_LOADING: { key: string; re: RegExp }[] = [
  { key: "eval", re: /\beval\s*\(/ },
  { key: "new Function", re: /\bnew\s+function\s*\(/ },
  { key: "dynamic import over the network", re: /\bimport\s*\(\s*["'`]?https?:/ },
  { key: "fetch then eval", re: /\bfetch\s*\([^\n]{0,200}\)\s*[^\n]{0,120}\b(eval|new\s+function)\b/ },
  { key: "vm.runIn", re: /\bvm\.run(in(this|new)?context|inthiscontext)\b/ },
];

export type Reject = { key: string; reason: string };

/** Scan the folded text of a manifest for the reject list. Returns one hit per key. */
export function scanRejects(text: string): Reject[] {
  const folded = fold(text);
  const hits: Reject[] = [];
  for (const key of ENDPOINT_AND_SANDBOX_KEYS) {
    if (folded.includes(key)) hits.push({ key, reason: `reject list: ${key}` });
  }
  for (const { key, re } of INSTALL_TIME_EXECUTION) {
    if (re.test(folded)) hits.push({ key, reason: `install-time execution: ${key}` });
  }
  for (const { key, re } of REMOTE_CODE_LOADING) {
    if (re.test(folded)) hits.push({ key, reason: `remote code loading: ${key}` });
  }
  return hits;
}

/** contracts/docker-args.md section 2: a property name that must be a secret, never a config value. */
export const SECRET_LIKE = /token|secret|password|passwd|apikey|api_key|private_key/i;

/** A value that is a `${VAR}` reference rather than a literal. */
export function isEnvReference(value: string): boolean {
  return /^\$\{[A-Za-z_][A-Za-z0-9_]*\}$/.test(value.trim());
}
