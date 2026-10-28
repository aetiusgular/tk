// The policy file and the matcher, in TypeScript. Contract:
// contracts/policy.md; schema: contracts/policy.schema.json; fixture:
// contracts/fixtures/policy/. The Rust copy is crates/tk-core/src/policy.rs
// (lane B); both answer the same 21 fixture cases.
//
// tk-pi additions beyond the contract, each one only narrowing (allow to ask,
// or a match that leads to deny), recorded in the lane A status doc:
// - `powershell` is a shell tool (Pi 1.4.0 has a `powershell` tool:
//   types.d.ts:946-949);
// - an `allow` whose input names a tk or Pi control file (the policy file,
//   the daemon's token, Pi's settings and extension folders) becomes `ask`;
// - the policy file is refused when another user can write it.

import { createHash } from "node:crypto";
import { decodeEscapes, foldText } from "./fold.ts";

export type Action = "allow" | "ask" | "deny";

export interface Rule {
  tool: string;
  harness?: string;
  chat?: string;
  action: Action;
  descriptionSha256?: string;
}

export interface PolicyFile {
  version: 1;
  askTimeoutSeconds: number;
  servers: string[];
  rules: Rule[];
}

export interface LoadedPolicy {
  policy: PolicyFile;
  /** Why the file was not used, when it was not. Shown in `no rule` reasons. */
  problem?: string;
}

export interface Call {
  /** The name as the harness gave it. */
  tool: string;
  harness: string;
  chat?: string;
  /** The tool's description now, if known. A function so it is read only when a pin needs it. */
  description?: string | (() => string | undefined);
  /** The text the reject list runs over. */
  inputText: string;
}

export interface Decision {
  action: Action;
  normalizedTool: string;
  reason: string;
}

export const DEFAULT_ASK_TIMEOUT_SECONDS = 60;

export function defaultPolicy(): PolicyFile {
  return { version: 1, askTimeoutSeconds: DEFAULT_ASK_TIMEOUT_SECONDS, servers: [], rules: [] };
}

// ------------------------------------------------------------ validation

const TOOL_PATTERN = /^(\*|[a-z0-9_]+\/(\*|[a-z0-9_]+)|[a-z0-9_]+)$/;
const HARNESS_PATTERN = /^[a-z0-9][a-z0-9-]{0,63}$/;
const SERVER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const RULE_KEYS = new Set(["tool", "harness", "chat", "action", "descriptionSha256"]);
const FILE_KEYS = new Set(["version", "askTimeoutSeconds", "servers", "rules"]);

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Parses and checks a policy file's text against the schema. Returns the
 * default policy and a problem on any failure: nothing fails open.
 */
export function parsePolicy(text: string): LoadedPolicy {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return { policy: defaultPolicy(), problem: "policy file is not JSON" };
  }
  const problem = schemaProblem(raw);
  if (problem) return { policy: defaultPolicy(), problem: `policy file refused: ${problem}` };
  const obj = raw as Record<string, unknown>;
  return {
    policy: {
      version: 1,
      askTimeoutSeconds: (obj.askTimeoutSeconds as number | undefined) ?? DEFAULT_ASK_TIMEOUT_SECONDS,
      servers: (obj.servers as string[] | undefined) ?? [],
      rules: ((obj.rules as Rule[] | undefined) ?? []).map((r) => ({ ...r })),
    },
  };
}

function schemaProblem(raw: unknown): string | undefined {
  if (!isObject(raw)) return "not an object";
  for (const k of Object.keys(raw)) if (!FILE_KEYS.has(k)) return `unknown field ${JSON.stringify(k).slice(0, 40)}`;
  if (raw.version !== 1) return "version must be 1";
  if (raw.askTimeoutSeconds !== undefined) {
    const t = raw.askTimeoutSeconds;
    if (typeof t !== "number" || !Number.isInteger(t) || t < 1 || t > 3600) return "askTimeoutSeconds must be 1 to 3600";
  }
  if (raw.servers !== undefined) {
    if (!Array.isArray(raw.servers)) return "servers must be an array";
    const seen = new Set<string>();
    for (const s of raw.servers) {
      if (typeof s !== "string" || !SERVER_PATTERN.test(s)) return "bad server name";
      if (seen.has(s)) return "duplicate server name";
      seen.add(s);
    }
  }
  if (raw.rules !== undefined) {
    if (!Array.isArray(raw.rules)) return "rules must be an array";
    if (raw.rules.length > 1000) return "more than 1000 rules";
    for (const [i, r] of raw.rules.entries()) {
      if (!isObject(r)) return `rule ${i} is not an object`;
      for (const k of Object.keys(r)) if (!RULE_KEYS.has(k)) return `rule ${i} has an unknown field`;
      if (typeof r.tool !== "string" || !TOOL_PATTERN.test(r.tool)) return `rule ${i} has a bad tool pattern`;
      if (r.action !== "allow" && r.action !== "ask" && r.action !== "deny") return `rule ${i} has a bad action`;
      if (r.harness !== undefined && (typeof r.harness !== "string" || !HARNESS_PATTERN.test(r.harness)))
        return `rule ${i} has a bad harness`;
      if (r.chat !== undefined && (typeof r.chat !== "string" || r.chat.length < 1 || r.chat.length > 200))
        return `rule ${i} has a bad chat`;
      if (
        r.descriptionSha256 !== undefined &&
        (typeof r.descriptionSha256 !== "string" || !SHA256_PATTERN.test(r.descriptionSha256))
      )
        return `rule ${i} has a bad descriptionSha256`;
    }
  }
  return undefined;
}

