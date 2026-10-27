// Contract checks against the real `codex app-server` with a fake model.
// Each check returns { status, finding, seen }:
//   status "pass"  the behaviour the plans build on is there
//   status "fail"  it is not; the finding says what to switch
//   status "info"  a question with no expected answer; the finding records it
// `seen` holds the requests and messages the answer rests on.

import { writeFileSync, readFileSync, existsSync, mkdirSync } from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { homedir, userInfo } from "node:os";
import { fileURLToPath } from "node:url";
import { startFakeResponses, text, call, callOutputs, inputText, reasoning } from "../lib/fakeResponses.mjs";
import { CodexClient, makeTempDirs, SUPERVISED, SUPERVISED_TURN, userInput } from "../lib/codexDriver.mjs";
import { mockReply } from "../codex-mock/mockReply.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const STUB_MCP = path.join(HERE, "..", "lib", "stubMcp.mjs");

const FULL_ACCESS = { approvalPolicy: "on-request", sandbox: "danger-full-access", approvalsReviewer: "user" };
const FULL_ACCESS_TURN = { approvalPolicy: "on-request", approvalsReviewer: "user", sandboxPolicy: { type: "dangerFullAccess" } };

/** Answers every server request the way a test asks, and records it. */
function answerer(decide = {}) {
  return (msg) => {
    const own = decide[msg.method];
    if (own) return own(msg);
    switch (msg.method) {
      case "currentTime/read":
        return { currentTimeAt: Math.floor(Date.now() / 1000) };
      case "item/fileChange/requestApproval":
      case "item/commandExecution/requestApproval":
        return { decision: "accept" };
      case "mcpServer/elicitation/request":
        return { action: "accept", content: {}, _meta: null };
      case "item/tool/requestUserInput":
        return { answers: {} };
      default:
        return {};
    }
  };
}

async function open({ codexPath, script, configExtra = "", configTop = "", decide, dirs }) {
  const fake = await startFakeResponses({ script });
  dirs = dirs ?? makeTempDirs();
  const client = new CodexClient({ codexPath, dirs, baseUrl: fake.baseUrl, configExtra, configTop, onServerRequest: answerer(decide) });
  await client.initialize();
  return { fake, dirs, client };
}

async function shut(...parts) {
  for (const p of parts) {
    try {
      await p?.client?.close();
    } catch {}
    try {
      await p?.fake?.close();
    } catch {}
  }
}

/** The developer-role texts of a Responses request, in order. */
function developerTexts(body) {
  return (body?.input ?? [])
    .filter((i) => i.role === "developer" || i.role === "system")
    .map((i) =>
      (Array.isArray(i.content) ? i.content : [{ text: i.content }])
        .map((c) => c?.text ?? "")
        .join("\n"),
    );
}

function count(haystack, needle) {
  return haystack.split(needle).length - 1;
}

/** The last request that carried this user prompt. */
function requestFor(fake, prompt) {
  return [...fake.requests].reverse().find((r) => inputText(r.body).includes(prompt));
}

function userTexts(body) {
  return (body?.input ?? [])
    .filter((i) => i.role === "user")
    .flatMap((i) => (Array.isArray(i.content) ? i.content : []).map((c) => c?.text ?? ""))
    .filter((t) => /^turn-\d+$/.test(t.trim()));
}

// ---------------------------------------------------------------------------

async function check3({ codexPath }) {
  const seen = {};
  const a = await open({ codexPath, script: () => [text("ok")] });
  let b;
  try {
    const started = await a.client.request("thread/start", { cwd: a.dirs.work, ...SUPERVISED, developerInstructions: "The code word is PINEAPPLE." });
    const threadId = started.thread.id;
    await a.client.runTurn({ threadId, input: userInput("turn-1"), ...SUPERVISED_TURN });
    const r1 = requestFor(a.fake, "turn-1");
    seen.start = { developerTexts: developerTexts(r1.body).map((t) => t.slice(0, 300)), pineapple: count(JSON.stringify(r1.body), "PINEAPPLE") };

    // MonoCode's turn/start with a known model sends collaborationMode with
    // developer_instructions: null (buildTurnStartParams at 53e1b90).
    await a.client.runTurn({
      threadId,
      input: userInput("turn-2"),
      ...SUPERVISED_TURN,
      model: "fake-model",
      collaborationMode: { mode: "default", settings: { model: "fake-model", reasoning_effort: null, developer_instructions: null } },
    });
    const r2 = requestFor(a.fake, "turn-2");
    seen.nullOnTurn = { pineapple: count(JSON.stringify(r2.body), "PINEAPPLE"), developerCount: developerTexts(r2.body).length };

    // Turn-level text through collaborationMode, the only other channel.
    await a.client.runTurn({
      threadId,
      input: userInput("turn-2b"),
      ...SUPERVISED_TURN,
      model: "fake-model",
      collaborationMode: { mode: "default", settings: { model: "fake-model", reasoning_effort: null, developer_instructions: "The code word is KIWI." } },
    });
    const r2b = requestFor(a.fake, "turn-2b");
    seen.turnLevel = {
      kiwi: count(JSON.stringify(r2b.body), "KIWI"),
      wrapped: developerTexts(r2b.body).find((t) => t.includes("KIWI"))?.slice(0, 120) ?? null,
      pineapple: count(JSON.stringify(r2b.body), "PINEAPPLE"),
    };
    await a.client.close();

    // A new app-server on the same home resumes with other text, as after a restart.
    b = await open({ codexPath, script: () => [text("ok")], dirs: a.dirs });
    await b.client.request("thread/resume", { threadId, ...SUPERVISED, developerInstructions: "The code word is MANGO." });
    await b.client.runTurn({ threadId, input: userInput("turn-3"), ...SUPERVISED_TURN });
    const r3 = requestFor(b.fake, "turn-3");
    const s3 = JSON.stringify(r3.body);
    seen.resume = {
      pineapple: count(s3, "PINEAPPLE"),
      mango: count(s3, "MANGO"),
      kiwi: count(s3, "KIWI"),
    };
  } finally {
    await shut(a, b);
  }
  // The native channel the plan builds on: text set at thread/start reaches
  // the model and stays for the thread's life, a restart included.
  const ok = seen.start.pineapple > 0 && seen.nullOnTurn.pineapple > 0 && seen.resume.pineapple > 0;
  return {
    status: ok ? "pass" : "fail",
    finding:
      `thread/start developerInstructions reaches the first request as a developer message and stays in the thread (${seen.resume.pineapple}x after a restart). ` +
      `turn/start with collaborationMode developer_instructions:null leaves it in place (${seen.nullOnTurn.pineapple}x); null does not clear it. ` +
      `thread/resume with new developerInstructions is IGNORED for an existing thread (MANGO ${seen.resume.mango}x). ` +
      `Turn-level collaborationMode developer_instructions works (KIWI ${seen.turnLevel.kiwi}x, sent as "${seen.turnLevel.wrapped}") and stays in history after it (${seen.resume.kiwi}x). ` +
      `So a changed preamble reaches an existing Codex thread only through turn-level developer_instructions.`,
    resumeIgnored: seen.resume.mango === 0,
    seen,
  };
}

