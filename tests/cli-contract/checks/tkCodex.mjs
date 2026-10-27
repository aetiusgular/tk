// tk lane F: contract checks for the Codex plugin (T2.2) against the real
// CLI. The plugin is installed the way a user would (a local marketplace,
// `codex plugin marketplace add`, `codex plugin add tk@tk`) into a temp
// CODEX_HOME, its two hooks pre-trusted in config.toml by their hash, then
// `codex exec` runs against the fake Responses server with a fake tk daemon.
//
// Codex 0.163.0 facts this relies on (source at rust-v0.163.0, see
// plans/parallel/tk_lane_F.md, Research): shell calls reach hooks as
// tool_name "Bash"; plugin hooks need trust, kept in
// [hooks.state."<plugin>@<marketplace>:hooks/hooks.json:<event>:<group>:<handler>"]
// with trusted_hash = "sha256:" + sha256 of the key-sorted JSON of
// {event_name, matcher, hooks: [normalized handler]} (hooks/src/engine/discovery.rs:766-794,
// config/src/fingerprint.rs:54-66).

import { createHash } from "node:crypto";
import { spawn, execFileSync } from "node:child_process";
import { cpSync, mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startFakeResponses, call, text, callOutputs } from "../lib/fakeResponses.mjs";
import { codexHomeConfig, makeTempDirs, isolatedEnv } from "../lib/codexDriver.mjs";
import { startFakeDaemon } from "../../../packages/tk-claude-plugin/src/testing/fakeDaemon.ts";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN = path.resolve(HERE, "..", "..", "..", "packages", "tk-codex-plugin");
const STUB_MCP = path.join(HERE, "..", "lib", "stubMcp.mjs");
const SECRET_OUT = "AKIAABCDEFGHIJKLMNOP";
const SECRET_IN = "SEKRETin42";

function sortKeys(v) {
  if (Array.isArray(v)) return v.map(sortKeys);
  if (v && typeof v === "object") return Object.fromEntries(Object.keys(v).sort().map((k) => [k, sortKeys(v[k])]));
  return v;
}

/** The trust entries for every handler in the plugin's hooks.json. */
export function trustToml(hooksJson, keySource) {
  const labels = { PreToolUse: "pre_tool_use", PostToolUse: "post_tool_use" };
  const lines = [];
  for (const [event, groups] of Object.entries(hooksJson.hooks)) {
    groups.forEach((group, gi) => {
      group.hooks.forEach((h, hi) => {
        const handler = { type: h.type, command: h.command, timeout: h.timeout ?? 600, async: h.async ?? false };
        if (h.statusMessage !== undefined) handler.statusMessage = h.statusMessage;
        const identity = { event_name: labels[event], hooks: [handler] };
        if (group.matcher !== undefined) identity.matcher = group.matcher;
        const hash = createHash("sha256").update(JSON.stringify(sortKeys(identity))).digest("hex");
        lines.push(`[hooks.state."${keySource}:hooks/hooks.json:${labels[event]}:${gi}:${hi}"]`, `trusted_hash = "sha256:${hash}"`, "");
      });
    });
  }
  return lines.join("\n");
}