// --------------------------------------------------------- normalization

/** contracts/policy.md, section 2. Mirrors `normalize_tool_name` in tk-core. */
/** Step 1: trim, ASCII-lowercase, every character outside `[a-z0-9_/]` to `_`. Also applied to each `servers` entry (integration w1). */
export function lowerName(raw: string): string {
  let lowered = "";
  for (const ch of raw.trim()) {
    const c = ch.length === 1 && ch.charCodeAt(0) < 0x80 ? ch.toLowerCase() : "_";
    lowered += /[a-z0-9_/]/.test(c) ? c : "_";
  }
  return lowered;
}

/**
 * contracts/policy.md section 2, the same steps as the hook core and
 * tk-core. `harness` is the agent's id, for the per-agent name forms later
 * waves add from preset rows (note AQ section 6); unused in wave 1.
 */
export function normalizeToolName(raw: string, servers: readonly string[], harness?: string): string {
  void harness;
  const lowered = lowerName(raw);
  const slash = lowered.indexOf("/");
  if (slash >= 0) return `${stripPlugin(lowered.slice(0, slash))}/${lowered.slice(slash + 1)}`;
  if (lowered.startsWith("mcp__")) return join(splitDoubleThenSingle(lowered.slice(5)));
  if (lowered.startsWith("mcp_")) return join(splitDoubleThenSingle(lowered.slice(4)));
  const run = splitAtRun(lowered);
  if (run) return join(run);
  let best: string | undefined;
  for (const s of servers.map(lowerName)) {
    if (lowered.length > s.length + 1 && lowered.startsWith(s) && lowered[s.length] === "_") {
      if (best === undefined || s.length > best.length) best = s;
    }
  }
  if (best !== undefined) return join([best, lowered.slice(best.length + 1)]);
  return lowered;
}

/** Splits at the first run of two or more `_`, the whole run consumed (Droid's `s___t`; note AQ section 6). */
function splitAtRun(s: string): [string, string] | undefined {
  const d = s.indexOf("__");
  if (d < 0) return undefined;
  let e = d;
  while (e < s.length && s[e] === "_") e++;
  return [s.slice(0, d), s.slice(e)];
}

function splitDoubleThenSingle(rest: string): [string, string] {
  const run = splitAtRun(rest);
  if (run) return run;
  const s = rest.indexOf("_");
  if (s >= 0) return [rest.slice(0, s), rest.slice(s + 1)];
  return ["", rest];
}

function join([server, tool]: [string, string]): string {
  return server === "" ? tool : `${stripPlugin(server)}/${tool}`;
}

function stripPlugin(server: string): string {
  if (server.startsWith("plugin_")) {
    const rest = server.slice(7);
    const u = rest.indexOf("_");
    if (u >= 0 && u + 1 < rest.length) return rest.slice(u + 1);
  }
  return server;
}

// ------------------------------------------------------------ reject list

/** contracts/policy.md, section 3.1. Every tool. */
export const ENDPOINT_KEYS = [
  "anthropic_base_url",
  "chatgpt_base_url",
  "dangerouslydisablesandbox",
  "approval_policy",
  "--dangerously-",
  "settings.local.json",
] as const;

