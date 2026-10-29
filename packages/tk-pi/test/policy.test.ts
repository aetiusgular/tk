import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  CONTROL_PATH_MARKERS,
  ENDPOINT_KEYS,
  GIT_CONFIG_KEYS,
  evaluate,
  isGitContext,
  normalizeToolName,
  parsePolicy,
  rejectKey,
  sha256Hex,
  type PolicyFile,
} from "../src/policy.ts";
import { foldText } from "../src/fold.ts";

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, "..", "..", "..", "contracts", "fixtures", "policy");

interface Case {
  id: number;
  call: { tool: string; harness: string; chat?: string; description?: string; inputText: string };
  expect: { action: string; normalizedTool: string; reasonPrefix: string };
}

const loaded = parsePolicy(readFileSync(join(fixtures, "policy.json"), "utf8"));
const cases = (JSON.parse(readFileSync(join(fixtures, "cases.json"), "utf8")) as { cases: Case[] }).cases;

describe("contract fixture (contracts/fixtures/policy)", () => {
  it("the fixture policy parses with no problem", () => {
    expect(loaded.problem).toBeUndefined();
    expect(loaded.policy.rules).toHaveLength(13);
    expect(loaded.policy.askTimeoutSeconds).toBe(30);
  });

  it("has all 51 cases (21 from session 0, 24 from integration w1, 6 from integration w2)", () => {
    expect(cases).toHaveLength(51);
  });

  for (const c of cases) {
    it(`case ${c.id}: ${c.call.tool} -> ${c.expect.action}`, () => {
      const d = evaluate(loaded.policy, c.call);
      expect(d.action).toBe(c.expect.action);
      expect(d.normalizedTool).toBe(c.expect.normalizedTool);
      expect(d.reason.startsWith(c.expect.reasonPrefix), `reason "${d.reason}"`).toBe(true);
    });
  }

  it("no reason ever holds the input text", () => {
    for (const c of cases) {
      const d = evaluate(loaded.policy, c.call);
      if (c.call.inputText.length > 3) expect(d.reason.includes(c.call.inputText)).toBe(false);
    }
  });
});

describe("normalizer (note AP section 4; corner case 60)", () => {
  const servers = ["exa", "github", "dev_radius"];
  it.each([
    "mcp__exa__search",
    "mcp_exa_search",
    "exa_search",
    "exa__search",
    "MCP__Exa__Search",
    "exa/search",
    " mcp__exa__search ",
  ])("%s is exa/search", (raw) => {
    expect(normalizeToolName(raw, servers)).toBe("exa/search");
  });

  it("a Claude Code plugin server keeps its whole name (S5, integration w2)", () => {
    expect(normalizeToolName("mcp__plugin_tk_exa__search", servers)).toBe("plugin_tk_exa/search");
  });

  it("a {workload}_tool name splits at the longest known server", () => {
    expect(normalizeToolName("dev_radius_lookup", servers)).toBe("dev_radius/lookup");
  });

  it("Pi's own MCP spelling keeps a server name with an underscore (Pi docs/mcp.md:77)", () => {
    expect(normalizeToolName("mcp__dev_radius__lookup", servers)).toBe("dev_radius/lookup");
  });

  it("built-ins stay bare", () => {
    expect(normalizeToolName("bash", servers)).toBe("bash");
    expect(normalizeToolName("Bash", servers)).toBe("bash");
    expect(normalizeToolName("powershell", servers)).toBe("powershell");
    expect(normalizeToolName("read_file", servers)).toBe("read_file");
    expect(normalizeToolName("codemode", servers)).toBe("codemode");
  });

  it("other characters, including non-ASCII, become underscores", () => {
    expect(normalizeToolName("mcp__my-server__do.thing", servers)).toBe("my_server/do_thing");
    expect(normalizeToolName("mcp__éxa__search", servers)).toBe("_xa/search");
  });

  it("a nested codemode call id does not matter; only the name is normalized", () => {
    expect(normalizeToolName("mcp__github__get_issue", servers)).toBe("github/get_issue");
  });
});

describe("fold (cases 30 and 33)", () => {
  it("drops invisibles, maps spaces, fullwidth and confusables, lowercases", () => {
    expect(foldText("ANTHROPIC​_BASE URL")).toBe("anthropic_base url");
    expect(foldText("ＡＰＩ")).toBe("api");
    expect(foldText("АNTHROPIC")).toBe("anthropic"); // Cyrillic A
    expect(foldText("аnthrоpic")).toBe("anthropic"); // Cyrillic a, o
    expect(foldText("\u{1D400}NTHROPIC")).toBe("anthropic"); // mathematical bold A (NFKD)
    expect(foldText("ÁNTHROPIC")).toBe("anthropic"); // combining acute
    expect(foldText("Knthropic")).toBe("knthropic"); // Kelvin sign is K
  });

  it("ASCII text folds to its lowercase only", () => {
    const ascii = "git -c core.fsmonitor=./x status; echo $HOME ~ {a,b} `x` !1 (e:x)";
    expect(foldText(ascii)).toBe(ascii.toLowerCase());
  });

  it("every Unicode space in the contract maps to one ASCII space", () => {
    for (const cp of [0x85, 0xa0, 0x1680, 0x2000, 0x2005, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000]) {
      expect(foldText(`a${String.fromCodePoint(cp)}b`), cp.toString(16)).toBe("a b");
    }
  });

  it("every invisible in the contract is dropped", () => {
    for (const cp of [0xad, 0x34f, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x2060, 0x2061, 0x2064, 0xfeff, 0x180e]) {
      expect(foldText(`a${String.fromCodePoint(cp)}b`), cp.toString(16)).toBe("ab");
    }
  });

  it("is linear on 240,000 characters (case 33)", () => {
    const big = "а​".repeat(120_000);
    const t0 = performance.now();
    expect(foldText(big)).toBe("a".repeat(120_000));
    expect(performance.now() - t0).toBeLessThan(2000);
  });
});

