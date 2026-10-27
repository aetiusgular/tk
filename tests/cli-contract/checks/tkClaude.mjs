// tk lane F: contract checks for the Claude Code plugin (T2.1) against the
// real CLI, loaded with --plugin-dir, in MonoCode's spawn shape, with the fake
// Messages server and a fake tk daemon on a Unix socket.
//
//   tk-claude-gate     deny, ask (approver yes, then no approver), MCP input
//                      redaction, output redaction, and the daemon records
//   tk-claude-failure  Claude Code's own onFailure "block" on a crashing and a
//                      hanging PreToolUse hook (the plugin relies on it)
//   tk-claude-bypass   a hook "ask" under bypassPermissions: does Claude Code
//                      still prompt? (not documented; the plugin denies there)

import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startFakeMessages, textBlock, toolUse, lastUserText } from "../lib/fakeMessages.mjs";
import { ClaudeSession } from "../lib/claudeDriver.mjs";
import { makeTempDirs } from "../lib/codexDriver.mjs";
import { startFakeDaemon } from "../../../packages/tk-claude-plugin/src/testing/fakeDaemon.ts";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN = path.resolve(HERE, "..", "..", "..", "packages", "tk-claude-plugin");
const STUB_MCP = path.join(HERE, "..", "lib", "stubMcp.mjs");
const SECRET_IN = "SEKRETin42";
const SECRET_OUT = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

/** Tool results in the request that follows a tool_use, by tool_use_id. */
function toolResult(fake, id) {
  for (const r of [...fake.requests].reverse()) {
    for (const m of r.body?.messages ?? []) {
      for (const c of Array.isArray(m.content) ? m.content : []) {
        if (c?.type === "tool_result" && c.tool_use_id === id) {
          const text = typeof c.content === "string" ? c.content : JSON.stringify(c.content);
          return { text, isError: c.is_error === true };
        }
      }
    }
  }
  return null;
}

/** A model that answers each prompt "<case>" with the tool call `plan[case]`, then "done". */
function scripted(plan, used) {
  return (body) => {
    const last = (body?.messages ?? []).at(-1);
    if (last?.role === "user" && Array.isArray(last.content) && last.content.some((c) => c?.type === "tool_result")) return [textBlock("done")];
    // Claude Code adds its own text blocks to the user message, so match by containment.
    const said = lastUserText(body);
    const p = Object.keys(plan).find((k) => new RegExp(`(^|\\s)${k}(\\s|$)`).test(said));
    const step = p ? plan[p] : undefined;
    if (!p || !step) return [textBlock("ok")];
    const block = toolUse(step.name, step.input);
    used[p] = block.id;
    return [block];
  };
}