async function fiveTurns(client, threadId, n, turnParams = SUPERVISED_TURN) {
  const ids = [];
  for (let i = 1; i <= n; i++) {
    const r = await client.runTurn({ threadId, input: userInput(`turn-${i}`), ...turnParams });
    ids.push(r.turnId);
  }
  return ids;
}

async function check4({ codexPath }) {
  const seen = {};
  const tryMode = async (historyMode) => {
    const s = await open({ codexPath, script: () => [text("ok")] });
    try {
      const started = await s.client.request("thread/start", {
        cwd: s.dirs.work,
        ...SUPERVISED,
        ...(historyMode ? { historyMode } : {}),
      });
      const threadId = started.thread.id;
      const ids = await fiveTurns(s.client, threadId, 5);
      let revert;
      try {
        revert = await s.client.request("thread/revert", { threadId, beforeTurnId: ids[2] });
      } catch (error) {
        return { historyMode: historyMode ?? "default (MonoCode)", error: error.message };
      }
      const list = await s.client.request("thread/turns/list", { threadId, sortDirection: "asc" });
      await s.client.runTurn({ threadId, input: userInput("turn-6"), ...SUPERVISED_TURN });
      const r6 = requestFor(s.fake, "turn-6");
      return {
        historyMode: historyMode ?? "default (MonoCode)",
        revertKeys: Object.keys(revert ?? {}),
        turnsAfter: list.data.length,
        nextRequestHas: userTexts(r6.body),
      };
    } finally {
      await shut(s);
    }
  };
  seen.default = await tryMode(undefined);
  if (seen.default.error) seen.paginated = await tryMode("paginated");
  const good = (r) => r && !r.error && r.turnsAfter === 2 && r.nextRequestHas.join() === "turn-1,turn-2,turn-6";
  const ok = good(seen.default);
  return {
    status: ok ? "pass" : "fail",
    finding: ok
      ? "thread/revert three turns back on a five-turn thread, as MonoCode starts threads: 2 turns left, and the next request carries only turn-1, turn-2 and the new prompt."
      : seen.default.error
        ? `thread/revert refused on a thread started as MonoCode starts it: "${seen.default.error}". With historyMode "paginated": ${good(seen.paginated) ? "works" : JSON.stringify(seen.paginated)}.`
        : `thread/revert ran but history is wrong: ${JSON.stringify(seen.default)}.`,
    seen,
  };
}

async function check5({ codexPath }) {
  const seen = {};
  const s = await open({ codexPath, script: () => [text("ok")] });
  try {
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...SUPERVISED });
    const threadId = started.thread.id;
    const ids = await fiveTurns(s.client, threadId, 3);
    const fork = await s.client.request("thread/fork", { threadId, lastTurnId: ids[1], ...SUPERVISED });
    const forkId = fork.thread.id;
    seen.forkId = forkId;
    seen.newId = forkId !== threadId;
    await s.client.runTurn({ threadId: forkId, input: userInput("turn-9"), ...SUPERVISED_TURN });
    seen.forkRequestHas = userTexts(requestFor(s.fake, "turn-9").body);
    await s.client.runTurn({ threadId, input: userInput("turn-4"), ...SUPERVISED_TURN });
    seen.originalRequestHas = userTexts(requestFor(s.fake, "turn-4").body);
  } finally {
    await shut(s);
  }
  const ok =
    seen.newId &&
    seen.forkRequestHas.join() === "turn-1,turn-2,turn-9" &&
    seen.originalRequestHas.join() === "turn-1,turn-2,turn-3,turn-4";
  return {
    status: ok ? "pass" : "fail",
    finding: ok
      ? "thread/fork with lastTurnId gives a new thread id holding turns 1 and 2 only; the original thread keeps all three."
      : `thread/fork result differs: ${JSON.stringify(seen)}.`,
    seen,
  };
}

