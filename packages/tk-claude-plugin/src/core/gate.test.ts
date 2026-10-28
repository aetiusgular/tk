// The text a shell call is checked on (integration w1, security finding S7):
// the command, and every other value the harness gives the shell tool except
// its free-text description, so a working folder (OpenCode's bash `workdir`)
// cannot carry a control path the command leaves out.

import { describe, expect, it } from "vitest";
import { decideCall, inputTextFor } from "./gate.ts";
import { parsePolicy, type LoadedPolicy, type PolicyFile } from "./policy.ts";

const policy = parsePolicy({ version: 1, rules: [{ tool: "bash", action: "allow" }] }) as PolicyFile;
const loaded: LoadedPolicy = { policy };

describe("inputTextFor a shell tool", () => {
  it("checks a workdir beside the command", () => {
    const a = decideCall(loaded, { harness: "opencode", tool: "bash", toolInput: { command: "sed -i s/a/b/ settings.json", workdir: "/home/u/.claude" } });
    expect(a.action).toBe("ask");
    const b = decideCall(loaded, { harness: "opencode", tool: "bash", toolInput: { command: "cat policy.json", workdir: "/home/u/.tk/" } });
    expect(b.action).toBe("ask");
    expect(b.reason).toMatch(/control file/);
  });

  it("leaves the description out, so its prose does not make a plain command ask", () => {
    const a = decideCall(loaded, { harness: "claude-code", tool: "Bash", toolInput: { command: "ls -la", description: "List the files in ~ (all of them!)", timeout: 5000 } });
    expect(a.action).toBe("allow");
    expect(inputTextFor("bash", { command: "ls", description: "x ~", timeout: 5 })).toBe("ls\n5");
  });

  it("is the command alone when nothing else is given", () => {
    expect(inputTextFor("bash", { command: "ls -la" })).toBe("ls -la");
  });
});
