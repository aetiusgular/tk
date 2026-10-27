// The policy file and the matcher, lane F's TypeScript reader of
// contracts/policy.md (sections 1 to 4). Tested against
// contracts/fixtures/policy/. Canonical copy: packages/tk-claude-plugin/src/core/.

import { createHash } from "node:crypto";
import { closeSync, fstatSync, openSync, readSync, realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { decodeEscapes, fold } from "./fold.ts";

export type Action = "allow" | "ask" | "deny";

export type Rule = {
  tool: string;
  harness?: string;
  chat?: string;
  action: Action;
  descriptionSha256?: string;
};

export type PolicyFile = {
  version: 1;
  askTimeoutSeconds: number;
  servers: string[];
  rules: Rule[];
};

export type LoadedPolicy = {
  policy: PolicyFile;
  /** Set when the file was missing or bad; the reason of a `no rule` answer names it. */
  problem?: string;
};

export type Call = {
  tool: string;
  harness: string;
  chat?: string;
  description?: string;
  inputText: string;
};

export type Decision = {
  action: Action;
  normalizedTool: string;
  reason: string;
};

export const DEFAULT_ASK_TIMEOUT_SECONDS = 60;
const MAX_POLICY_BYTES = 1024 * 1024;

export function emptyPolicy(): PolicyFile {
  return { version: 1, askTimeoutSeconds: DEFAULT_ASK_TIMEOUT_SECONDS, servers: [], rules: [] };
}

/** `$TK_POLICY` if set, else `~/.tk/policy.json` (contract, section 1). */
export function policyPath(env: NodeJS.ProcessEnv = process.env): string {
  const fromEnv = env.TK_POLICY;
  if (fromEnv && fromEnv.trim() !== "") return fromEnv;
  return path.join(homedir(), ".tk", "policy.json");
}

/** The real path of `p`, following links on every part that exists; a missing tail is appended as written. */
function realish(p: string): string {
  const abs = path.resolve(p);
  let head = abs;
  const tail: string[] = [];
  for (;;) {
    try {
      return path.join(realpathSync(head), ...tail.reverse());
    } catch {
      const parent = path.dirname(head);
      if (parent === head) return abs;
      tail.push(path.basename(head));
      head = parent;
    }
  }
}

/** True when `file` is `folder` or lies under it, after links are followed. */
export function insideFolder(file: string, folder: string): boolean {
  const rel = path.relative(realish(folder), realish(file));
  return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel));
}

/**
 * Loads the policy for a hook. A `TK_POLICY` that points inside the session
 * folder is refused: a project's settings file (Claude Code's
 * `.claude/settings.json` `env`) or a cloned repository can set it there, and
 * the file would then be the project's, not yours. The default
 * `~/.tk/policy.json` is never refused this way (a session may start in HOME).
 */
export function loadPolicyFrom(env: NodeJS.ProcessEnv = process.env, cwd: string = process.cwd()): LoadedPolicy {
  const fromEnv = env.TK_POLICY?.trim();
  if (fromEnv && path.isAbsolute(fromEnv) && insideFolder(fromEnv, cwd)) {
    return { policy: emptyPolicy(), problem: "TK_POLICY is inside the session folder, which a project can set; not used" };
  }
  return loadPolicy(policyPath(env));
}

const TOOL_PATTERN = /^(\*|[a-z0-9_]+\/(\*|[a-z0-9_]+)|[a-z0-9_]+)$/;
const HARNESS_PATTERN = /^[a-z0-9][a-z0-9-]{0,63}$/;
const SERVER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
const SHA_PATTERN = /^[0-9a-f]{64}$/;

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Validates parsed JSON against contracts/policy.schema.json by hand (the hook
 * has no dependencies). Returns the policy or the first problem found.
 */
