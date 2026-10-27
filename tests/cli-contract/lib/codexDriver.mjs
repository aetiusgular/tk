// Starts `codex app-server` in an isolated home pointed at a fake Responses
// server, and speaks its JSON-RPC over stdio the way MonoCode does
// (initialize with experimentalApi, then thread/start and turn/start).
//
// Isolation: the child gets an empty environment plus HOME, CODEX_HOME, a
// short PATH and the telemetry switches. Nothing from this workspace's own
// environment, credentials or Codex home reaches it.

import { spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, realpathSync } from "node:fs";
import { tmpdir, homedir } from "node:os";
import path from "node:path";

export const PROVIDER_ID = "fake_local";

/** The local home's config, as on the Mac, with the base URL at the fake. */
export function codexHomeConfig({ baseUrl, model = "fake-model", contextWindow = 16384, extra = "", top = "" }) {
  // Top-level keys must sit above the first [table].
  return [
    top,
    `model_provider = "${PROVIDER_ID}"`,
    `model = "${model}"`,
    `model_context_window = ${contextWindow}`,
    "",
    `[model_providers.${PROVIDER_ID}]`,
    `name = "${PROVIDER_ID}"`,
    `base_url = "${baseUrl}"`,
    `wire_api = "responses"`,
    "requires_openai_auth = false",
    "supports_websockets = false",
    "stream_idle_timeout_ms = 60000",
    "request_max_retries = 0",
    "stream_max_retries = 0",
    "",
    "[analytics]",
    "enabled = false",
    "",
    "[otel]",
    'exporter = "none"',
    'metrics_exporter = "none"',
    "",
    extra,
  ].join("\n");
}

/**
 * Where test homes go. Codex refuses to create its sandbox helper binaries
 * when CODEX_HOME is under /tmp, so on Linux the default is ~/.cache.
 */
export function runsDir() {
  if (process.env.CLI_CONTRACT_TMP) return process.env.CLI_CONTRACT_TMP;
  const t = tmpdir();
  return t === "/tmp" || t.startsWith("/tmp/") ? path.join(homedir(), ".cache", "cli-contract-runs") : t;
}

export function makeTempDirs(prefix = "cc-") {
  mkdirSync(runsDir(), { recursive: true });
  const root = realpathSync(mkdtempSync(path.join(runsDir(), prefix)));
  const home = path.join(root, "home");
  const codexHome = path.join(root, "codex-home");
  const work = path.join(root, "work");
  for (const dir of [home, codexHome, work]) mkdirSync(dir, { recursive: true });
  return { root, home, codexHome, work };
}

