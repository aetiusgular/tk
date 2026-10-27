// End-to-end check of the edit adapter (session 10): the real Codex, the
// adapter proxy built from `src-tauri/examples/edit_adapter_proxy.rs`, and the
// fake Responses server scripted to call `edit_file` and `write_file`.
//
// Codex's home points its provider at the fake server. The thread opens with
// a `config` override of that provider's `base_url` to the proxy, as MonoCode
// does, so the check also shows the override reaches Codex.
//
// Pass means: the model was offered the two tools; each call reached Codex as
// an `exec_command` that Codex's patch path applied, with a file change item;
// the next request showed the model its own call; and the refused calls came
// back to it as plain errors.

import { writeFileSync, readFileSync, existsSync } from "node:fs";
import { spawn } from "node:child_process";
import path from "node:path";
import { startFakeResponses, text, call, callOutputs } from "../lib/fakeResponses.mjs";
import { CodexClient, makeTempDirs, PROVIDER_ID, SUPERVISED, SUPERVISED_TURN, userInput } from "../lib/codexDriver.mjs";

/** Starts the adapter and returns its base URL and a stop function. */
async function startAdapter(bin, upstream, root) {
  const child = spawn(bin, ["--upstream", upstream, "--root", root], { stdio: ["pipe", "pipe", "pipe"] });
  let out = "";
  let err = "";
  child.stderr.on("data", (d) => (err += d));
  const base = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`adapter did not start: ${err}`)), 60_000);
    child.stdout.on("data", (d) => {
      out += d;
      const line = out.split("\n")[0];
      if (out.includes("\n")) {
        clearTimeout(timer);
        resolve(line.trim());
      }
    });
    child.on("exit", (code) => reject(new Error(`adapter exited ${code}: ${err}`)));
  });
  return {
    base,
    stop: () => {
      child.stdin.end();
      child.kill();
    },
  };
}

/** The history entry the model sees for a call id, from a request body. */
function historyCall(body, callId) {
  return (body?.input ?? []).find((i) => i.type === "function_call" && i.call_id === callId);
}

const STEPS = [
  // Substring of an indented line.
  { id: "e1", name: "edit_file", args: { path: "hello.py", old_text: '"hi"', new_text: '"hello"' } },
  // A CRLF file edited with plain line breaks.
  { id: "e2", name: "edit_file", args: { path: "dos.txt", old_text: "one\ntwo", new_text: "one\nTWO" } },
  // A name with a space, in an existing folder.
  { id: "w1", name: "write_file", args: { path: "docs/new notes.md", content: "# New\n\nbody\n" } },
  // Replace a whole existing file.
  { id: "w2", name: "write_file", args: { path: "hello.py", content: "print('replaced')\n" } },
  // Refused: outside the folder.
  { id: "x1", name: "edit_file", args: { path: "../outside.txt", old_text: "a", new_text: "b" } },
  // Base indentation dropped by the model: matched, file's indentation kept.
  { id: "i1", name: "edit_file", args: { path: "cls.py", old_text: "def f(self):\n    return 1", new_text: "def f(self):\n    return 2" } },
  // Text not in the file: Codex's verifier refuses it, with no approval card.
  { id: "x2", name: "edit_file", args: { path: "dos.txt", old_text: "nothing like this", new_text: "x" } },
  // Refused: the text is in the file twice.
  { id: "x3", name: "edit_file", args: { path: "twice.txt", old_text: "x = 1", new_text: "x = 2" } },
];