async function check10({ codexPath }) {
  const seen = { serverRequests: [] };
  const logDir = makeTempDirs("mcplog-").root;
  const mcpLog = path.join(logDir, "calls.log");
  let toolNames = [];
  const s = await open({
    codexPath,
    configExtra: [
      "[mcp_servers.stub]",
      'command = "node"',
      `args = [${JSON.stringify(STUB_MCP)}, ${JSON.stringify(mcpLog)}]`,
    ].join("\n"),
    script: (body, n) => {
      if (n === 0) {
        toolNames = (body.tools ?? []).map((t) => t.name ?? t.type);
        seen.mcpToolShape = (body.tools ?? []).find((t) => t.name === "mcp__stub");
        // Codex 0.161.0 sends MCP tools as a Responses `namespace` tool and
        // only accepts a call that names the namespace (probe, 12 Oct).
        return [{ ...call("fetch_url", { url: "https://example.com/secret-page" }, "m1"), namespace: "mcp__stub" }];
      }
      return [text(`done ${JSON.stringify(callOutputs(body)).slice(0, 200)}`)];
    },
    decide: {
      "mcpServer/elicitation/request": (msg) => {
        seen.serverRequests.push(msg);
        return { action: "accept", content: {}, _meta: null };
      },
      "item/tool/requestUserInput": (msg) => {
        seen.serverRequests.push(msg);
        return { answers: {} };
      },
      "item/commandExecution/requestApproval": (msg) => {
        seen.serverRequests.push(msg);
        return { decision: "accept" };
      },
      "item/permissions/requestApproval": (msg) => {
        seen.serverRequests.push(msg);
        return { permissions: {}, scope: "turn" };
      },
    },
  });
  try {
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...SUPERVISED });
    // MCP servers start in the background; give the stub a moment.
    await new Promise((r) => setTimeout(r, 1500));
    const r = await s.client.runTurn({ threadId: started.thread.id, input: userInput("fetch it"), ...SUPERVISED_TURN });
    seen.mcpTools = toolNames.filter((t) => /stub|fetch_url|write_note/.test(t));
    seen.items = r.notifications
      .filter((n) => n.method === "item/completed" && /mcp/i.test(n.params?.item?.type ?? ""))
      .map((n) => n.params.item);
    seen.stubCalled = existsSync(mcpLog) && readFileSync(mcpLog, "utf8").includes("tools/call");
  } finally {
    await shut(s);
  }
  const req = seen.serverRequests[0];
  const carriesUrl = req ? JSON.stringify(req.params).includes("example.com/secret-page") : false;
  const meta = req?.params?._meta ?? {};
  return {
    status: "info",
    finding: req
      ? `A supervised MCP tool call reaches the client as ${req.method} (mode ${req.params?.mode}), message "${req.params?.message}". ` +
        `The URL is ${carriesUrl ? "in it" : "NOT in it"}${carriesUrl ? `: _meta.tool_params ${JSON.stringify(meta.tool_params)}` : ""}. ` +
        `_meta.codex_approval_kind "${meta.codex_approval_kind}", persist ${JSON.stringify(meta.persist)}. Stub tool ran after accept: ${seen.stubCalled}. ` +
        `MCP tools go out as one Responses "namespace" tool (mcp__stub with nested functions).`
      : `No confirmation reached the client for a supervised MCP tool call. Stub tool ran: ${seen.stubCalled}. Tools offered: ${seen.mcpTools.join(", ") || "none"}.`,
    carriesUrl,
    seen,
  };
}

async function checkMcpFlatName({ codexPath }) {
  // A local model behind Ollama may not emit the `namespace` field. Does Codex
  // take the flat name a model would guess from the tool list?
  const seen = { outputs: {} };
  const s = await open({
    codexPath,
    configExtra: ["[mcp_servers.stub]", 'command = "node"', `args = [${JSON.stringify(STUB_MCP)}]`].join("\n"),
    script: (body, n) => {
      if (n === 0) return [call("mcp__stub__fetch_url", { url: "https://example.com/a" }, "f1")];
      seen.outputs = callOutputs(body);
      return [text("done")];
    },
  });
  try {
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...SUPERVISED });
    await new Promise((r) => setTimeout(r, 1500));
    await s.client.runTurn({ threadId: started.thread.id, input: userInput("fetch"), ...SUPERVISED_TURN });
  } finally {
    await shut(s);
  }
  const refused = /unsupported call/.test(seen.outputs.f1 ?? "");
  return {
    status: "info",
    finding: refused
      ? `A flat call named mcp__stub__fetch_url is refused with "${seen.outputs.f1}". A model must send name "fetch_url" with namespace "mcp__stub". Whether Ollama's /v1/responses passes namespace tools to the model and returns the namespace field is not tested here (Mac, corner case 13).`
      : `A flat call named mcp__stub__fetch_url was accepted: ${seen.outputs.f1}.`,
    refused,
    seen,
  };
}

async function checkBackground({ codexPath }) {
  const seen = {};
  const s = await open({
    codexPath,
    script: (body, n) => {
      if (n === 0) return [call("exec_command", { cmd: "sleep 120", yield_time_ms: 250 }, "bg1")];
      return [text("started")];
    },
  });
  try {
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...FULL_ACCESS });
    const threadId = started.thread.id;
    await s.client.runTurn({ threadId, input: userInput("run it in the background"), ...FULL_ACCESS_TURN });
    try {
      const list = await s.client.request("thread/backgroundTerminals/list", { threadId });
      seen.list = list.data.map((t) => ({ processId: t.processId, command: t.command, osPid: t.osPid }));
      if (list.data[0]) {
        seen.terminate = await s.client.request("thread/backgroundTerminals/terminate", { threadId, processId: list.data[0].processId });
        const after = await s.client.request("thread/backgroundTerminals/list", { threadId });
        seen.afterTerminate = after.data.length;
      }
    } catch (error) {
      seen.error = error.message;
    }
  } finally {
    await shut(s);
  }
  const ok = !seen.error && seen.list?.length === 1 && /sleep 120/.test(seen.list[0].command) && seen.afterTerminate === 0;
  return {
    status: ok ? "pass" : "fail",
    finding: seen.error
      ? `thread/backgroundTerminals/list failed on a local provider: "${seen.error}".`
      : `A command left running after its turn is listed (${JSON.stringify(seen.list)}); terminate ${seen.afterTerminate === 0 ? "removes it" : "did not remove it"}.`,
    seen,
  };
}