export function isolatedEnv({ home, extra = {} }) {
  return {
    HOME: home,
    // The running Node's folder first: the CLIs' launchers and the stub MCP
    // server need `node`, which is not in /usr/local/bin on every machine.
    PATH: [path.dirname(process.execPath), "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].join(":"),
    LANG: "C.UTF-8",
    NO_COLOR: "1",
    // Telemetry and update checks off.
    DO_NOT_TRACK: "1",
    CODEX_DISABLE_UPDATE_CHECK: "1",
    DISABLE_TELEMETRY: "1",
    DISABLE_ERROR_REPORTING: "1",
    DISABLE_AUTOUPDATER: "1",
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
    ...extra,
  };
}

export class CodexClient {
  constructor({ codexPath, dirs, baseUrl, configExtra = "", configTop = "", rawConfig, args = [], onServerRequest }) {
    writeFileSync(
      path.join(dirs.codexHome, "config.toml"),
      rawConfig ?? codexHomeConfig({ baseUrl, extra: configExtra, top: configTop }),
    );
    this.dirs = dirs;
    this.notifications = [];
    this.serverRequests = [];
    this.stderr = "";
    this.pending = new Map();
    this.nextId = 1;
    this.waiters = [];
    this.onServerRequest = onServerRequest ?? (() => ({ decision: "decline" }));
    this.child = spawn(codexPath, ["app-server", ...args], {
      cwd: dirs.work,
      env: isolatedEnv({ home: dirs.home, extra: { CODEX_HOME: dirs.codexHome } }),
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
        if (line) this.#onLine(line);
      }
    });
  }

  #onLine(line) {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    if (msg.id !== undefined && msg.method) {
      this.serverRequests.push(msg);
      Promise.resolve(this.onServerRequest(msg))
        .then((result) => this.#send({ id: msg.id, result }))
        .catch((error) => this.#send({ id: msg.id, error: { code: -32000, message: String(error) } }));
      this.#wake();
      return;
    }
    if (msg.id !== undefined) {
      const p = this.pending.get(msg.id);
      if (p) {
        this.pending.delete(msg.id);
        if (msg.error) p.reject(Object.assign(new Error(msg.error.message ?? "rpc error"), { rpc: msg.error }));
        else p.resolve(msg.result);
      }
      return;
    }
    if (msg.method) {
      this.notifications.push(msg);
      this.#wake();
    }
  }

  #wake() {
    for (const w of [...this.waiters]) w();
  }

  #send(obj) {
    this.child.stdin.write(`${JSON.stringify(obj)}\n`);
  }

  request(method, params, timeoutMs = 30000) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out after ${timeoutMs} ms. ${this.debugTail()}`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => (clearTimeout(timer), resolve(v)),
        reject: (e) => (clearTimeout(timer), reject(e)),
      });
      this.#send({ id, method, params });
    });
  }

  /** The last notifications and stderr lines, for a timeout message. */
  debugTail() {
    const recent = this.notifications.slice(-6).map((n) => n.method).join(", ");
    const errs = this.stderr
      .split("\n")
      .filter((l) => /ERROR|WARN|panic/i.test(l) && !/bubblewrap/.test(l))
      .slice(-3)
      .join(" / ");
    return `Recent notifications: ${recent || "none"}. Stderr: ${errs || "none"}`;
  }

  notify(method, params) {
    this.#send(params === undefined ? { method } : { method, params });
  }

  /** Waits until `predicate()` is truthy, re-checked on every message. */
  waitFor(predicate, timeoutMs = 60000, label = "condition") {
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
        reject(new Error(`timed out waiting for ${label}`));
      }, timeoutMs);
      const done = () => {
        clearTimeout(timer);
        this.waiters = this.waiters.filter((w) => w !== check);
      };
      this.waiters.push(check);
      check();
    });
  }

  async initialize() {
    const result = await this.request("initialize", {
      clientInfo: { name: "monocode", title: "MonoCode", version: "0.1.0" },
      capabilities: { experimentalApi: true },
    });
    this.notify("initialized");
    return result;
  }

  /** Starts a turn and waits for its turn/completed notification. */
  async runTurn(params, timeoutMs = 90000) {
    const before = this.notifications.length;
    const started = await this.request("turn/start", params, timeoutMs);
    const turnId = started?.turn?.id;
    const done = await this.waitFor(
      () =>
        this.notifications
          .slice(before)
          .find((n) => n.method === "turn/completed" && (!turnId || n.params?.turn?.id === turnId)),
      timeoutMs,
      `turn/completed for ${turnId} (${this.debugTail()})`,
    );
    return { turnId, completed: done.params, notifications: this.notifications.slice(before) };
  }

  async close() {
    try {
      this.child.stdin.end();
    } catch {}
    const timer = setTimeout(() => this.child.kill("SIGKILL"), 3000);
    this.child.kill("SIGTERM");
    await this.exited;
    clearTimeout(timer);
  }
}

/** MonoCode's supervised mode on thread/start (codexProtocol.ts at 53e1b90). */
export const SUPERVISED = {
  approvalPolicy: "untrusted",
  sandbox: "read-only",
  approvalsReviewer: "user",
};

export const SUPERVISED_TURN = {
  approvalPolicy: "untrusted",
  approvalsReviewer: "user",
  sandboxPolicy: { type: "readOnly" },
};

export function userInput(textValue) {
  return [{ type: "text", text: textValue }];
}
