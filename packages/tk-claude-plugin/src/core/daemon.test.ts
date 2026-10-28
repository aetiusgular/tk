// Which token a hook presents to the daemon, and which paths it accepts from
// its environment. Integration w1 decision (plans/parallel/tk_integration_w1.md,
// Decisions): a hook is never the daemon. It presents the chat token `tk run`
// gave it, from TK_CHAT_TOKEN or the file TK_CHAT_TOKEN_FILE names, and never
// reads daemon.token. Paths from the environment (TK_POLICY, TK_DAEMON_SOCKET,
// TK_CHAT_TOKEN_FILE) are refused inside the session's folder, where a
// cloned repository's settings file could point them.

import { chmodSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { chatToken, exchange, insideFolder } from "./daemon.ts";
import { loadPolicyFrom } from "./policy.ts";
import { startFakeDaemon } from "../testing/fakeDaemon.ts";

const root = mkdtempSync(path.join(tmpdir(), "tk-tok-"));
chmodSync(root, 0o700);
const safe = path.join(root, "safe");
const project = path.join(root, "project");
mkdirSync(safe, { mode: 0o700 });
mkdirSync(project, { mode: 0o700 });
const CHAT = "a".repeat(64);
const tokenFile = path.join(safe, "token");
writeFileSync(tokenFile, `${CHAT}\n`, { mode: 0o600 });

afterAll(() => rmSync(root, { recursive: true, force: true }));

describe("chatToken", () => {
  it("never falls back to daemon.token beside the socket", async () => {
    const d = await startFakeDaemon({ approve: "yes" });
    try {
      expect(chatToken({ TK_DAEMON_SOCKET: d.socket }, project)).toBeUndefined();
      const r = await exchange([{ op: "event", event: { id: "x" } }], { env: { TK_DAEMON_SOCKET: d.socket }, client: "t", replyTimeoutMs: 500, cwd: project });
      expect("error" in r && r.error).toMatch(/no chat token/);
      expect(d.lines).toHaveLength(0);
    } finally {
      await d.close();
    }
  });

  it("takes TK_CHAT_TOKEN first", () => {
    expect(chatToken({ TK_CHAT_TOKEN: "b".repeat(64), TK_CHAT_TOKEN_FILE: tokenFile }, project)).toBe("b".repeat(64));
  });

  it("reads TK_CHAT_TOKEN_FILE when it is absolute, yours, private and outside the session folder", () => {
    expect(chatToken({ TK_CHAT_TOKEN_FILE: tokenFile }, project)).toBe(CHAT);
  });

  it("refuses a token file that is relative, readable by others, a link, malformed or inside the session folder", () => {
    const loose = path.join(safe, "loose");
    writeFileSync(loose, CHAT, { mode: 0o600 });
    chmodSync(loose, 0o644);
    const link = path.join(safe, "link");
    symlinkSync(tokenFile, link);
    const bad = path.join(safe, "bad");
    writeFileSync(bad, "not-a-token", { mode: 0o600 });
    const planted = path.join(project, "token");
    writeFileSync(planted, CHAT, { mode: 0o600 });
    for (const f of ["token", loose, link, bad, planted]) expect(chatToken({ TK_CHAT_TOKEN_FILE: f }, project), f).toBeUndefined();
  });
});

describe("a daemon-scope token is never presented (security finding S3)", () => {
  it("refuses TK_CHAT_TOKEN_FILE naming daemon.token beside the socket", async () => {
    const d = await startFakeDaemon({ approve: "yes" });
    try {
      const env = { TK_DAEMON_SOCKET: d.socket, TK_CHAT_TOKEN_FILE: path.join(d.dir, "daemon.token") };
      expect(chatToken(env, project)).toBeUndefined();
      const r = await exchange([{ op: "event", event: { id: "x" } }], { env, client: "t", replyTimeoutMs: 500, cwd: project });
      expect("error" in r && r.error).toMatch(/no chat token/);
      expect(d.lines).toHaveLength(0);
    } finally {
      await d.close();
    }
  });

  it("closes when the daemon says the token it got has daemon scope, whatever file it came from", async () => {
    const d = await startFakeDaemon({ approve: "yes" });
    try {
      const copy = path.join(safe, "copied-token");
      writeFileSync(copy, `${d.token}\n`, { mode: 0o600 });
      const env = { TK_DAEMON_SOCKET: d.socket, TK_CHAT_TOKEN_FILE: copy };
      const r = await exchange([{ op: "approve", request: { id: "ap-1" } }], { env, client: "t", replyTimeoutMs: 500, cwd: project });
      expect("error" in r && r.error).toMatch(/daemon scope/);
      expect(d.lines.filter((l) => l.line.op !== "hello")).toHaveLength(0);
      const r2 = await exchange([{ op: "approve", request: { id: "ap-2" } }], { env: { TK_DAEMON_SOCKET: d.socket, TK_CHAT_TOKEN: d.token }, client: "t", replyTimeoutMs: 500, cwd: project });
      expect("error" in r2 && r2.error).toMatch(/daemon scope/);
    } finally {
      await d.close();
    }
  });
});

describe("paths from the environment", () => {
  it("insideFolder follows links on the folder and handles missing files", () => {
    expect(insideFolder(path.join(project, "a", "b.json"), project)).toBe(true);
    expect(insideFolder(path.join(safe, "p.json"), project)).toBe(false);
    expect(insideFolder(project, project)).toBe(true);
    const linkToProject = path.join(root, "via-link");
    symlinkSync(project, linkToProject);
    expect(insideFolder(path.join(linkToProject, "p.json"), project)).toBe(true);
    expect(insideFolder(path.join(project, "..", "safe", "p.json"), project)).toBe(false);
  });

  it("refuses TK_DAEMON_SOCKET inside the session folder", async () => {
    const r = await exchange([{ op: "event", event: { id: "x" } }], { env: { TK_DAEMON_SOCKET: path.join(project, "daemon.sock"), TK_CHAT_TOKEN: CHAT }, client: "t", replyTimeoutMs: 500, cwd: project });
    expect("error" in r && r.error).toMatch(/inside the session folder/);
  });

  it("refuses a daemon socket inside the session folder or relative, whichever variable named it (S14)", async () => {
    if (process.platform === "darwin") return; // the socket there is under HOME, not XDG_RUNTIME_DIR
    for (const runtime of [path.join(project, "run"), "relative/run"]) {
      const r = await exchange([{ op: "event", event: { id: "x" } }], { env: { XDG_RUNTIME_DIR: runtime, TK_CHAT_TOKEN: CHAT }, client: "t", replyTimeoutMs: 500, cwd: project });
      expect("error" in r && r.error, runtime).toMatch(/the daemon socket is (inside the session folder|not an absolute path)/);
    }
  });

  it("refuses TK_POLICY inside the session folder, and accepts the same file elsewhere", () => {
    const body = JSON.stringify({ version: 1, rules: [{ tool: "bash", action: "allow" }] });
    const inProject = path.join(project, ".tk-policy.json");
    writeFileSync(inProject, body, { mode: 0o600 });
    const outside = path.join(safe, "policy.json");
    writeFileSync(outside, body, { mode: 0o600 });
    const refused = loadPolicyFrom({ TK_POLICY: inProject }, project);
    expect(refused.problem).toMatch(/inside the session folder/);
    expect(refused.policy.rules).toHaveLength(0);
    expect(loadPolicyFrom({ TK_POLICY: outside }, project).policy.rules).toHaveLength(1);
  });

  it("does not apply the folder check to the default ~/.tk/policy.json (a session started in HOME)", () => {
    const r = loadPolicyFrom({}, homedir());
    expect(r.problem ?? "").not.toMatch(/inside the session folder/);
  });
});