/** contracts/policy.md, section 3.2. Only in a git context. */
export const GIT_CONFIG_KEYS = [
  "core.fsmonitor",
  "core.hookspath",
  "core.sshcommand",
  "core.pager",
  "core.editor",
  "core.askpass",
  "core.gitproxy",
  "core.attributesfile",
  "sequence.editor",
  "pager.",
  "alias.",
  "gpg.program",
  "gpg.ssh.program",
  "credential.helper",
  "include.path",
  "includeif.",
  "filter.",
  ".driver",
  ".textconv",
  "diff.external",
  "uploadpack",
  "receivepack",
  ".insteadof",
  ".pushinsteadof",
  "protocol.",
  "ext::",
  "fd::",
  "git_allow_protocol",
  "git_ssh",
  "git_proxy_command",
  "git_askpass",
  "git_external_diff",
  "git_config_parameters",
  "git_config_count",
] as const;

export const GIT_CONTEXT_PATHS = [".git/", ".gitmodules", ".gitattributes", ".gitconfig", "git_config"] as const;

/** contracts/policy.md, section 4, step 7, plus Pi's `powershell`. */
export const SHELL_TOOLS = new Set([
  "bash",
  "shell",
  "exec_command",
  "local_shell",
  "run_shell_command",
  "user_bash",
  "terminal",
  "powershell",
]);

/**
 * Shell text the matcher cannot read; `*`, `?` and `[` are globs that name
 * files the matcher never sees (integration w1, security finding S1).
 */
export const UNREADABLE_SHELL_TOKENS = ["$", "`", "!", "{", "}", "~", "(e:", "(+", "(-", "\\", "*", "?", "["] as const;

/**
 * The text with every quote removed: the shell joins `ANTHROPIC_"BASE_URL"`
 * and `g''it` into one word, so the reject list and the control files are
 * also read this way (integration w1, finding S1). Only adds matches.
 */
