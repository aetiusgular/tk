// Publish-time rejects (lane E, T3.1; plan T3.3 and section 9 item 4;
// note AP section 8.5; contracts/policy.md section 3.1).
//
// The folding is the one the policy contract specifies for case 30 and case
// 33: lowercase, drop invisible and format characters, map Unicode spaces to
// one ASCII space, map fullwidth ASCII to ASCII, map confusable letters from
// other scripts to their Latin look-alike. The confusable table here is the
// small Latin-target subset the library needs for its key list and brand
// names; it is not the whole of Unicode TR39 confusables.txt, which lanes A
// and B carry for the policy matcher (see the status doc, Research).

const INVISIBLE = /[\u00AD\u034F\u200B-\u200F\u2060-\u2064\uFEFF\u180E]/g;
const SPACES = /[\u0085\u00A0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000]/g;

/** Confusable single characters with a Latin or ASCII target. Sources:
 *  Unicode TR39 confusables.txt, the Cyrillic, Greek and Armenian rows whose
 *  target is a basic Latin letter, plus the ASCII digit look-alikes. */
const CONFUSABLES: Record<string, string> = {
  "\u0430": "a", "\u0435": "e", "\u043E": "o", "\u0440": "p", "\u0441": "c", "\u0443": "y", "\u0445": "x", "\u0456": "i", "\u0458": "j", "\u04BB": "h", "\u0501": "d", "\u051B": "q", "\u051D": "w", "\u0455": "s", "\u04CF": "l",
  "\u0391": "a", "\u0392": "b", "\u0395": "e", "\u0396": "z", "\u0397": "h", "\u0399": "i", "\u039A": "k", "\u039C": "m", "\u039D": "n", "\u039F": "o", "\u03A1": "p", "\u03A4": "t", "\u03A5": "y", "\u03A7": "x",
  "\u03B1": "a", "\u03BF": "o", "\u03BD": "v", "\u03C1": "p", "\u03C5": "u", "\u03B9": "i", "\u03BA": "k", "\u03C3": "o",
  "\u0410": "a", "\u0412": "b", "\u0415": "e", "\u041A": "k", "\u041C": "m", "\u041D": "h", "\u041E": "o", "\u0420": "p", "\u0421": "c", "\u0422": "t", "\u0425": "x", "\u0406": "i", "\u0408": "j", "\u0405": "s",
  "\u0131": "i", "\u0237": "j", "\u00F0": "d", "\u2113": "l", "\u212A": "k",
};

/** Fold text the way the policy contract's reject list does. */
export function fold(text: string): string {
  let s = text.normalize("NFKC").toLowerCase().replace(INVISIBLE, "").replace(SPACES, " ");
  // Fullwidth ASCII (U+FF01 to U+FF5E) to ASCII.
  s = s.replace(/[\uFF01-\uFF5E]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xfee0));
  let out = "";
  for (const ch of s) out += CONFUSABLES[ch] ?? ch;
  return out;
}

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