export function parsePolicy(value: unknown): PolicyFile | string {
  if (!isPlainObject(value)) return "not an object";
  for (const key of Object.keys(value)) {
    if (!["version", "askTimeoutSeconds", "servers", "rules"].includes(key)) return `unknown field ${key}`;
  }
  if (value.version !== 1) return "version is not 1";
  let askTimeoutSeconds = DEFAULT_ASK_TIMEOUT_SECONDS;
  if (value.askTimeoutSeconds !== undefined) {
    const t = value.askTimeoutSeconds;
    if (typeof t !== "number" || !Number.isInteger(t) || t < 1 || t > 3600) return "askTimeoutSeconds out of range";
    askTimeoutSeconds = t;
  }
  const servers: string[] = [];
  if (value.servers !== undefined) {
    if (!Array.isArray(value.servers)) return "servers is not an array";
    for (const s of value.servers) {
      if (typeof s !== "string" || !SERVER_PATTERN.test(s)) return "bad server name";
      if (servers.includes(s)) return "duplicate server name";
      servers.push(s);
    }
  }
  const rules: Rule[] = [];
  if (value.rules !== undefined) {
    if (!Array.isArray(value.rules)) return "rules is not an array";
    if (value.rules.length > 1000) return "more than 1000 rules";
    for (const [i, r] of value.rules.entries()) {
      if (!isPlainObject(r)) return `rule ${i} is not an object`;
      for (const key of Object.keys(r)) {
        if (!["tool", "harness", "chat", "action", "descriptionSha256"].includes(key)) return `rule ${i}: unknown field ${key}`;
      }
      if (typeof r.tool !== "string" || !TOOL_PATTERN.test(r.tool)) return `rule ${i}: bad tool`;
      if (r.action !== "allow" && r.action !== "ask" && r.action !== "deny") return `rule ${i}: bad action`;
      const rule: Rule = { tool: r.tool, action: r.action };
      if (r.harness !== undefined) {
        if (typeof r.harness !== "string" || !HARNESS_PATTERN.test(r.harness)) return `rule ${i}: bad harness`;
        rule.harness = r.harness;
      }
      if (r.chat !== undefined) {
        if (typeof r.chat !== "string" || r.chat.length < 1 || r.chat.length > 200) return `rule ${i}: bad chat`;
        rule.chat = r.chat;
      }
      if (r.descriptionSha256 !== undefined) {
        if (typeof r.descriptionSha256 !== "string" || !SHA_PATTERN.test(r.descriptionSha256)) return `rule ${i}: bad descriptionSha256`;
        rule.descriptionSha256 = r.descriptionSha256;
      }
      rules.push(rule);
    }
  }
  return { version: 1, askTimeoutSeconds, servers, rules };
}

/**
 * Reads the policy file. Called on the first tool call, never at load (case
 * 46). Missing, unreadable, too large, not JSON, or failing the schema: the
 * empty policy, with the problem named (contract, section 1). Nothing fails open.
 */