export async function checkEditAdapter({ codexPath }) {
  const bin = process.env.EDIT_ADAPTER_BIN;
  if (!bin || !existsSync(bin)) {
    return { status: "skip", finding: "edit adapter binary not given (--edit-adapter)" };
  }
  const seen = { requests: [], approvals: [], outputs: {}, history: {}, phase: 1, resumeHistory: {} };
  const fake = await startFakeResponses({
    script: (body) => {
      const offered = (body.tools ?? []).map((t) => t.name ?? t.type);
      const outputs = callOutputs(body);
      seen.requests.push({ phase: seen.phase, offered, outputs: Object.keys(outputs) });
      if (seen.phase === 2) {
        // After a restart: a new adapter with no memory of the calls, and a
        // resumed thread. The history must still show the model its calls.
        for (const step of STEPS) seen.resumeHistory[step.id] = historyCall(body, step.id)?.name;
        if (outputs.r1 === undefined) return [call("edit_file", { path: "hello.py", old_text: "replaced", new_text: "resumed" }, "r1")];
        seen.outputs.r1 = outputs.r1;
        return [text("done again")];
      }
      for (const step of STEPS) {
        if (outputs[step.id] !== undefined) {
          seen.outputs[step.id] = outputs[step.id];
          seen.history[step.id] = historyCall(body, step.id)?.name;
          continue;
        }
        return [call(step.name, step.args, step.id)];
      }
      return [text("done")];
    },
  });
  const dirs = makeTempDirs();
  writeFileSync(path.join(dirs.work, "hello.py"), 'def greet():\n    return "hi"\n');
  writeFileSync(path.join(dirs.work, "dos.txt"), "one\r\ntwo\r\nthree\r\n");
  writeFileSync(path.join(dirs.work, "cls.py"), "class A:\n    def f(self):\n        return 1\n");
  writeFileSync(path.join(dirs.work, "twice.txt"), "x = 1\nx = 1\n");
  writeFileSync(path.join(dirs.work, "..", "outside.txt"), "a\n");
  const { mkdirSync } = await import("node:fs");
  mkdirSync(path.join(dirs.work, "docs"), { recursive: true });
  const adapter = await startAdapter(bin, fake.baseUrl, dirs.work);
  const client = new CodexClient({
    codexPath,
    dirs,
    baseUrl: fake.baseUrl,
    onServerRequest: (msg) => {
      if (msg.method === "currentTime/read") return { currentTimeAt: Math.floor(Date.now() / 1000) };
      if (msg.method.endsWith("/requestApproval")) {
        seen.approvals.push({ method: msg.method, command: msg.params?.command ?? null });
        return { decision: "accept" };
      }
      return {};
    },
  });
  try {
    await client.initialize();
    const started = await client.request("thread/start", {
      cwd: dirs.work,
      ...SUPERVISED,
      config: { [`model_providers.${PROVIDER_ID}.base_url`]: adapter.base },
    });
    const r = await client.runTurn({ threadId: started.thread.id, input: userInput("make the edits"), ...SUPERVISED_TURN });
    seen.fileChanges = r.notifications
      .filter((n) => n.method === "item/completed" && n.params.item?.type === "fileChange")
      .map((n) => ({ status: n.params.item.status, paths: (n.params.item.changes ?? []).map((c) => c.path) }));
    seen.hello = readFileSync(path.join(dirs.work, "hello.py"), "utf8");
    seen.dos = readFileSync(path.join(dirs.work, "dos.txt"), "utf8");
    seen.notes = existsSync(path.join(dirs.work, "docs", "new notes.md"))
      ? readFileSync(path.join(dirs.work, "docs", "new notes.md"), "utf8")
      : null;
    seen.outside = readFileSync(path.join(dirs.work, "..", "outside.txt"), "utf8");
    seen.cls = readFileSync(path.join(dirs.work, "cls.py"), "utf8");
    seen.twice = readFileSync(path.join(dirs.work, "twice.txt"), "utf8");
    seen.threadId = started.thread.id;
  } finally {
    adapter.stop();
    try {
      await client.close();
    } catch {}
  }

  // Restart both, then resume the thread through the new adapter.
  let adapter2;
  let client2;
  try {
    seen.phase = 2;
    adapter2 = await startAdapter(bin, fake.baseUrl, dirs.work);
    seen.newRoute = adapter2.base !== adapter.base;
    client2 = new CodexClient({
      codexPath,
      dirs,
      baseUrl: fake.baseUrl,
      onServerRequest: (msg) =>
        msg.method === "currentTime/read" ? { currentTimeAt: Math.floor(Date.now() / 1000) } : msg.method.endsWith("/requestApproval") ? { decision: "accept" } : {},
    });
    await client2.initialize();
    await client2.request("thread/resume", {
      threadId: seen.threadId,
      cwd: dirs.work,
      ...SUPERVISED,
      config: { [`model_providers.${PROVIDER_ID}.base_url`]: adapter2.base },
    });
    await client2.runTurn({ threadId: seen.threadId, input: userInput("one more"), ...SUPERVISED_TURN });
    seen.helloAfterResume = readFileSync(path.join(dirs.work, "hello.py"), "utf8");
  } finally {
    adapter2?.stop();
    try {
      await client2?.close();
    } catch {}
    await fake.close();
  }

  const viaProxy = seen.requests.length > 0 && seen.requests[0].offered.includes("edit_file");
  const results = {
    "override reaches Codex and tools added": viaProxy && seen.requests[0].offered.includes("write_file"),
    "e1 indented substring edit applied": seen.history.e1 === "edit_file",
    "e2 CRLF file edited": /TWO/.test(seen.dos) && seen.dos.endsWith("three\r\n"),
    "w1 new file with a space applied": seen.notes === "# New\n\nbody\n",
    "w2 whole existing file replaced": seen.hello === "print('replaced')\n",
    "x1 path escape refused, file untouched": seen.outside === "a\n" && /edit not applied/.test(seen.outputs.x1 ?? ""),
    "i1 wrong base indentation matched, file's indentation kept": seen.cls === "class A:\n    def f(self):\n        return 2\n",
    "x2 missing text refused by Codex's verifier, no card": /apply_patch verification failed: Failed to find expected lines/.test(seen.outputs.x2 ?? "") &&
      !seen.approvals.some((a) => /nothing like this/.test(JSON.stringify(a))),
    "x3 ambiguous text refused, file untouched": seen.twice === "x = 1\nx = 1\n" && /appears 2 times/.test(seen.outputs.x3 ?? ""),
    "history shows the model its own calls": STEPS.every((s) => seen.history[s.id] === s.name),
    "file change items emitted": seen.fileChanges?.filter((c) => c.status === "completed").length >= 5,
    "after a restart, resume goes through the new adapter": seen.newRoute && seen.requests.some((r) => r.phase === 2 && r.offered.includes("edit_file")),
    "after a restart, history still shows the model's calls": ["e1", "e2", "w1", "w2", "i1"].every((id) => seen.resumeHistory[id] === STEPS.find((s) => s.id === id).name),
    "after a restart, a new edit applies": seen.helloAfterResume === "print('resumed')\n",
  };
  const failed = Object.entries(results).filter(([, ok]) => !ok).map(([name]) => name);
  const refusalApprovals = seen.approvals.filter((a) => /edit not applied/.test(JSON.stringify(a.command ?? "")));
  return {
    status: failed.length ? "fail" : "pass",
    finding:
      (failed.length ? `Failed: ${failed.join("; ")}. ` : `All ${Object.keys(results).length} parts pass: ${Object.keys(results).join("; ")}. `) +
      `Approvals asked: ${seen.approvals.length} (${[...new Set(seen.approvals.map((a) => a.method))].join(", ")}); ` +
      `for a refusal echo: ${refusalApprovals.length}. CRLF file after the edit: ${JSON.stringify(seen.dos)}. ` +
      `File change items: ${seen.fileChanges?.length ?? 0}.`,
    seen,
  };
}