// Mac test 1.8: is a command listed while the turn that started it still
// runs, and does terminate end it there? MonoCode's Running section polls
// during the turn and offers Stop on what this returns.
async function checkBackgroundMidTurn({ codexPath }) {
  const seen = {};
  const s = await open({
    codexPath,
    script: (body, n) => {
      if (n === 0) return [call("exec_command", { cmd: "sleep 120", yield_time_ms: 60000 }, "mt1")];
      return [text("done")];
    },
  });
  try {
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...FULL_ACCESS });
    const threadId = started.thread.id;
    const before = s.client.notifications.length;
    await s.client.request("turn/start", { threadId, input: userInput("run it"), ...FULL_ACCESS_TURN });
    await s.client.waitFor(
      () => s.client.notifications.slice(before).find((n) => n.method === "item/started" && n.params?.item?.type === "commandExecution"),
      30000,
      "the command item",
    );
    await new Promise((r) => setTimeout(r, 1000));
    try {
      const list = await s.client.request("thread/backgroundTerminals/list", { threadId, cursor: null, limit: 50 });
      seen.turnRunning = !s.client.notifications.slice(before).some((n) => n.method === "turn/completed");
      seen.list = list.data.map((t) => ({ processId: t.processId, command: t.command }));
      if (list.data[0]) {
        seen.terminate = await s.client.request("thread/backgroundTerminals/terminate", { threadId, processId: list.data[0].processId });
        const done = await s.client
          .waitFor(() => s.client.notifications.slice(before).find((n) => n.method === "turn/completed"), 20000, "turn/completed")
          .catch(() => null);
        seen.turnEnded = !!done;
        const after = await s.client.request("thread/backgroundTerminals/list", { threadId, cursor: null, limit: 50 });
        seen.afterTerminate = after.data.length;
      }
    } catch (error) {
      seen.error = error.message;
    }
  } finally {
    await shut(s);
  }
  const ok =
    !seen.error && seen.turnRunning && seen.list?.length === 1 && /sleep 120/.test(seen.list[0].command) &&
    seen.terminate?.terminated === true && seen.turnEnded && seen.afterTerminate === 0;
  return {
    status: ok ? "pass" : "fail",
    finding: seen.error
      ? `thread/backgroundTerminals/list during a turn failed: "${seen.error}".`
      : `During the turn the list held ${JSON.stringify(seen.list)}; terminate ${JSON.stringify(seen.terminate)}; the turn ${seen.turnEnded ? "then ended" : "did not end"}; ${seen.afterTerminate} left.`,
    seen,
  };
}

async function checkNetworkApproval({ codexPath }) {
  // The wire shape is read from Codex 0.161.0's generated schema. A live run
  // turns on the managed network proxy with a permissions profile that allows
  // only localhost, and runs curl to a public IP literal (DNS in a cloud
  // workspace can resolve names to private addresses, which are blocked
  // before any approval; a TEST-NET address is blocked the same way, so it
  // cannot reach the allowlist path). The address is example.com's IANA
  // host. Codex's proxy refuses it before a connection is made; if the proxy
  // did not start on some machine, one HTTP GET could leave.
  const seen = {
    schema: {
      request: "item/commandExecution/requestApproval with networkApprovalContext {host, protocol} and proposedNetworkPolicyAmendments [{host, action}]",
      decision: '{ applyNetworkPolicyAmendment: { network_policy_amendment: { host, action: "allow" | "deny" } } }',
      profileVsSandbox: "thread/start and turn/start take `permissions` (a named profile) OR `sandbox`/`sandboxPolicy`, never both",
    },
    serverRequests: [],
    outputs: {},
  };
  const proxyPort = 20000 + Math.floor(Math.random() * 20000);
  const s = await open({
    codexPath,
    configTop: 'default_permissions = "netcheck"',
    configExtra: [
      "[features]",
      "network_proxy = true",
      "",
      "[permissions.netcheck]",
      'extends = ":workspace"',
      "",
      "[permissions.netcheck.network]",
      "enabled = true",
      `proxy_url = "http://127.0.0.1:${proxyPort}"`,
      "enable_socks5 = false",
      'mode = "full"',
      "",
      "[permissions.netcheck.network.domains]",
      '"localhost" = "allow"',
    ].join("\n"),
    script: (body, n) => {
      if (n === 0) return [call("exec_command", { cmd: "curl -sS -m 8 -o /dev/null -w '%{http_code}' http://93.184.215.14/", yield_time_ms: 9000 }, "n1")];
      seen.outputs = callOutputs(body);
      return [text("done")];
    },
    decide: {
      "item/commandExecution/requestApproval": (msg) => {
        seen.serverRequests.push(msg.params);
        return { decision: "decline" };
      },
    },
  });
  try {
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, approvalPolicy: "on-request", approvalsReviewer: "user", permissions: "netcheck" });
    seen.profile = started.activePermissionProfile;
    await s.client.runTurn({ threadId: started.thread.id, input: userInput("fetch"), approvalPolicy: "on-request", approvalsReviewer: "user", permissions: "netcheck" });
  } catch (error) {
    seen.error = error.message;
  } finally {
    await shut(s);
  }
  const live = seen.serverRequests.find((p) => p.networkApprovalContext);
  return {
    status: "info",
    finding: live
      ? `Live network approval: context ${JSON.stringify(live.networkApprovalContext)}, amendments ${JSON.stringify(live.proposedNetworkPolicyAmendments)}, decisions ${JSON.stringify(live.availableDecisions)}.`
      : `No live network approval. Profile ${JSON.stringify(seen.profile)}; the command got: ${JSON.stringify(Object.values(seen.outputs)[0] ?? seen.error ?? "").slice(0, 220)}. ` +
        `Codex's source (core/src/tools/network_approval.rs at rust-v0.161.0) asks only when the turn's permission profile is "Managed" and the approval policy is not "never"; a config profile extending :workspace is blocked without asking here. ` +
        `Wire shape from the schema: ${seen.schema.request}; answer ${seen.schema.decision}. ${seen.schema.profileVsSandbox}.`,
    seen,
  };
}