export function loadPolicy(file: string = policyPath()): LoadedPolicy {
  // A relative path would resolve inside the project, which an agent can write.
  if (!path.isAbsolute(file)) return { policy: emptyPolicy(), problem: "policy path is not absolute" };
  let fd: number | undefined;
  try {
    fd = openSync(file, "r");
    const st = fstatSync(fd);
    if (!st.isFile()) return { policy: emptyPolicy(), problem: "policy file is not a regular file" };
    if (typeof process.getuid === "function" && st.uid !== process.getuid()) return { policy: emptyPolicy(), problem: "policy file owned by another user" };
    if ((st.mode & 0o022) !== 0) return { policy: emptyPolicy(), problem: "policy file writable by group or others" };
    // A folder others can write (such as /tmp) lets them swap the file.
    const dir = statSync(path.dirname(file));
    if ((dir.mode & 0o022) !== 0) return { policy: emptyPolicy(), problem: "policy folder writable by group or others" };
    if (st.size > MAX_POLICY_BYTES) return { policy: emptyPolicy(), problem: "policy file too large" };
    const buf = Buffer.alloc(st.size);
    let read = 0;
    while (read < st.size) {
      const n = readSync(fd, buf, read, st.size - read, read);
      if (n === 0) break;
      read += n;
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(buf.subarray(0, read).toString("utf8"));
    } catch {
      return { policy: emptyPolicy(), problem: "policy file is not JSON" };
    }
    const result = parsePolicy(parsed);
    if (typeof result === "string") return { policy: emptyPolicy(), problem: `policy file invalid: ${result}` };
    return { policy: result };
  } catch (error) {
    const code = (error as NodeJS.ErrnoException)?.code;
    return { policy: emptyPolicy(), problem: code === "ENOENT" ? "no policy file" : "policy file unreadable" };
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

// ---------------------------------------------------------------- names

function stripPlugin(server: string): string {
  if (server.startsWith("plugin_")) {
    const rest = server.slice("plugin_".length);
    const i = rest.indexOf("_");
    if (i >= 0 && i + 1 < rest.length) return rest.slice(i + 1);
  }
  return server;
}

function join(server: string, tool: string): string {
  return server === "" ? tool : `${stripPlugin(server)}/${tool}`;
}

function splitDoubleThenSingle(rest: string): [string, string] {
  const d = rest.indexOf("__");
  if (d >= 0) return [rest.slice(0, d), rest.slice(d + 2)];
  const s = rest.indexOf("_");
  if (s >= 0) return [rest.slice(0, s), rest.slice(s + 1)];
  return ["", rest];
}

/** contracts/policy.md section 2, the same steps as tk-core's normalize_tool_name. */
export function normalizeToolName(raw: string, servers: readonly string[]): string {
  let lowered = "";
  for (const ch of raw.trim()) {
    const c = ch.length === 1 && ch <= "\x7f" ? ch.toLowerCase() : ch;
    lowered += /^[a-z0-9_/]$/.test(c) ? c : "_";
  }
  const slash = lowered.indexOf("/");
  if (slash >= 0) return `${stripPlugin(lowered.slice(0, slash))}/${lowered.slice(slash + 1)}`;
  if (lowered.startsWith("mcp__")) return join(...splitDoubleThenSingle(lowered.slice(5)));
  if (lowered.startsWith("mcp_")) return join(...splitDoubleThenSingle(lowered.slice(4)));
  const dd = lowered.indexOf("__");
  if (dd >= 0) return join(lowered.slice(0, dd), lowered.slice(dd + 2));
  let best: string | undefined;
  for (const s0 of servers) {
    const s = s0.toLowerCase();
    if (lowered.length > s.length + 1 && lowered.startsWith(s) && lowered[s.length] === "_" && (best === undefined || s.length > best.length)) {
      best = s;
    }
  }
  if (best !== undefined) return join(best, lowered.slice(best.length + 1));
  return lowered;
}

// ---------------------------------------------------------------- reject list

export const ENDPOINT_KEYS = [
  "anthropic_base_url",
  "chatgpt_base_url",
  "dangerouslydisablesandbox",
  "approval_policy",
  "--dangerously-",
  "settings.local.json",
] as const;

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

// `powershell` is not in the contract's list; lane A added it (Pi 1.4.0 has the tool). A request for integration.
export const SHELL_TOOLS = ["bash", "shell", "exec_command", "local_shell", "run_shell_command", "user_bash", "terminal", "powershell"] as const;

/**
 * Files that control tk or a harness's hooks. An `allow` whose folded input
 * names one becomes `ask`, so an allowed tool cannot quietly rewrite the
 * policy or switch the gate off (for example `disableAllHooks` in Claude
 * Code's settings, or the plugin entry in Codex's config). Lane A's list
 * plus each harness's own files. Narrowing only; not in the contract.
 */
export const CONTROL_PATHS = [
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
] as const;

export const UNREADABLE_SHELL_TOKENS = ["$", "`", "!", "{", "}", "~", "(e:", "(+", "(-", "\\"] as const;

const GIT_WORD = /(?<![a-z0-9_])git(?![a-z0-9_])/g;

/** contracts/policy.md section 3.2: is folded text a git context? */
export function isGitContext(folded: string): boolean {
  if (GIT_CONTEXT_PATHS.some((p) => folded.includes(p))) return true;
  for (const line of folded.split(/\r\n|\n|\r/)) {
    GIT_WORD.lastIndex = 0;
    const m = GIT_WORD.exec(line);
    if (!m) continue;
    const after = line.slice(m.index + 3);
    if (after.includes("-c") || after.includes("config") || after.includes("--config-env")) return true;
  }
  return false;
}

/** The first reject-list key the text holds, or undefined. */
export function rejectKey(inputText: string): string | undefined {
  const folded = fold(decodeEscapes(inputText));
  for (const k of ENDPOINT_KEYS) if (folded.includes(k)) return k;
  if (isGitContext(folded)) for (const k of GIT_CONFIG_KEYS) if (folded.includes(k)) return k;
  return undefined;
}

// ---------------------------------------------------------------- matching

function rankOf(rule: Rule): [number, number, number] {
  const tool = rule.tool === "*" ? 0 : rule.tool.endsWith("/*") ? 1 : 2;
  return [rule.chat !== undefined ? 1 : 0, rule.harness !== undefined ? 1 : 0, tool];
}

function compareRank(a: [number, number, number], b: [number, number, number]): number {
  for (let i = 0; i < 3; i++) {
    const d = (a[i] ?? 0) - (b[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

const SEVERITY: Record<Action, number> = { allow: 0, ask: 1, deny: 2 };

function toolMatches(pattern: string, normalized: string): boolean {
  if (pattern === "*") return true;
  if (pattern === normalized) return true;
  if (pattern.endsWith("/*")) {
    const slash = normalized.indexOf("/");
    return slash >= 0 && normalized.slice(0, slash) === pattern.slice(0, -2);
  }
  return false;
}

export function isShellTool(normalized: string): boolean {
  return (SHELL_TOOLS as readonly string[]).includes(normalized);
}

export function sha256Hex(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

/** contracts/policy.md section 4, steps 1 to 7. Step 8 (the wait) is the caller's. */
export function evaluate(loaded: LoadedPolicy, call: Call): Decision {
  const { policy } = loaded;
  const normalizedTool = normalizeToolName(call.tool, policy.servers);
  const key = rejectKey(call.inputText);
  if (key !== undefined) return { action: "deny", normalizedTool, reason: `reject list: ${key}` };

  const matches = policy.rules.filter(
    (r) =>
      toolMatches(r.tool, normalizedTool) &&
      (r.harness === undefined || r.harness === call.harness) &&
      (r.chat === undefined || r.chat === call.chat),
  );
  if (matches.length === 0) {
    return { action: "ask", normalizedTool, reason: loaded.problem ? `no rule (${loaded.problem})` : "no rule" };
  }
  let top = matches[0] as Rule;
  for (const r of matches.slice(1)) {
    const c = compareRank(rankOf(r), rankOf(top));
    if (c > 0 || (c === 0 && SEVERITY[r.action] > SEVERITY[top.action])) top = r;
  }
  const reason = `rule ${top.tool}${top.harness ? ` harness=${top.harness}` : ""}${top.chat ? " chat" : ""}`;
  if (top.action === "allow" && top.descriptionSha256 !== undefined) {
    if (call.description === undefined || sha256Hex(call.description) !== top.descriptionSha256) {
      return { action: "ask", normalizedTool, reason: "description changed" };
    }
  }
  if (top.action === "allow") {
    const folded = fold(decodeEscapes(call.inputText));
    const control = CONTROL_PATHS.find((p) => folded.includes(p));
    if (control !== undefined) return { action: "ask", normalizedTool, reason: `input names a control file (${control})` };
  }
  if (top.action === "allow" && isShellTool(normalizedTool)) {
    if (UNREADABLE_SHELL_TOKENS.some((t) => call.inputText.includes(t))) {
      return { action: "ask", normalizedTool, reason: "shell text the matcher cannot read" };
    }
  }
  return { action: top.action, normalizedTool, reason };
}
