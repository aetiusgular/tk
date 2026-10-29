import { describe, expect, it } from "vitest";
import { parsePolicy, type LoadedPolicy, type PolicyFile } from "./core/policy.ts";
import { policyToToml } from "./policyToml.ts";

function loaded(value: unknown): LoadedPolicy {
  const p = parsePolicy(value);
  if (typeof p === "string") throw new Error(p);
  return { policy: p as PolicyFile };
}

describe("policyToToml", () => {
  const toml = policyToToml(
    loaded({
      version: 1,
      servers: ["my-server", "exa"],
      rules: [
        { tool: "run_shell_command", action: "allow" },
        { tool: "write_file", action: "ask" },
        { tool: "my_server/*", action: "deny" },
        { tool: "exa/search", action: "allow", descriptionSha256: "a".repeat(64) },
        { tool: "read_file", harness: "gemini", action: "deny" },
        { tool: "bash", harness: "pi", action: "allow" },
        { tool: "read_file", chat: "c1", action: "allow" },
      ],
    }),
  );

  it("maps each rule to Gemini's rule shape", () => {
    expect(toml).toContain('[[rule]]\ntoolName = "run_shell_command"\ndecision = "allow"\npriority = 201');
    expect(toml).toContain('[[rule]]\ntoolName = "write_file"\ndecision = "ask_user"\npriority = 211');
    // The server's own spelling, from the policy's servers list.
    expect(toml).toContain('[[rule]]\nmcpName = "my-server"\ntoolName = "*"\ndecision = "deny"\npriority = 121');
  });

  it("a harness rule outranks a general one; a chat rule and another harness's rule are skipped", () => {
    expect(toml).toContain('toolName = "read_file"\ndecision = "deny"\npriority = 521');
    expect(toml).toContain("# skipped: bash allow for harness pi");
    expect(toml).toContain("# skipped: read_file allow for one chat");
  });

  it("an allow with a description pin asks, because Gemini cannot check the pin", () => {
    expect(toml).toContain('mcpName = "exa"\ntoolName = "search"\ndecision = "ask_user"');
  });

  it("ends with tk's default: ask when no rule matches", () => {
    expect(toml.trimEnd().endsWith('toolName = "*"\ndecision = "ask_user"\npriority = 0')).toBe(true);
    const withStar = policyToToml(loaded({ version: 1, rules: [{ tool: "*", action: "deny" }] }));
    expect(withStar.match(/\[\[rule\]\]/g)?.length).toBe(1);
  });

  it("stays inside Gemini's priority range (0 to 999)", () => {
    for (const m of toml.matchAll(/priority = (\d+)/g)) expect(Number(m[1])).toBeLessThanOrEqual(999);
  });
});