const PATCH = (file, from, to) =>
  `apply_patch <<'EOF_PATCH'\n*** Begin Patch\n*** Update File: ${file}\n@@\n-${from}\n+${to}\n*** End Patch\nEOF_PATCH\n`;

async function checkApplyPatchIntercept({ codexPath }) {
  const seen = {};
  let execSchema;
  const s = await open({
    codexPath,
    script: (body, n) => {
      if (n === 0) {
        execSchema = (body.tools ?? []).find((t) => t.name === "exec_command");
        seen.toolsOffered = (body.tools ?? []).map((t) => t.name ?? t.type);
        // Only `cmd`, as a small model would send it: no sandbox_permissions.
        return [call("exec_command", { cmd: PATCH("hello.py", '    return "hi"', '    return "hello"') }, "p1")];
      }
      seen.toolOutput = callOutputs(body).p1;
      return [text("done")];
    },
    decide: {
      "item/fileChange/requestApproval": (msg) => {
        seen.approval = msg.params;
        return { decision: "accept" };
      },
    },
  });
  try {
    writeFileSync(path.join(s.dirs.work, "hello.py"), 'def greet():\n    return "hi"\n');
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...SUPERVISED });
    const r = await s.client.runTurn({ threadId: started.thread.id, input: userInput("edit"), ...SUPERVISED_TURN });
    seen.fileChangeItem = r.notifications.find((n) => n.method === "item/completed" && n.params.item?.type === "fileChange")?.params.item;
    seen.fileAfter = readFileSync(path.join(s.dirs.work, "hello.py"), "utf8");
    seen.execRequired = execSchema?.parameters?.required ?? [];
    seen.execProperties = Object.keys(execSchema?.parameters?.properties ?? {});
  } finally {
    await shut(s);
  }
  const ok = seen.fileAfter.includes('"hello"') && seen.fileChangeItem?.status === "completed" && !!seen.approval;
  return {
    status: ok ? "pass" : "fail",
    finding:
      `exec_command with only {cmd} carrying an apply_patch heredoc: ${ok ? "intercepted, approval asked (item/fileChange/requestApproval), file changed, fileChange item emitted" : JSON.stringify(seen).slice(0, 600)}. ` +
      `exec_command required: [${seen.execRequired.join(", ")}]. apply_patch offered as its own tool: ${seen.toolsOffered?.includes("apply_patch") ? "yes" : "no"}.`,
    seen,
  };
}

async function checkAcceptForSession({ codexPath }) {
  const seen = { approvals: [] };
  const s = await open({
    codexPath,
    script: (body, n) => {
      if (n === 0) return [call("exec_command", { cmd: PATCH("a.txt", "one", "two") }, "s1")];
      if (n === 1) return [text("first done")];
      if (n === 2) return [call("exec_command", { cmd: PATCH("a.txt", "two", "three") }, "s2")];
      if (n === 3) return [text("second done")];
      if (n === 4) return [call("exec_command", { cmd: PATCH("b.txt", "one", "two") }, "s3")];
      return [text("third done")];
    },
    decide: {
      "item/fileChange/requestApproval": (msg) => {
        seen.approvals.push(msg.params.itemId);
        return { decision: seen.approvals.length === 1 ? "acceptForSession" : "accept" };
      },
    },
  });
  try {
    writeFileSync(path.join(s.dirs.work, "a.txt"), "one\n");
    writeFileSync(path.join(s.dirs.work, "b.txt"), "one\n");
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...SUPERVISED });
    const threadId = started.thread.id;
    await s.client.runTurn({ threadId, input: userInput("first"), ...SUPERVISED_TURN });
    await s.client.runTurn({ threadId, input: userInput("second"), ...SUPERVISED_TURN });
    await s.client.runTurn({ threadId, input: userInput("third"), ...SUPERVISED_TURN });
    seen.aAfter = readFileSync(path.join(s.dirs.work, "a.txt"), "utf8").trim();
    seen.bAfter = readFileSync(path.join(s.dirs.work, "b.txt"), "utf8").trim();
  } finally {
    await shut(s);
  }
  const sameFileAsked = seen.approvals.includes("s2");
  const otherFileAsked = seen.approvals.includes("s3");
  return {
    status: "info",
    finding:
      `After acceptForSession on a.txt, a second change to a.txt ${sameFileAsked ? "asked again" : "did not ask"}; a change to b.txt in the same session ${otherFileAsked ? "asked" : "did not ask"}. Files: a.txt=${seen.aAfter}, b.txt=${seen.bAfter}.`,
    sameFileAsked,
    otherFileAsked,
    seen,
  };
}

async function checkOutsidePatch({ codexPath }) {
  const seen = {};
  const dirs = makeTempDirs();
  const outside = path.join(dirs.root, "outside.txt");
  writeFileSync(outside, "one\n");
  const s = await open({
    codexPath,
    dirs,
    script: (body, n) => {
      if (n === 0) return [call("exec_command", { cmd: PATCH(outside, "one", "two") }, "o1")];
      seen.toolOutput = callOutputs(body).o1;
      return [text("done")];
    },
    decide: {
      "item/fileChange/requestApproval": (msg) => {
        seen.approval = msg.params;
        return { decision: "accept" };
      },
    },
  });
  try {
    const started = await s.client.request("thread/start", { cwd: dirs.work, ...SUPERVISED });
    const r = await s.client.runTurn({ threadId: started.thread.id, input: userInput("edit outside"), ...SUPERVISED_TURN });
    seen.item = r.notifications.find((n) => n.method === "item/completed" && n.params.item?.type === "fileChange")?.params.item;
  } finally {
    await shut(s);
  }
  seen.after = readFileSync(outside, "utf8").trim();
  const applied = seen.after === "two";
  return {
    status: "info",
    finding: `Supervised (untrusted, read-only), a patch to a file outside the working folder answered "accept": ${applied ? "APPLIED. Codex does not refuse it, so lane 5's path check is the only guard" : "not applied, so Codex is a second guard"}. Approval asked: ${!!seen.approval}${seen.approval?.grantRoot ? `, grantRoot ${seen.approval.grantRoot}` : ""}.`,
    applied,
    seen,
  };
}