/** MonoCode's Full access mode (codexProtocol.ts). */
const FULL_ACCESS = { approvalPolicy: "on-request", sandbox: "danger-full-access", approvalsReviewer: "user" };
const FULL_ACCESS_TURN = { approvalPolicy: "on-request", approvalsReviewer: "user", sandboxPolicy: { type: "dangerFullAccess" } };

/** One edit in Full access: applied through Codex with no card (corner case 17). */
export async function checkEditAdapterFullAccess({ codexPath }) {
  const bin = process.env.EDIT_ADAPTER_BIN;
  if (!bin || !existsSync(bin)) {
    return { status: "skip", finding: "edit adapter binary not given (--edit-adapter)" };
  }
  const seen = { approvals: 0 };
  const fake = await startFakeResponses({
    script: (body) => {
      const outputs = callOutputs(body);
      if (outputs.f1 === undefined) return [call("edit_file", { path: "hello.py", old_text: '"hi"', new_text: '"full"' }, "f1")];
      seen.output = outputs.f1;
      return [text("done")];
    },
  });
  const dirs = makeTempDirs();
  writeFileSync(path.join(dirs.work, "hello.py"), 'def greet():\n    return "hi"\n');
  const adapter = await startAdapter(bin, fake.baseUrl, dirs.work);
  const client = new CodexClient({
    codexPath,
    dirs,
    baseUrl: fake.baseUrl,
    onServerRequest: (msg) => {
      if (msg.method === "currentTime/read") return { currentTimeAt: Math.floor(Date.now() / 1000) };
      if (msg.method.endsWith("/requestApproval")) {
        seen.approvals++;
        return { decision: "accept" };
      }
      return {};
    },
  });
  try {
    await client.initialize();
    const started = await client.request("thread/start", {
      cwd: dirs.work,
      ...FULL_ACCESS,
      config: { [`model_providers.${PROVIDER_ID}.base_url`]: adapter.base },
    });
    const r = await client.runTurn({ threadId: started.thread.id, input: userInput("edit"), ...FULL_ACCESS_TURN });
    seen.fileChange = r.notifications.some((n) => n.method === "item/completed" && n.params.item?.type === "fileChange");
    seen.after = readFileSync(path.join(dirs.work, "hello.py"), "utf8");
  } finally {
    adapter.stop();
    try {
      await client.close();
    } catch {}
    await fake.close();
  }
  const ok = seen.after.includes('return "full"') && seen.fileChange && seen.approvals === 0;
  return {
    status: ok ? "pass" : "fail",
    finding: `Full access: file ${ok ? "changed" : JSON.stringify(seen.after)}, file change item ${seen.fileChange}, approvals ${seen.approvals}.`,
    seen,
  };
}

export const editAdapterChecks = [
  { id: "edit-adapter", title: "The edit adapter turns edit_file and write_file into Codex's patch path", flag: "session 10", run: checkEditAdapter },
  { id: "edit-adapter-full-access", title: "The edit adapter in Full access", flag: "session 10", run: checkEditAdapterFullAccess },
];
