// Starts Claude Code in MonoCode's spawn shape (stream-json in and out, no
// -p) against a fake Messages server, in an isolated home.
//
// The flags mirror buildClaudeSpawnArgs at 53e1b90 for an interactive
// session. The environment is empty except for HOME, CLAUDE_CONFIG_DIR,
// ANTHROPIC_BASE_URL at the fake, a dummy ANTHROPIC_API_KEY and the
// telemetry switches. This workspace's credentials never reach the child.

import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { isolatedEnv } from "./codexDriver.mjs";

export const MONOCODE_FLAGS = [
  "--output-format",
  "stream-json",
  "--verbose",
  "--input-format",
  "stream-json",
  "--permission-prompt-tool",
  "stdio",
  "--include-partial-messages",
  "--setting-sources=user,project,local",
];

export const DUMMY_KEY = "sk-ant-api03-contract-test-dummy-key-not-real";

export function claudeDirs(dirs) {
  const config = path.join(dirs.root, "claude-config");
  mkdirSync(config, { recursive: true });
  return config;
}

export class ClaudeSession {
  /**
   * @param {object} o
   * @param {string} o.claudePath
   * @param {object} o.dirs       from makeTempDirs()
   * @param {string} o.baseUrl    the fake server
   * @param {object} [o.settings] merged into --settings with MonoCode's env
   * @param {string[]} [o.args]   extra flags, after MonoCode's
   * @param {object} [o.env]      extra environment
   */
  constructor({ claudePath, dirs, baseUrl, settings = {}, args = [], env = {}, onControl }) {
    this.configDir = claudeDirs(dirs);
    const settingsJson = JSON.stringify({ ...settings, env: { CLAUDE_CODE_ENABLE_TODO_TOOLS: "1", ...(settings.env ?? {}) } });
    this.args = [...MONOCODE_FLAGS, "--settings", settingsJson, "--model", "claude-sonnet-4-5", ...args];
    this.events = [];
    this.stderr = "";
    this.waiters = [];
    this.onControl = onControl ?? ((req) => ({ behavior: "allow", updatedInput: req.request?.input ?? {} }));
    this.child = spawn(claudePath, this.args, {
      cwd: dirs.work,
      env: isolatedEnv({
        home: dirs.home,
        extra: {
          CLAUDE_CONFIG_DIR: this.configDir,
          ANTHROPIC_BASE_URL: baseUrl,
          ANTHROPIC_API_KEY: DUMMY_KEY,
          CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
          CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY: "1",
          DISABLE_BUG_COMMAND: "1",
          ...env,
        },
      }),
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.exited = new Promise((resolve) => this.child.on("exit", (code) => resolve(code)));
    this.child.stderr.on("data", (d) => {
      this.stderr += d.toString();
      if (this.stderr.length > 200_000) this.stderr = this.stderr.slice(-100_000);
    });
    let buf = "";
    this.child.stdout.on("data", (chunk) => {
      buf += chunk.toString();
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (!line.startsWith("{")) continue;
        let msg;
        try {
          msg = JSON.parse(line);
        } catch {
          continue;
        }
        this.events.push(msg);
        if (msg.type === "control_request") this.#answerControl(msg);
        for (const w of [...this.waiters]) w();
      }
    });
  }

  #answerControl(msg) {
    const response = this.onControl(msg);
    this.write({
      type: "control_response",
      response: { subtype: "success", request_id: msg.request_id, response },
    });
  }

  write(obj) {
    this.child.stdin.write(`${JSON.stringify(obj)}\n`);
  }

  waitFor(predicate, timeoutMs = 90000, label = "event") {
    return new Promise((resolve, reject) => {
      const check = () => {
        const v = predicate();
        if (v) {
          done();
          resolve(v);
        }
      };
      const timer = setTimeout(() => {
        done();
        const tail = this.events.slice(-5).map((e) => `${e.type}${e.subtype ? `/${e.subtype}` : ""}`).join(", ");
        reject(new Error(`timed out waiting for ${label}. Recent events: ${tail || "none"}. Stderr: ${this.stderr.slice(-300) || "none"}`));
      }, timeoutMs);
      const done = () => {
        clearTimeout(timer);
        this.waiters = this.waiters.filter((w) => w !== check);
      };
      this.waiters.push(check);
      check();
    });
  }

  /** Sends one user message (MonoCode's buildClaudeUserMessage shape) and waits for its result. */
  async send(text, timeoutMs = 90000) {
    const before = this.events.length;
    this.write({ type: "user", session_id: "", parent_tool_use_id: null, message: { role: "user", content: [{ type: "text", text }] } });
    const result = await this.waitFor(() => this.events.slice(before).find((e) => e.type === "result"), timeoutMs, `result for "${text}"`);
    return { result, events: this.events.slice(before) };
  }

  sessionId() {
    return this.events.find((e) => e.type === "system" && e.subtype === "init")?.session_id ?? this.events.find((e) => e.session_id)?.session_id;
  }

  async close() {
    try {
      this.child.stdin.end();
    } catch {}
    const timer = setTimeout(() => this.child.kill("SIGKILL"), 4000);
    const quick = setTimeout(() => this.child.kill("SIGTERM"), 1500);
    await this.exited;
    clearTimeout(timer);
    clearTimeout(quick);
  }
}

/** Writes a project file into the session's working folder. */
export function writeProjectFile(dirs, rel, content) {
  const file = path.join(dirs.work, rel);
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, content);
  return file;
}