function lastTextOf(t) {
  return t.notifications.filter((n) => n.method === "item/completed" && n.params.item?.type === "agentMessage").map((n) => n.params.item.text).at(-1);
}

async function checkCodexMock({ codexPath }) {
  // The Mac setup for session 7: the codex-mock reply logic and its home
  // config, with only the port changed, behind the real Codex in supervised mode.
  const fake = await startFakeResponses({ script: (body) => mockReply(body), model: "codex-mock" });
  const dirs = makeTempDirs("mock-");
  const raw = readFileSync(path.join(HERE, "..", "codex-mock", "config.toml"), "utf8").replace("http://127.0.0.1:11436/v1", fake.baseUrl);
  const seen = { approvals: [] };
  const client = new CodexClient({
    codexPath,
    dirs,
    baseUrl: fake.baseUrl,
    rawConfig: raw,
    onServerRequest: (msg) => {
      if (msg.method === "currentTime/read") return { currentTimeAt: Math.floor(Date.now() / 1000) };
      seen.approvals.push(msg.method);
      return { decision: "accept" };
    },
  });
  try {
    await client.initialize();
    writeFileSync(path.join(dirs.work, "hello.py"), 'def greet():\n    return "hi"\n');
    const started = await client.request("thread/start", { cwd: dirs.work, ...SUPERVISED });
    const threadId = started.thread.id;
    const t1 = await client.runTurn({ threadId, input: userInput('edit hello.py:     return "hi" =>     return "hello"'), ...SUPERVISED_TURN });
    const t2 = await client.runTurn({ threadId, input: userInput("write todo.md: first line"), ...SUPERVISED_TURN });
    const t2b = await client.runTurn({ threadId, input: userInput("write notes/todo.md: first line"), ...SUPERVISED_TURN });
    seen.newFolderReply = lastTextOf(t2b);
    const t3 = await client.runTurn({ threadId, input: userInput("run ls"), ...SUPERVISED_TURN });
    const t4 = await client.runTurn({ threadId, input: userInput("hello"), ...SUPERVISED_TURN });
    seen.hello = readFileSync(path.join(dirs.work, "hello.py"), "utf8");
    seen.todo = existsSync(path.join(dirs.work, "todo.md")) ? readFileSync(path.join(dirs.work, "todo.md"), "utf8") : null;
    seen.newFolderWritten = existsSync(path.join(dirs.work, "notes", "todo.md"));
    seen.replies = [t1, t2, t3, t4].map(lastTextOf);
    seen.items = [t1, t2, t3].map((t) => t.notifications.filter((n) => n.method === "item/completed").map((n) => n.params.item?.type).join(","));
  } finally {
    await client.close();
    await fake.close();
  }
  const ok = seen.hello.includes('"hello"') && seen.todo?.trim() === "first line" && seen.approvals.filter((m) => m === "item/fileChange/requestApproval").length >= 2 && seen.approvals.includes("item/commandExecution/requestApproval");
  return {
    status: ok ? "pass" : "fail",
    finding:
      `codex-mock behind Codex 0.161.0, supervised: approvals ${seen.approvals.join(", ")}; hello.py ${seen.hello.includes('"hello"') ? "edited" : "NOT edited"}; todo.md ${seen.todo ? "written" : "NOT written"}; replies ${JSON.stringify(seen.replies)}. ` +
      `A new file in a NEW folder (notes/todo.md), approved: ${seen.newFolderWritten ? "written" : `not written, Codex replied "${seen.newFolderReply}"`}.`,
    newFolderWritten: seen.newFolderWritten,
    seen,
  };
}

async function checkCommandsAlways({ codexPath }) {
  // Mac test 11.T2.3. MonoCode's Supervised mode is `untrusted`, which asks
  // about every command; an accepted command the sandbox stops is retried
  // outside it without asking. With Commands = Always the chat's turns use
  // `on-request` and the same read-only sandbox instead. This records what
  // Codex then asks about and what it runs.
  const seen = { asked: [], outputs: {} };
  const steps = [
    ["a1", { cmd: "ls && echo LISTED", workdir: "src", sandbox_permissions: "use_default" }],
    ["a2", { cmd: "touch made.txt && echo WROTE" }],
    ["a3", { cmd: "rm -rf src && echo REMOVED", sandbox_permissions: "require_escalated" }],
  ];
  const s = await open({
    codexPath,
    script: (body, n) => {
      Object.assign(seen.outputs, callOutputs(body));
      if (n < steps.length) return [call("exec_command", steps[n][1], steps[n][0])];
      return [text("done")];
    },
    decide: {
      "item/commandExecution/requestApproval": (msg) => {
        seen.asked.push({ itemId: msg.params.itemId, reason: msg.params.reason ?? null });
        return { decision: "decline" };
      },
    },
  });
  try {
    mkdirSync(path.join(s.dirs.work, "src"));
    writeFileSync(path.join(s.dirs.work, "src", "a.txt"), "x");
    const turn = { ...SUPERVISED_TURN, approvalPolicy: "on-request" };
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...SUPERVISED, approvalPolicy: "on-request" });
    for (const [id] of steps) {
      await s.client.runTurn({ threadId: started.thread.id, input: userInput(id), ...turn });
    }
    seen.srcLeft = existsSync(path.join(s.dirs.work, "src", "a.txt"));
    seen.made = existsSync(path.join(s.dirs.work, "made.txt"));
  } finally {
    await shut(s);
  }
  const askedIds = seen.asked.map((a) => a.itemId);
  const listed = String(seen.outputs.a1 ?? "").includes("LISTED");
  const ok = listed && !askedIds.includes("a1") && !askedIds.includes("a2") && askedIds.includes("a3") && !seen.made && seen.srcLeft;
  return {
    status: ok ? "pass" : "fail",
    finding:
      `Supervised sandbox with on-request: a plain command in a subfolder ${askedIds.includes("a1") ? "ASKED" : "ran without asking"} (${listed ? "listed" : "no listing"}); ` +
      `a write the sandbox stops ${askedIds.includes("a2") ? "ASKED" : "was not asked about"} and ${seen.made ? "WAS WRITTEN" : "was refused by the sandbox"}; ` +
      `an escalation ${askedIds.includes("a3") ? "asked" : "DID NOT ASK"} and, declined, ${seen.srcLeft ? "changed nothing" : "RAN"}.`,
    seen,
  };
}

