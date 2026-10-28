// Lane I's first contract check, written by integration w1 to prove
// lib/fakeGemini.mjs against the real Gemini CLI before lane I builds on it.
// Lane I owns this folder and may keep, change or replace it.
//
// One headless run (`gemini -p`, API-key auth, GOOGLE_GEMINI_BASE_URL at the
// fake, the workspace trusted, --approval-mode yolo so no prompt blocks): the
// fake asks for one `run_shell_command`, then ends the turn. Pass: the CLI ran
// the command and sent its output back as a functionResponse, and every side
// call it made was answered.
//
//   node tests/cli-contract/run.mjs --cli gemini=/path/to/gemini --only gemini-fake

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { functionCall, functionResponses, startFakeGemini, text, toolNames } from "../../lib/fakeGemini.mjs";

function run(bin, args, env, cwd, seconds = 120) {
  return new Promise((resolve) => {
    const child = spawn(bin, args, { env, cwd, stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    const timer = setTimeout(() => child.kill("SIGKILL"), seconds * 1000);
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ code, out, err });
    });
  });
}

export const checks = [
  {
    id: "gemini-fake",
    title: "The fake Gemini API serves a headless Gemini CLI turn with a tool call",
    harness: "gemini",
    async run({ binPath }) {
      const marker = "tk-gemini-ok";
      const fake = await startFakeGemini({
        script: (body, n) => (n === 0 ? [functionCall("run_shell_command", { command: `echo ${marker}` })] : [text("done")]),
      });
      const root = mkdtempSync(path.join(process.env.CLI_CONTRACT_TMP ?? tmpdir(), "gemini-"));
      const home = path.join(root, "home");
      const work = path.join(root, "work");
      mkdirSync(path.join(home, ".gemini"), { recursive: true });
      mkdirSync(work);
      writeFileSync(path.join(home, ".gemini", "settings.json"), JSON.stringify({ security: { auth: { selectedType: "gemini-api-key" } } }));
      const env = {
        PATH: process.env.PATH,
        HOME: home,
        GEMINI_API_KEY: "fake-key",
        GOOGLE_GEMINI_BASE_URL: fake.baseUrl,
        GEMINI_CLI_TRUST_WORKSPACE: "true",
        GEMINI_CLI_NO_RELAUNCH: "true",
        NO_COLOR: "1",
      };
      try {
        const r = await run(binPath, ["-p", "run the step", "--approval-mode", "yolo", "--output-format", "json"], env, work);
        const turns = fake.turnRequests();
        const responses = turns.flatMap((t) => functionResponses(t.body));
        const sawOutput = responses.some((f) => JSON.stringify(f).includes(marker));
        const sides = fake.requests.filter((q) => q.kind === "side").length;
        const other = fake.requests.filter((q) => q.kind === "other").map((q) => q.url);
        const offered = turns[0] ? toolNames(turns[0].body) : [];
        const ok = r.code === 0 && turns.length >= 2 && sawOutput && other.length === 0;
        return {
          status: ok ? "pass" : "fail",
          finding: `exit ${r.code}; ${turns.length} turn request(s), ${sides} side call(s) answered from their schema, unrouted ${JSON.stringify(other)}; run_shell_command offered: ${offered.includes("run_shell_command")}; its output came back as a functionResponse: ${sawOutput}.${ok ? "" : ` stderr: ${r.err.slice(-300)}`}`,
        };
      } finally {
        await fake.close();
        rmSync(root, { recursive: true, force: true });
      }
    },
  },
];