async function checkGate({ claudePath }) {
  const dirs = makeTempDirs("tkc-");
  const daemon = await startFakeDaemon();
  const policy = path.join(dirs.root, "policy.json");
  writeFileSync(
    policy,
    JSON.stringify({
      version: 1,
      askTimeoutSeconds: 5,
      servers: ["stub"],
      rules: [
        { tool: "bash", action: "deny" },
        { tool: "read", action: "ask" },
        { tool: "stub/*", action: "allow" },
      ],
    }),
  );
  const note = path.join(dirs.work, "note.txt");
  writeFileSync(note, "plain note\n");
  const secretFile = path.join(dirs.work, "secret.txt");
  writeFileSync(secretFile, `GITHUB_TOKEN=${SECRET_OUT}\n`);
  const stubLog = path.join(dirs.root, "stub.log");
  const used = {};
  const plan = {
    "deny-bash": { name: "Bash", input: { command: "echo hi", description: "say hi" } },
    "ask-yes": { name: "Read", input: { file_path: note } },
    "ask-none": { name: "Read", input: { file_path: note } },
    "redact-in": { name: "mcp__stub__fetch_url", input: { url: `https://example.test/?api_key=${SECRET_IN}` } },
    "redact-out": { name: "Read", input: { file_path: secretFile } },
  };
  const fake = await startFakeMessages({ script: scripted(plan, used) });
  const prompts = [];
  const mcp = JSON.stringify({ mcpServers: { stub: { type: "stdio", command: "node", args: [STUB_MCP], env: { STUB_MCP_LOG: stubLog } } } });
  const s = new ClaudeSession({
    claudePath,
    dirs,
    baseUrl: fake.baseUrl,
    settings: { env: { ENABLE_TOOL_SEARCH: "false" } },
    args: ["--plugin-dir", PLUGIN, "--mcp-config", mcp],
    env: { TK_POLICY: policy, ...daemon.env() },
    onControl: (req) => {
      prompts.push({ tool: req.request?.tool_name, subtype: req.request?.subtype });
      return { behavior: "deny", message: "denied by the contract check" };
    },
  });
  const seen = {};
  try {
    seen.init = s.events.find((e) => e.type === "system" && e.subtype === "init") ? "ok" : "none yet";
    await s.send("deny-bash");
    seen.denyBash = toolResult(fake, used["deny-bash"]);

    daemon.setApprove("yes");
    await s.send("ask-yes");
    seen.askYes = toolResult(fake, used["ask-yes"]);
    seen.approvalsAfterYes = daemon.approvals().map((a) => ({ tool: a.tool, harness: a.harness, timeoutSeconds: a.timeoutSeconds }));

    daemon.setApprove("none");
    const before = prompts.length;
    await s.send("ask-none");
    seen.askNone = toolResult(fake, used["ask-none"]);
    seen.promptsAfterNone = prompts.slice(before);

    await s.send("redact-in");
    seen.redactIn = toolResult(fake, used["redact-in"]);
    seen.stubLog = existsSync(stubLog) ? readFileSync(stubLog, "utf8").split("\n").filter((l) => l.includes("tools/call")) : [];

    daemon.setApprove("yes");
    await s.send("redact-out");
    seen.redactOut = toolResult(fake, used["redact-out"]);
    seen.events = daemon.events().map((e) => ({ id: e.id, kind: e.kind, decision: e.detail?.decision, tool: e.detail?.tool, sha: typeof e.detail?.resultSha256 === "string" }));
  } catch (error) {
    seen.error = error.message;
  } finally {
    seen.stderr = s.stderr.slice(-400);
    await s.close();
    await fake.close();
    await daemon.close();
  }

  const checks = {
    denyReasonReachesModel: !!seen.denyBash && seen.denyBash.text.includes("tk: rule bash"),
    askReachedDaemon: (seen.approvalsAfterYes ?? []).some((a) => a.tool === "read" && a.harness === "claude-code"),
    approvedReadRan: !!seen.askYes && seen.askYes.text.includes("plain note"),
    noApproverFallsBackToPrompt: (seen.promptsAfterNone ?? []).length > 0 && !!seen.askNone && !seen.askNone.text.includes("plain note"),
    mcpInputRedactedAtTool: (seen.stubLog ?? []).length > 0 && seen.stubLog.every((l) => !l.includes(SECRET_IN)) && seen.stubLog.some((l) => l.includes("[redacted by tk]")),
    outputRedactedForModel: !!seen.redactOut && !seen.redactOut.text.includes(SECRET_OUT) && seen.redactOut.text.includes("[redacted by tk]"),
    daemonRecordedPreAndPost: (seen.events ?? []).some((e) => e.kind === "approval") && (seen.events ?? []).some((e) => e.kind === "tool.call" && e.sha),
  };
  const failed = Object.entries(checks).filter(([, v]) => !v).map(([k]) => k);
  return {
    status: seen.error ? "error" : failed.length ? "fail" : "pass",
    finding: seen.error
      ? `error: ${seen.error}`
      : failed.length
        ? `failed: ${failed.join(", ")}. Deny result: ${JSON.stringify(seen.denyBash)?.slice(0, 200)}`
        : "With the plugin loaded by --plugin-dir: a denied Bash returns the hook's reason to the model; a policy ask reached the fake daemon and the approver's yes ran Read; with no approver Claude Code raised its own permission prompt (can_use_tool to the host); an MCP call's api_key reached the stub server redacted; a token in a file read reached the model redacted; the daemon got the decision and the result records.",
    checks,
    seen,
  };
}

/** A session with plain --settings hooks (no plugin), for Claude Code's own behaviour. */
async function hookProbe({ claudePath, hooks, args = [], plan, onControl }) {
  const dirs = makeTempDirs("tkh-");
  const used = {};
  const fake = await startFakeMessages({ script: scripted(plan, used) });
  const prompts = [];
  const s = new ClaudeSession({
    claudePath,
    dirs,
    baseUrl: fake.baseUrl,
    settings: { hooks },
    args,
    onControl: (req) => {
      prompts.push(req.request?.tool_name ?? req.request?.subtype);
      return onControl ? onControl(req) : { behavior: "deny", message: "denied by the contract check" };
    },
  });
  const out = { prompts, results: {} };
  try {
    for (const p of Object.keys(plan)) {
      await s.send(p, 60000);
      out.results[p] = toolResult(fake, used[p]);
    }
  } catch (error) {
    out.error = error.message;
  } finally {
    out.stderr = s.stderr.slice(-300);
    await s.close();
    await fake.close();
  }
  return { out, dirs };
}