function run(bin, args, opts) {
  return new Promise((resolve) => {
    const ch = spawn(bin, args, { ...opts, stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    ch.stdout.on("data", (d) => (out += d));
    ch.stderr.on("data", (d) => (err += d));
    const timer = setTimeout(() => ch.kill("SIGKILL"), opts.timeoutMs ?? 90000);
    ch.on("exit", (code) => {
      clearTimeout(timer);
      resolve({ code, out, err });
    });
  });
}

/** One codex exec run with the tk plugin installed. `plan` returns the model's first reply. */
async function execWithPlugin({ codexPath, rules, first, approve = "none", mcp = false, files = {} }) {
  const dirs = makeTempDirs("tkx-");
  for (const [name, content] of Object.entries(files)) writeFileSync(path.join(dirs.work, name), content);
  const daemon = await startFakeDaemon({ approve });
  const fake = await startFakeResponses({
    script: (body, n) => (n === 0 ? first() : [text("done")]),
  });
  const policy = path.join(dirs.root, "policy.json");
  writeFileSync(policy, JSON.stringify({ version: 1, askTimeoutSeconds: 5, servers: ["stub"], rules }));
  // A local marketplace with a copy of the plugin, as `codex plugin add` reads it.
  const mkt = path.join(dirs.root, "mkt");
  mkdirSync(path.join(mkt, ".agents", "plugins"), { recursive: true });
  writeFileSync(path.join(mkt, ".agents", "plugins", "marketplace.json"), JSON.stringify({ name: "tk", plugins: [{ name: "tk", source: { source: "local", path: "./tk" } }] }));
  for (const part of [".codex-plugin", "hooks", "bin", "src"]) cpSync(path.join(PLUGIN, part), path.join(mkt, "tk", part), { recursive: true });
  const stubLog = path.join(dirs.root, "stub.log");
  const extra = [
    // default_tools_approval_mode: Codex's own approval would refuse the call under approval_policy "never" (config/src/mcp_types.rs:280).
    mcp ? `[mcp_servers.stub]\ndefault_tools_approval_mode = "approve"\ncommand = "node"\nargs = [${JSON.stringify(STUB_MCP)}]\nenv = { STUB_MCP_LOG = ${JSON.stringify(stubLog)} }\n` : "",
  ].join("\n");
  writeFileSync(path.join(dirs.codexHome, "config.toml"), codexHomeConfig({ baseUrl: `${fake.baseUrl}/v1`, extra }));
  const env = isolatedEnv({ home: dirs.home, extra: { CODEX_HOME: dirs.codexHome, TK_POLICY: policy, ...daemon.env() } });
  const seen = {};
  try {
    seen.mktAdd = execFileSync(codexPath, ["plugin", "marketplace", "add", mkt, "--json"], { env, encoding: "utf8", timeout: 60000 }).trim().slice(0, 200);
    seen.pluginAdd = execFileSync(codexPath, ["plugin", "add", "tk@tk", "--json"], { env, encoding: "utf8", timeout: 60000 }).trim().slice(0, 200);
    const hooksJson = JSON.parse(readFileSync(path.join(PLUGIN, "hooks", "hooks.json"), "utf8"));
    writeFileSync(path.join(dirs.codexHome, "config.toml"), `${readFileSync(path.join(dirs.codexHome, "config.toml"), "utf8")}\n${trustToml(hooksJson, "tk@tk")}`);
    const r = await run(codexPath, ["exec", "--skip-git-repo-check", "--json", "-s", "workspace-write", "-c", 'approval_policy="never"', "go"], { cwd: dirs.work, env });
    seen.exit = r.code;
    seen.stderrTail = r.err.slice(-300);
    seen.outputs = Object.assign({}, ...fake.requests.map((q) => callOutputs(q.body)));
    seen.approvals = daemon.approvals();
    seen.events = daemon.events().map((e) => ({ id: e.id, kind: e.kind, decision: e.detail?.decision, sha: typeof e.detail?.resultSha256 === "string" }));
    seen.stubLog = existsSync(stubLog) ? readFileSync(stubLog, "utf8").split("\n").filter((l) => l.includes("tools/call")) : [];
  } catch (error) {
    seen.error = error.message?.slice(0, 400);
  } finally {
    await fake.close();
    await daemon.close();
  }
  return seen;
}

async function checkGate({ codexPath }) {
  const seen = {};
  seen.deny = await execWithPlugin({
    codexPath,
    rules: [{ tool: "bash", action: "allow" }],
    first: () => [call("exec_command", { cmd: "git -c core.fsmonitor=./x status" }, "d1")],
  });
  seen.askYes = await execWithPlugin({
    codexPath,
    rules: [{ tool: "bash", action: "ask" }],
    approve: "yes",
    first: () => [call("exec_command", { cmd: "echo approved-run" }, "y1")],
  });
  seen.askNone = await execWithPlugin({
    codexPath,
    rules: [{ tool: "bash", action: "ask" }],
    approve: "none",
    first: () => [call("exec_command", { cmd: "echo should-not-run" }, "n1")],
  });
  seen.redactOut = await execWithPlugin({
    codexPath,
    rules: [{ tool: "bash", action: "allow" }],
    // The token is in the file, not in the command: a token in a command is an ask.
    files: { "creds.txt": `AWS=${SECRET_OUT}\n` },
    first: () => [call("exec_command", { cmd: "cat creds.txt" }, "r1")],
  });
  seen.redactIn = await execWithPlugin({
    codexPath,
    mcp: true,
    rules: [{ tool: "stub/*", action: "allow" }],
    first: () => [{ ...call("fetch_url", { url: `https://example.test/?api_key=${SECRET_IN}` }, "m1"), namespace: "mcp__stub" }],
  });
  const out = (s, id) => s.outputs?.[id] ?? "";
  const checks = {
    denyReasonReachesModel: out(seen.deny, "d1").includes("tk: reject list: core.fsmonitor"),
    askReachedDaemon: (seen.askYes.approvals ?? []).some((a) => a.harness === "codex" && a.tool === "bash"),
    approvedCommandRan: out(seen.askYes, "y1").includes("approved-run"),
    // Codex's block message repeats the command, so check that it was blocked, not that the text is absent.
    noApproverIsDeny: out(seen.askNone, "n1").startsWith("Command blocked by PreToolUse hook: tk:") && out(seen.askNone, "n1").includes("none is connected"),
    outputRedactedForModel: out(seen.redactOut, "r1").includes("[redacted by tk]") && !out(seen.redactOut, "r1").includes(SECRET_OUT),
    mcpInputRedactedAtTool: (seen.redactIn.stubLog ?? []).length > 0 && seen.redactIn.stubLog.every((l) => !l.includes(SECRET_IN)) && seen.redactIn.stubLog.some((l) => l.includes("[redacted by tk]")),
    daemonRecordedPreAndPost: (seen.askYes.events ?? []).some((e) => e.kind === "approval") && (seen.askYes.events ?? []).some((e) => e.kind === "tool.call" && e.sha),
  };
  const failed = Object.entries(checks).filter(([, v]) => !v).map(([k]) => k);
  const errors = Object.entries(seen).filter(([, s]) => s.error).map(([k, s]) => `${k}: ${s.error}`);
  return {
    status: errors.length ? "error" : failed.length ? "fail" : "pass",
    finding: errors.length
      ? errors.join(" | ")
      : failed.length
        ? `failed: ${failed.join(", ")}. Outputs: ${JSON.stringify(Object.fromEntries(Object.entries(seen).map(([k, s]) => [k, s.outputs]))).slice(0, 700)}`
        : "Installed through codex plugin marketplace add and codex plugin add, hooks trusted by hash: a reject-list command was blocked with the hook's reason; an ask reached the fake daemon and the approver's yes ran the command; with no approver the ask was a deny with the reason; a token in command output reached the model redacted (PostToolUse decision block); an MCP call's api_key reached the stub server redacted; the daemon got both records.",
    checks,
    seen,
  };
}

export const tkCodexChecks = [{ id: "tk-codex-gate", title: "tk Codex plugin: installed, trusted, deny, ask through the daemon, redaction", flag: "lane F T2.2", run: checkGate }];