export function unquote(text: string): string {
  return text.replace(/["']/g, "");
}

/**
 * Files that control tk or a harness's hooks: an `allow` whose folded input
 * names one becomes `ask` (contracts/policy.md section 4, step 7a). The same
 * list as the hook core's `CONTROL_PATHS` (integration w1; lane A's list plus
 * each harness's own files). Matched on folded text.
 */
export const CONTROL_PATH_MARKERS = [
  ".tk/",
  "tk/daemon.",
  "tk/otlp.endpoint",
  "tk_policy",
  "tk_chat_token",
  ".pi/agent/",
  ".claude/settings",
  ".claude.json",
  ".claude/plugins",
  "disableallhooks",
  "managed-settings",
  ".codex/config.toml",
  ".codex/hooks.json",
  ".codex/plugins",
  "requirements.toml",
  "opencode.json",
  ".config/opencode/",
  ".cache/opencode/",
  // Git's config and hooks name programs git runs later (rules cases 27, 31;
  // integration w1, finding S4).
  ".git/config",
  ".git/hooks/",
  ".gitconfig",
  ".config/git/",
  ".gitmodules",
  // A harness's whole project or home folder (integration w1, finding S7).
  ".claude/",
  ".codex/",
  ".pi/",
  // Wave 2 lane I, tk-pi only until integration adds them to the hook core
  // and tk-core: oh-my-pi's config root (~/.omp: agent/config.yml holds
  // tools.approvalMode and the extensions list, plugins/omp-plugins.lock.json
  // can switch this package off; dirs.ts at 18.13.3), the variables that move
  // Pi's and omp's folders, and the gate's own freeze switch.
  ".omp/",
  "pi_coding_agent_dir",
  "pi_config_dir",
  "tk_pi_freeze_input",
] as const;

const GIT_WORD = /(^|[^a-z0-9_])git(?![a-z0-9_])/g;

/** True when folded text is a git context (contract, section 3.2). */
export function isGitContext(folded: string): boolean {
  if (GIT_CONTEXT_PATHS.some((p) => folded.includes(p))) return true;
  // A line that ends in `\` continues on the next, as the shell reads it
  // (tk-core's `git_context`; integration w1, finding S10).
  for (const line of folded.replace(/\\\r?\n/g, " ").split(/\r\n|\n|\r/)) {
    GIT_WORD.lastIndex = 0;
    const m = GIT_WORD.exec(line);
    if (!m) continue;
    const after = line.slice(m.index + m[0].length);
    if (after.includes("-c") || after.includes("config")) return true;
  }
  return false;
}

/** The first reject key the text hits, or undefined. Runs `foldText` itself. */
export function rejectKey(inputText: string): string | undefined {
  return rejectKeyFolded(foldText(decodeEscapes(inputText)));
}

function rejectKeyFolded(folded: string): string | undefined {
  for (const text of [folded, unquote(folded)]) {
    for (const k of ENDPOINT_KEYS) if (text.includes(k)) return k;
    if (isGitContext(text)) for (const k of GIT_CONFIG_KEYS) if (text.includes(k)) return k;
  }
  return undefined;
}

// ---------------------------------------------------------------- matching

function specificity(pattern: string, normalized: string): number {
  if (pattern === normalized) return 2;
  if (pattern === "*") return 0;
  if (pattern.endsWith("/*")) {
    const slash = normalized.indexOf("/");
    if (slash >= 0 && pattern.slice(0, -2) === normalized.slice(0, slash)) return 1;
  }
  return -1;
}

const RESTRICTIVENESS: Record<Action, number> = { allow: 0, ask: 1, deny: 2 };

export function sha256Hex(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

function describeRule(r: Rule): string {
  let s = `rule ${r.tool}`;
  if (r.harness) s += ` harness=${r.harness}`;
  if (r.chat) s += ` chat=${r.chat.slice(0, 40)}`;
  return `${s}: ${r.action}`;
}

/**
 * contracts/policy.md, section 4. `problem` is the loader's reason when the
 * file was not used; it goes into the `no rule` reason. The reason never
 * holds the input text.
 */
export function evaluate(policy: PolicyFile, call: Call, problem?: string): Decision {
  const normalizedTool = normalizeToolName(call.tool, policy.servers, call.harness);
  // JSON and TOML escapes decoded before folding (section 3; integration w1).
  const folded = foldText(decodeEscapes(call.inputText));
  const hit = rejectKeyFolded(folded);
  if (hit !== undefined) return { action: "deny", normalizedTool, reason: `reject list: ${hit}` };

  let best: { rank: [number, number, number]; rules: Rule[] } | undefined;
  for (const r of policy.rules) {
    const spec = specificity(r.tool, normalizedTool);
    if (spec < 0) continue;
    if (r.harness !== undefined && r.harness !== call.harness) continue;
    if (r.chat !== undefined && r.chat !== call.chat) continue;
    const rank: [number, number, number] = [r.chat !== undefined ? 1 : 0, r.harness !== undefined ? 1 : 0, spec];
    if (!best || compareRank(rank, best.rank) > 0) best = { rank, rules: [r] };
    else if (compareRank(rank, best.rank) === 0) best.rules.push(r);
  }
  if (!best) {
    return { action: "ask", normalizedTool, reason: problem ? `no rule (${problem})` : "no rule" };
  }
  let winner = best.rules[0] as Rule;
  for (const r of best.rules) if (RESTRICTIVENESS[r.action] > RESTRICTIVENESS[winner.action]) winner = r;

  if (winner.action !== "allow") return { action: winner.action, normalizedTool, reason: describeRule(winner) };

  // Every tied allow must hold its pin: one changed description is enough to
  // ask (case 57; tk-core's rule, integration w1 finding S10).
  const pins = best.rules.filter((r) => r.descriptionSha256 !== undefined).map((r) => (r.descriptionSha256 as string).toLowerCase());
  if (pins.length > 0) {
    const description = typeof call.description === "function" ? call.description() : call.description;
    const current = description === undefined ? undefined : sha256Hex(description);
    if (current === undefined || pins.some((p) => p !== current)) {
      return { action: "ask", normalizedTool, reason: "description changed since it was approved" };
    }
  }
  // Control files before shell text (section 4, step 7; the hook core's order).
  const unquoted = unquote(folded);
  const control = CONTROL_PATH_MARKERS.find((m) => folded.includes(m) || unquoted.includes(m));
  if (control !== undefined) {
    return { action: "ask", normalizedTool, reason: `input names a control file (${control})` };
  }
  // On the folded text, so `＄HOME` asks (integration w1; the hook core and tk-core do the same).
  const token = SHELL_TOOLS.has(normalizedTool) ? UNREADABLE_SHELL_TOKENS.find((t) => folded.includes(t)) : undefined;
  if (token !== undefined) {
    // The token named, a glob called one (Mac test w1, M9; the hook core's text).
    const glob = token === "*" || token === "?" || token === "[";
    return { action: "ask", normalizedTool, reason: `shell text the matcher cannot read: \`${token}\`${glob ? " (a glob)" : ""}` };
  }
  return { action: "allow", normalizedTool, reason: describeRule(winner) };
}

function compareRank(a: [number, number, number], b: [number, number, number]): number {
  for (let i = 0; i < 3; i++) {
    const d = (a[i] as number) - (b[i] as number);
    if (d !== 0) return d;
  }
  return 0;
}