async function checkWebSearchOff({ codexPath }) {
  // Mac test 11.15: MonoCode turns Codex's web search off on a local model
  // route with the thread config `web_search = "disabled"`.
  const toolNames = (body) =>
    (body?.tools ?? []).map((t) => (t.type === "function" || t.type === "namespace" ? t.name : t.type));
  const seen = {};
  for (const [label, config] of [
    ["plain", undefined],
    ["disabled", { web_search: "disabled" }],
  ]) {
    const s = await open({ codexPath, script: () => [text("ok")] });
    try {
      const started = await s.client.request("thread/start", {
        cwd: s.dirs.work,
        ...SUPERVISED,
        ...(config ? { config } : {}),
      });
      await s.client.runTurn({ threadId: started.thread.id, input: userInput("hi"), ...SUPERVISED_TURN });
      seen[label] = toolNames(s.fake.turnRequests()[0]?.body);
    } finally {
      await shut(s);
    }
  }
  const before = (seen.plain ?? []).includes("web_search");
  const after = (seen.disabled ?? []).includes("web_search");
  return {
    status: before && !after ? "pass" : "fail",
    finding:
      before && !after
        ? 'web_search = "disabled" on thread/start removes web_search from the request tools.'
        : `web_search in tools: plain ${before}, disabled ${after}. The override key needs another look.`,
    seen,
  };
}

async function checkMcpStartup({ codexPath }) {
  // Mac test 8.3: Codex does not wait for a thread's MCP servers before the
  // first model request, and reports their startup on the thread.
  const seen = { statuses: [] };
  const s = await open({
    codexPath,
    configExtra: [
      "[mcp_servers.slow]",
      'command = "node"',
      `args = [${JSON.stringify(STUB_MCP)}]`,
      "[mcp_servers.slow.env]",
      'STUB_MCP_INIT_DELAY_MS = "4000"',
    ].join("\n"),
    script: () => [text("ok")],
  });
  try {
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...SUPERVISED });
    await s.client.runTurn({ threadId: started.thread.id, input: userInput("first"), ...SUPERVISED_TURN });
    seen.firstHasTool = (s.fake.turnRequests()[0]?.body?.tools ?? []).some((t) => t.name === "mcp__slow");
    await s.client.waitFor(
      () => s.client.notifications.some((n) => n.method === "mcpServer/startupStatus/updated" && n.params?.status === "ready"),
      15000,
      "slow ready",
    );
    seen.statuses = s.client.notifications
      .filter((n) => n.method === "mcpServer/startupStatus/updated")
      .map((n) => ({ threadId: n.params?.threadId === started.thread.id, name: n.params?.name, status: n.params?.status }));
  } finally {
    await shut(s);
  }
  const reports = seen.statuses.map((x) => x.status).join(",");
  const ok = !seen.firstHasTool && reports === "starting,ready" && seen.statuses.every((x) => x.threadId);
  return {
    status: ok ? "pass" : "fail",
    finding: ok
      ? "A turn sent at once goes out without the slow server's tools; mcpServer/startupStatus/updated reports starting then ready on the thread."
      : `first request had the tools: ${seen.firstHasTool}; reports: ${reports}.`,
    seen,
  };
}

// Mac test 3.7, retest 3: a turn whose last model request returns only a
// reasoning item, after a tool call. Codex 0.161.0 takes last_agent_message
// from the last request alone, so the turn completes with no agent message.
// The app-server notifications are saved as the fixture
// src/integrations/harness/providers/codex/fixtures/reasoningOnlyEnd.json
// (with CLI_CONTRACT_WRITE_FIXTURES=1), which a vitest replays through
// MonoCode's Codex event handling and withEmptyTurnNotice.
export const REASONING_ONLY_FIXTURE = path.join(
  HERE, "..", "..", "..", "src", "integrations", "harness", "providers", "codex", "fixtures", "reasoningOnlyEnd.json",
);

function codexVersion(codexPath) {
  try {
    const PATH = [path.dirname(process.execPath), "/usr/bin", "/bin"].join(":");
    return execFileSync(codexPath, ["--version"], { env: { PATH, HOME: "/nonexistent" }, encoding: "utf8", timeout: 20000 }).trim();
  } catch {
    return null;
  }
}