describe("reject list (contract section 3; cases 27, 30, 31, 32, 33)", () => {
  it("every endpoint key is refused in any tool, in several spellings", () => {
    for (const k of ENDPOINT_KEYS) {
      expect(rejectKey(`x ${k} y`)).toBe(k);
      expect(rejectKey(`x ${k.toUpperCase()} y`)).toBe(k);
      const spaced = k.split("").join("​");
      expect(rejectKey(spaced)).toBe(k);
    }
  });

  it("every git key is refused in a git command and not in plain text", () => {
    for (const k of GIT_CONFIG_KEYS) {
      expect(rejectKey(`git -c ${k}x=1 status`), k).toBe(k);
      expect(rejectKey(`git config ${k}x 1`), k).toBe(k);
      expect(rejectKey(`echo "[x]\n${k}x = 1" >> .git/config`), k).toBe(k);
    }
    expect(rejectKey("a note about protocol.version and alias.foo")).toBeUndefined();
    expect(rejectKey("gitlab -c protocol.version")).toBeUndefined();
  });

  it("git context: the word git, then -c, config or --config-env on the same line", () => {
    expect(isGitContext("/usr/bin/git -c x")).toBe(true);
    expect(isGitContext("git --config-env=a=b")).toBe(true);
    expect(isGitContext("git status\n-c")).toBe(false);
    expect(isGitContext("legit -c")).toBe(false);
    expect(isGitContext("cat .gitmodules")).toBe(true);
  });

  it("a git key spelled with look-alike letters is refused", () => {
    expect(rejectKey("git -c cоre.fsmоnitor=./x status")).toBe("core.fsmonitor");
    expect(rejectKey("GIT -C CORE.FSMONITOR=x")).toBe("core.fsmonitor");
  });
});

