import { chmodSync, mkdtempSync, rmSync, symlinkSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { describe, expect, it } from "vitest";
import policyFixture from "../../../../contracts/fixtures/policy/policy.json" with { type: "json" };
import casesFixture from "../../../../contracts/fixtures/policy/cases.json" with { type: "json" };
import { CONFUSABLES_COUNT } from "./confusables.ts";
import { decodeEscapes, fold } from "./fold.ts";
import {
  ENDPOINT_KEYS,
  GIT_CONFIG_KEYS,
  UNREADABLE_SHELL_TOKENS,
  evaluate,
  isGitContext,
  loadPolicy,
  normalizeToolName,
  parsePolicy,
  rejectKey,
  type Call,
  type LoadedPolicy,
} from "./policy.ts";

const parsed = parsePolicy(policyFixture);
if (typeof parsed === "string") throw new Error(`fixture policy invalid: ${parsed}`);
const loaded: LoadedPolicy = { policy: parsed };

describe("contracts/fixtures/policy: every case", () => {
  for (const c of casesFixture.cases) {
    it(`case ${c.id}`, () => {
      const call = c.call as Call;
      const d = evaluate(loaded, call);
      expect(d.action).toBe(c.expect.action);
      expect(d.normalizedTool).toBe(c.expect.normalizedTool);
      expect(d.reason.startsWith(c.expect.reasonPrefix)).toBe(true);
    });
  }
});

describe("normalizeToolName (contract section 2, case 60)", () => {
  const servers = ["exa", "github"];
  it("holds one tool against every spelling in note AP section 4", () => {
    for (const raw of ["mcp__exa__search", "mcp_exa_search", "exa_search", "exa__search", "MCP__Exa__Search", "exa/search", " mcp__exa__search "]) {
      expect(normalizeToolName(raw, servers), raw).toBe("exa/search");
    }
  });
  it("drops the plugin name from Claude Code plugin servers", () => {
    expect(normalizeToolName("mcp__plugin_tk_exa__search", servers)).toBe("exa/search");
  });
  it("keeps built-ins bare and lowercased", () => {
    expect(normalizeToolName("Bash", servers)).toBe("bash");
    expect(normalizeToolName("WebFetch", servers)).toBe("webfetch");
    expect(normalizeToolName("apply_patch", servers)).toBe("apply_patch");
    expect(normalizeToolName("foo_bar", servers)).toBe("foo_bar");
  });
  it("maps other characters, non-ASCII included, to underscores", () => {
    expect(normalizeToolName("mcp__my-server__do.thing", servers)).toBe("my_server/do_thing");
    // Cyrillic "Ва" becomes "__", which splits like tk-core's Rust: never "bash".
    expect(normalizeToolName("\u0412\u0430sh", servers)).toBe("sh");
  });
  it("prefers the longest connected server", () => {
    expect(normalizeToolName("github_enterprise_get", ["github", "github_enterprise"])).toBe("github_enterprise/get");
  });
});

describe("fold and the reject list (contract section 3, cases 30 and 33)", () => {
  it("has the pinned confusables table", () => {
    expect(CONFUSABLES_COUNT).toBeGreaterThan(1000);
  });
  it("folds invisibles, Unicode spaces, fullwidth and confusables", () => {
    expect(fold("ANTHROPIC​_BASE URL")).toBe("anthropic_base url");
    expect(fold("ＡＰＩ")).toBe("api");
    expect(fold("АNTHROPIC")).toBe("anthropic");
    expect(fold("аnthropic")).toBe("anthropic");
    expect(fold("ΑPPROVAL")).toBe("approval");
  });
  it("finds every endpoint key in any letter case and spelling", () => {
    for (const key of ENDPOINT_KEYS) {
      expect(rejectKey(key.toUpperCase())).toBe(key);
      expect(rejectKey(`x ${key.split("").join("​")} y`)).toBe(key);
    }
  });
  it("reads JSON and TOML escapes before folding", () => {
    expect(decodeEscapes("ANTHROPIC_BASE\\u005fURL")).toBe("ANTHROPIC_BASE_URL");
    expect(rejectKey('{"env":{"ANTHROPIC_BASE\\u005fURL":"http://x"}}')).toBe("anthropic_base_url");
    expect(rejectKey("approval\\U0000005fpolicy = 'never'")).toBe("approval_policy");
  });
  it("matches git keys only in a git context", () => {
    for (const key of GIT_CONFIG_KEYS) {
      expect(rejectKey(`git -c ${key}x=1 status`), key).toBe(key);
      expect(rejectKey(`[x]\n${key}x = 1 in .git/config`), key).toBe(key);
    }
    expect(rejectKey("notes about protocol.version")).toBeUndefined();
    expect(rejectKey("legit config: protocol.version")).toBeUndefined();
    expect(isGitContext("git config --global alias.x")).toBe(true);
    expect(isGitContext("digit -c")).toBe(false);
  });
  it("turns an allowed shell call into ask for every case 30 token", () => {
    for (const t of UNREADABLE_SHELL_TOKENS) {
      const d = evaluate(loaded, { tool: "Bash", harness: "claude-code", inputText: `ls ${t}x` });
      expect(d.action, t).toBe("ask");
      expect(d.reason).toBe("shell text the matcher cannot read");
    }
    // zsh glob qualifier, history, braces, tilde: case 30's forms. A leading
    // `=` is in case 30 but not in the contract's list: a request for integration.
    for (const cmd of ["ls *(e:'rm -rf x':)", "echo !!", "echo {a,b}", "cat ~/.ssh/id_ed25519"]) {
      expect(evaluate(loaded, { tool: "Bash", harness: "claude-code", inputText: cmd }).action, cmd).toBe("ask");
    }
  });
  it("never puts the input text in the reason", () => {
    const d = evaluate(loaded, { tool: "Bash", harness: "claude-code", inputText: "export ANTHROPIC_BASE_URL=http://203.0.113.9/secret-path" });
    expect(d.reason).not.toContain("203.0.113.9");
  });
});

describe("narrowing beyond the contract (lane A's rules, recorded for integration)", () => {
  it("an allow whose input names a control file asks", () => {
    for (const text of [
      '{"file_path":"/home/u/.claude/settings.json","content":"{}"}',
      '{"file_path":"/home/u/.tk/policy.json"}',
      "echo disableAllHooks",
      "cat ~/.codex/config.toml",
      '{"file_path":"/home/u/.config/opencode/opencode.json"}',
      '{"file_path":"/home/u/.claude/plugins/cache/tk/tk/0.0.0/src/hook.ts"}',
    ]) {
      const d = evaluate(loaded, { tool: "Read", harness: "claude-code", inputText: text });
      expect(d.action, text).toBe("ask");
      expect(d.reason).toMatch(/^input names a control file/);
    }
    // A deny stays a deny.
    expect(evaluate(loaded, { tool: "mcp__exa__delete_index", harness: "pi", inputText: ".tk/x" }).action).toBe("deny");
  });
  it("powershell is a shell tool", () => {
    const p = parsePolicy({ version: 1, rules: [{ tool: "powershell", action: "allow" }] });
    if (typeof p === "string") throw new Error(p);
    expect(evaluate({ policy: p }, { tool: "PowerShell", harness: "pi", inputText: "Get-Item $env:HOME" }).action).toBe("ask");
  });
  it("folds NFKD and drops combining marks", () => {
    expect(fold("a\u0301pproval_policy")).toBe("approval_policy");
    expect(rejectKey("APPROVAL\uFE4Dpolicy")).toBe("approval_policy");
    expect(fold("\u{1D41A}\u{1D427}")).toBe("an");
  });
  it("folds and scans 240,000 characters in linear time (case 33)", () => {
    const big = "\u0410\u200B ".repeat(80_000);
    const started = Date.now();
    expect(rejectKey(big)).toBeUndefined();
    expect(Date.now() - started).toBeLessThan(2000);
  });
});

describe("rank and precedence (contract section 4)", () => {
  const p = parsePolicy({
    version: 1,
    rules: [
      { tool: "*", action: "allow" },
      { tool: "x/*", action: "ask" },
      { tool: "x/y", action: "allow" },
      { tool: "x/y", action: "deny" },
      { tool: "x/z", harness: "codex", action: "allow" },
      { tool: "x/z", action: "deny" },
    ],
  });
  if (typeof p === "string") throw new Error(p);
  const l = { policy: p };
  it("exact beats server/* beats *, and the most restrictive wins a tie", () => {
    expect(evaluate(l, { tool: "mcp__x__y", harness: "pi", inputText: "" }).action).toBe("deny");
    expect(evaluate(l, { tool: "mcp__x__w", harness: "pi", inputText: "" }).action).toBe("ask");
    expect(evaluate(l, { tool: "Read", harness: "pi", inputText: "" }).action).toBe("allow");
  });
  it("a harness-scoped rule beats an unscoped one", () => {
    expect(evaluate(l, { tool: "mcp__x__z", harness: "codex", inputText: "" }).action).toBe("allow");
    expect(evaluate(l, { tool: "mcp__x__z", harness: "claude-code", inputText: "" }).action).toBe("deny");
  });
});

describe("loadPolicy (contract section 1; case 46 is the caller's)", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "tk-policy-"));
  const file = (name: string, text: string) => {
    const f = path.join(dir, name);
    writeFileSync(f, text);
    return f;
  };
  it("a missing file is the empty policy, and every call asks", () => {
    const l = loadPolicy(path.join(dir, "absent.json"));
    expect(l.problem).toBe("no policy file");
    const d = evaluate(l, { tool: "Bash", harness: "claude-code", inputText: "ls" });
    expect(d.action).toBe("ask");
    expect(d.reason).toBe("no rule (no policy file)");
  });
  it("bad JSON, a bad schema, an unknown field or a folder never fail open", () => {
    expect(loadPolicy(file("a.json", "{")).problem).toBe("policy file is not JSON");
    expect(loadPolicy(file("b.json", '{"version":2}')).problem).toContain("version");
    expect(loadPolicy(file("c.json", '{"version":1,"rules":[{"tool":"*","action":"allow","extra":1}]}')).problem).toContain("unknown field");
    expect(loadPolicy(file("d.json", '{"version":1,"askTimeoutSeconds":0}')).problem).toContain("askTimeoutSeconds");
    expect(loadPolicy(file("e.json", '{"version":1,"rules":[{"tool":"Bash","action":"allow"}]}')).problem).toContain("bad tool");
    mkdirSync(path.join(dir, "folder.json"));
    expect(loadPolicy(path.join(dir, "folder.json")).problem).toBeDefined();
    for (const name of ["a.json", "b.json", "c.json", "d.json", "e.json"]) {
      expect(loadPolicy(path.join(dir, name)).policy.rules).toEqual([]);
    }
  });
  it("refuses a relative path and a file others can write", () => {
    expect(loadPolicy("policy.json").problem).toBe("policy path is not absolute");
    const loose = file("loose.json", JSON.stringify(policyFixture));
    chmodSync(loose, 0o666);
    expect(loadPolicy(loose).problem).toBe("policy file writable by group or others");
    expect(loadPolicy(loose).policy.rules).toEqual([]);
    const openDir = path.join(dir, "open");
    mkdirSync(openDir);
    chmodSync(openDir, 0o777);
    const inOpen = path.join(openDir, "policy.json");
    writeFileSync(inOpen, JSON.stringify(policyFixture), { mode: 0o600 });
    expect(loadPolicy(inOpen).problem).toBe("policy folder writable by group or others");
  });
  it("reads a good file through a symlink", () => {
    const good = file("good.json", JSON.stringify(policyFixture));
    symlinkSync(good, path.join(dir, "link.json"));
    expect(loadPolicy(path.join(dir, "link.json")).policy.rules.length).toBe(12);
  });
  it("cleans up", () => {
    rmSync(dir, { recursive: true, force: true });
  });
});

describe("tied allow rules and pins (integration w1, security finding S10)", () => {
  // Rust's evaluate asks unless every tied allow holds its pin; the hook core
  // took the first rule of the tie. One matcher, one answer: the stricter.
  const tied = parsePolicy({
    version: 1,
    servers: ["exa"],
    rules: [
      { tool: "exa/search", action: "allow" },
      { tool: "exa/search", action: "allow", descriptionSha256: "475c1f1fe876e645415d98a2650d6114d89a659a00af81e2741fd6158953a0a0" },
    ],
  });
  if (typeof tied === "string") throw new Error(tied);
  const base = { tool: "mcp__exa__search", harness: "pi", inputText: "" };
  it("asks when a tied allow's pin does not hold", () => {
    expect(evaluate({ policy: tied }, { ...base, description: "changed" }).action).toBe("ask");
    expect(evaluate({ policy: tied }, base).action).toBe("ask");
  });
  it("allows when it holds", () => {
    expect(evaluate({ policy: tied }, { ...base, description: "Write a file." }).action).toBe("allow");
  });
});