async function checkReasoningOnlyEnd({ codexPath }) {
  const seen = {};
  const s = await open({
    codexPath,
    script: (body, n) => {
      if (n === 0) return [call("exec_command", { cmd: "cat hello.py" }, "read1")];
      return [reasoning("I read hello.py. I should change the greeting next.")];
    },
  });
  try {
    writeFileSync(path.join(s.dirs.work, "hello.py"), 'def greet():\n    return "hi"\n');
    const started = await s.client.request("thread/start", { cwd: s.dirs.work, ...FULL_ACCESS });
    const threadId = started.thread.id;
    const before = s.client.notifications.length;
    const turn = await s.client.runTurn({ threadId, input: userInput("Change the greeting in hello.py to hello."), ...FULL_ACCESS_TURN });
    const notes = s.client.notifications.slice(before);
    seen.modelRequests = s.fake.turnRequests().length;
    seen.methods = notes.map((n) => `${n.method}${n.params?.item?.type ? `:${n.params.item.type}` : ""}`);
    seen.items = notes.filter((n) => n.method === "item/completed").map((n) => n.params?.item?.type);
    seen.agentMessages = seen.items.filter((t) => t === "agentMessage").length;
    seen.completedTurn = { status: turn.completed?.turn?.status, items: turn.completed?.turn?.items?.length ?? 0 };
    if (process.env.CLI_CONTRACT_WRITE_FIXTURES === "1") {
      // Paths in the temp home are this machine's; the replay needs none of them.
      // Longest first, so a path inside another is replaced whole. The
      // write is refused if this machine's user name is still in it (review
      // of the 15 Oct pass, G6).
      const swaps = [
        [s.dirs.work, "/work"],
        [s.dirs.root, "/run"],
        [path.dirname(path.resolve(codexPath)), "/codex-bin"],
        [path.dirname(process.execPath), "/node-bin"],
        [homedir(), "/home"],
      ].sort((a, b) => b[0].length - a[0].length);
      const scrub = (value) => {
        let text = JSON.stringify(value);
        for (const [from, to] of swaps) if (from && from !== "/") text = text.split(from).join(to);
        const user = userInfo().username;
        const escaped = user.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
        if (user && user.length > 2 && new RegExp(`\\b${escaped}\\b`).test(text)) {
          throw new Error(`fixture still names the user ${user}; not written`);
        }
        return JSON.parse(text);
      };
      mkdirSync(path.dirname(REASONING_ONLY_FIXTURE), { recursive: true });
      writeFileSync(
        REASONING_ONLY_FIXTURE,
        `${JSON.stringify(
          {
            about:
              "Codex 0.161.0 app-server notifications for one turn: request 1 is one exec_command call (cat hello.py), request 2 a reasoning item only. Written by tests/cli-contract check reasoning-only-end. Full access, so no approval request.",
            codex: codexVersion(codexPath),
            threadId,
            turnId: turn.turnId,
            notifications: scrub(notes),
          },
          null,
          2,
        )}\n`,
      );
      seen.fixture = REASONING_ONLY_FIXTURE;
    }
  } finally {
    await shut(s);
  }
  const ranTool = seen.items.includes("commandExecution");
  const ok = seen.modelRequests >= 2 && ranTool && seen.agentMessages === 0 && seen.completedTurn.status === "completed";
  return {
    status: ok ? "pass" : "fail",
    finding: ok
      ? `After an exec_command step, a last request that returns only reasoning completes the turn with no agentMessage item (turn/completed status ${seen.completedTurn.status}, ${seen.completedTurn.items} items). MonoCode must say so itself.`
      : `Expected a completed turn with a commandExecution and no agentMessage; saw items [${seen.items.join(", ")}], ${seen.modelRequests} model requests, status ${seen.completedTurn.status}.`,
    seen,
  };
}

export const codexChecks = [
  { id: "check-3", title: "Codex developerInstructions on thread/start, resume and turn/start", flag: "capabilities: codex instructions \"session\"", run: check3 },
  { id: "check-4", title: "Codex thread/revert three turns back", flag: "capabilities: codex rewind \"anyTurn\"", run: check4 },
  { id: "check-5", title: "Codex thread/fork with lastTurnId", flag: "capabilities: codex fork \"atTurn\"", run: check5 },
  { id: "check-10", title: "Codex MCP tool call confirmation under supervised", flag: "#38b on Codex", run: check10 },
  { id: "mcp-flat-name", title: "Codex refuses a flat MCP tool name", flag: "gateway connectors on the local home", run: checkMcpFlatName },
  { id: "bg-terminals", title: "Codex thread/backgroundTerminals/list and /terminate", flag: "capabilities: codex backgroundList, backgroundStop", run: checkBackground },
  { id: "bg-terminals-mid-turn", title: "Codex lists and terminates a command while its turn runs", flag: "Mac test 1.8: Running section polls during the turn", run: checkBackgroundMidTurn },
  { id: "net-approval", title: "Codex network approval and policy amendment shape", flag: "lane 5 always allow this host", run: checkNetworkApproval },
  { id: "apply-patch-heredoc", title: "Codex intercepts an apply_patch heredoc in exec_command", flag: "session 10 premise", run: checkApplyPatchIntercept },
  { id: "accept-for-session", title: "Codex acceptForSession on a file change", flag: "lane 5 Allow for this chat", run: checkAcceptForSession },
  { id: "codex-mock", title: "codex-mock home and server behind the real Codex", flag: "session 7 setup", run: checkCodexMock },
  { id: "outside-patch", title: "Codex supervised patch outside the working folder", flag: "lane 5 folder path check", run: checkOutsidePatch },
  { id: "web-search-off", title: "Codex web search off through thread config", flag: "Mac test 11.15 codexWebSearchConfig", run: checkWebSearchOff },
  { id: "mcp-startup", title: "Codex MCP startup reports and the first turn", flag: "Mac test 8.3 codexMcpStartup", run: checkMcpStartup },
  { id: "commands-always", title: "Codex Commands = Always: on-request with the read-only sandbox", flag: "Mac test 11.T2.3 commandsAlwaysPolicy", run: checkCommandsAlways },
  { id: "reasoning-only-end", title: "Codex ends a turn on a reasoning-only last step with no agent message", flag: "Mac test 3.7 withEmptyTurnNotice steps notice", run: checkReasoningOnlyEnd },
];