async function checkFailure({ claudePath }) {
  const marker = (dirs, name) => path.join(dirs.work, name);
  const seen = {};
  for (const [label, command, timeout] of [
    ["crash", "exit 1", undefined],
    ["hang", "sleep 20", 2],
  ]) {
    for (const onFailure of [undefined, "block"]) {
      const hook = { type: "command", command, ...(timeout ? { timeout } : {}), ...(onFailure ? { onFailure } : {}) };
      const { out, dirs } = await hookProbe({
        claudePath,
        hooks: { PreToolUse: [{ matcher: "Bash", hooks: [hook] }] },
        args: ["--permission-mode", "acceptEdits", "--allowedTools", "Bash(touch:*)"],
        plan: { probe: { name: "Bash", input: { command: "touch ran.txt", description: "touch" } } },
        onControl: (req) => ({ behavior: "allow", updatedInput: req.request?.input ?? {} }),
      });
      seen[`${label}-${onFailure ?? "unset"}`] = { ran: existsSync(marker(dirs, "ran.txt")), result: out.results.probe?.text?.slice(0, 160), error: out.error };
    }
  }
  const ok = seen["crash-block"]?.ran === false && seen["hang-block"]?.ran === false;
  return {
    status: ok ? "pass" : "fail",
    finding: `PreToolUse hook that fails, with and without onFailure "block": crash unset ran=${seen["crash-unset"]?.ran}, crash block ran=${seen["crash-block"]?.ran}, hang (timeout 2 s) unset ran=${seen["hang-unset"]?.ran}, hang block ran=${seen["hang-block"]?.ran}. The tk plugin sets onFailure "block" on PreToolUse, so a broken gate blocks.`,
    seen,
  };
}

async function checkBypass({ claudePath }) {
  const ask = `printf '%s' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"probe asks"}}'`;
  const seen = {};
  for (const mode of ["default", "bypassPermissions"]) {
    const { out, dirs } = await hookProbe({
      claudePath,
      hooks: { PreToolUse: [{ matcher: "Bash", hooks: [{ type: "command", command: ask }] }] },
      args: ["--permission-mode", mode],
      plan: { probe: { name: "Bash", input: { command: "touch ran.txt", description: "touch" } } },
    });
    seen[mode] = { prompted: out.prompts.length > 0, ran: existsSync(path.join(dirs.work, "ran.txt")), result: out.results.probe?.text?.slice(0, 160), error: out.error, stderr: out.stderr.slice(-160) };
  }
  const b = seen.bypassPermissions;
  if (/cannot be used with root/.test(`${b?.stderr ?? ""}${b?.error ?? ""}`)) {
    return {
      status: "skip",
      finding: `Not run: Claude Code refuses bypassPermissions as root ("--dangerously-skip-permissions cannot be used with root/sudo privileges"). Default mode prompted=${seen.default?.prompted}, ran=${seen.default?.ran}. Run this check as a normal user (the Mac test).`,
      seen,
    };
  }
  return {
    status: "info",
    finding: `A hook "ask" on Bash: default mode prompted=${seen.default?.prompted}, ran=${seen.default?.ran}; bypassPermissions prompted=${b?.prompted}, ran=${b?.ran}${b?.error ? ` (error ${b.error})` : ""}. ${b?.ran && !b?.prompted ? "Under bypassPermissions Claude Code skips the hook's ask: the tk plugin's deny there is needed." : "The tk plugin denies an unanswered ask in bypassPermissions either way."}`,
    seen,
  };
}

export const tkClaudeChecks = [
  { id: "tk-claude-gate", title: "tk Claude Code plugin: deny, ask through the daemon, redaction, records", flag: "lane F T2.1", run: checkGate },
  { id: "tk-claude-failure", title: "Claude Code onFailure block on a failing PreToolUse hook", flag: "lane F T2.1 fail closed", run: checkFailure },
  { id: "tk-claude-bypass", title: "Claude Code: a hook ask under bypassPermissions", flag: "lane F T2.1, Research (a)", run: checkBypass },
];