describe("matcher beyond the fixture", () => {
  const base: PolicyFile = {
    version: 1,
    askTimeoutSeconds: 60,
    servers: ["exa"],
    rules: [
      { tool: "bash", action: "allow" },
      { tool: "powershell", action: "allow" },
      { tool: "read", action: "allow" },
      { tool: "write", action: "allow" },
    ],
  };

  it("no policy file: every call is ask, and the problem is named", () => {
    const empty = parsePolicy("{oops");
    expect(empty.problem).toMatch(/not JSON/);
    const d = evaluate(empty.policy, { tool: "read", harness: "pi", inputText: "a.txt" }, empty.problem);
    expect(d.action).toBe("ask");
    expect(d.reason).toBe("no rule (policy file is not JSON)");
  });

  it("each unreadable shell token turns a bash allow into ask (case 30)", () => {
    for (const t of ["$", "`", "!", "{", "}", "~", "(e:", "(+", "(-", "\\", "*", "?", "["]) {
      const d = evaluate(base, { tool: "bash", harness: "pi", inputText: `ls ${t}x` });
      expect(d.action, t).toBe("ask");
      // The reason names the token, and calls a glob one (Mac test w1, M9).
      expect(d.reason).toBe(`shell text the matcher cannot read: \`${t}\`${["*", "?", "["].includes(t) ? " (a glob)" : ""}`);
    }
    expect(evaluate(base, { tool: "bash", harness: "pi", inputText: "ls -la" }).action).toBe("allow");
  });

  it("powershell is a shell tool too", () => {
    expect(evaluate(base, { tool: "powershell", harness: "pi", inputText: "Get-ChildItem $env:HOME" }).action).toBe("ask");
  });

  it("an allow that names a control file becomes ask", () => {
    for (const text of [
      "~/.tk/policy.json",
      "/home/u/.tk/policy.json",
      "/run/user/1000/tk/daemon.token",
      "/home/u/.pi/agent/settings.json",
      "/home/u/.pi/agent/extensions/x.ts",
    ]) {
      const d = evaluate(base, { tool: "write", harness: "pi", inputText: text });
      expect(d.action, text).toBe("ask");
      expect(d.reason).toMatch(/^input names a control file \(/);
    }
    // A project's .pi/settings.json can list extensions and packages that load
    // once the project is trusted (Pi 1.4.0 docs/settings.md: "Project settings
    // override agent-directory settings. Resource lists are combined"), so
    // integration w1 made the whole `.pi/` folder a control path (finding S7).
    // Lane A had this one allowed.
    expect(evaluate(base, { tool: "write", harness: "pi", inputText: "/home/u/proj/.pi/settings.json" }).action).toBe(
      "ask",
    );
    expect(evaluate(base, { tool: "write", harness: "pi", inputText: "/home/u/proj/src/settings.json" }).action).toBe(
      "allow",
    );
    expect(CONTROL_PATH_MARKERS.length).toBeGreaterThan(0);
  });

  it("a deny is never softened by a control file or a shell token", () => {
    const p: PolicyFile = { ...base, rules: [{ tool: "bash", action: "deny" }] };
    expect(evaluate(p, { tool: "bash", harness: "pi", inputText: "ls $x ~/.tk/" }).action).toBe("deny");
  });

  it("chat beats harness beats specificity; ties go to the most restrictive", () => {
    const p: PolicyFile = {
      version: 1,
      askTimeoutSeconds: 60,
      servers: ["exa"],
      rules: [
        { tool: "exa/search", action: "allow" },
        { tool: "exa/*", harness: "pi", action: "ask" },
        { tool: "*", chat: "c1", action: "allow" },
        { tool: "exa/search", chat: "c2", action: "allow" },
        { tool: "exa/search", chat: "c2", action: "deny" },
      ],
    };
    const call = (chat?: string, harness = "pi") => evaluate(p, { tool: "mcp__exa__search", harness, inputText: "", ...(chat ? { chat } : {}) });
    expect(call().action).toBe("ask");
    expect(call(undefined, "claude-code").action).toBe("allow");
    expect(call("c1").action).toBe("allow");
    expect(call("c2").action).toBe("deny");
  });

  it("a pinned description reads the description only when it must", () => {
    let reads = 0;
    const p: PolicyFile = {
      ...base,
      rules: [{ tool: "exa/search", action: "allow", descriptionSha256: sha256Hex("Search.") }],
    };
    const description = () => {
      reads++;
      return "Search.";
    };
    expect(evaluate(p, { tool: "mcp__exa__search", harness: "pi", inputText: "", description }).action).toBe("allow");
    expect(reads).toBe(1);
    expect(evaluate(base, { tool: "read", harness: "pi", inputText: "", description }).action).toBe("allow");
    expect(reads).toBe(1);
  });
});

describe("policy file schema (contracts/policy.schema.json)", () => {
  it.each([
    ['{"version":2}', /version/],
    ['{"version":1,"extra":true}', /unknown field/],
    ['{"version":1,"askTimeoutSeconds":0}', /askTimeoutSeconds/],
    ['{"version":1,"askTimeoutSeconds":3601}', /askTimeoutSeconds/],
    ['{"version":1,"askTimeoutSeconds":1.5}', /askTimeoutSeconds/],
    ['{"version":1,"servers":["a","a"]}', /duplicate/],
    ['{"version":1,"servers":["-a"]}', /server/],
    ['{"version":1,"rules":[{"tool":"Bash","action":"allow"}]}', /tool pattern/],
    ['{"version":1,"rules":[{"tool":"bash","action":"maybe"}]}', /action/],
    ['{"version":1,"rules":[{"tool":"bash","action":"allow","x":1}]}', /unknown field/],
    ['{"version":1,"rules":[{"tool":"bash","action":"allow","harness":"Pi"}]}', /harness/],
    ['{"version":1,"rules":[{"tool":"bash","action":"allow","chat":""}]}', /chat/],
    ['{"version":1,"rules":[{"tool":"bash","action":"allow","descriptionSha256":"abc"}]}', /descriptionSha256/],
    ["[]", /not an object/],
  ])("%s is refused", (text, why) => {
    const r = parsePolicy(text);
    expect(r.problem).toMatch(why);
    expect(r.policy.rules).toHaveLength(0);
  });

  it("more than 1000 rules is refused", () => {
    const rules = Array.from({ length: 1001 }, () => ({ tool: "bash", action: "deny" }));
    expect(parsePolicy(JSON.stringify({ version: 1, rules })).problem).toMatch(/1000/);
  });

  it("defaults: 60 seconds, no servers, no rules", () => {
    const r = parsePolicy('{"version":1}');
    expect(r.problem).toBeUndefined();
    expect(r.policy).toEqual({ version: 1, askTimeoutSeconds: 60, servers: [], rules: [] });
  });
});

describe("tied allow rules and pins (integration w1, security finding S10)", () => {
  // tk-core asks unless every tied allow holds its pin; the same here.
  const tied = parsePolicy(
    JSON.stringify({
      version: 1,
      servers: ["exa"],
      rules: [
        { tool: "exa/search", action: "allow" },
        { tool: "exa/search", action: "allow", descriptionSha256: sha256Hex("Write a file.") },
      ],
    }),
  ).policy;
  const base = { tool: "mcp__exa__search", harness: "pi", inputText: "" };
  it("asks when a tied allow's pin does not hold, and allows when it holds", () => {
    expect(evaluate(tied, { ...base, description: "changed" }).action).toBe("ask");
    expect(evaluate(tied, base).action).toBe("ask");
    expect(evaluate(tied, { ...base, description: "Write a file." }).action).toBe("allow");
  });
});
