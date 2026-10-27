// A fake tk daemon for tests: speaks contracts/daemon-protocol.md section 2
// on a Unix socket in a fresh 0700 folder, with a daemon.token file beside
// it, and records every line. Used by the vitest suites and by
// tests/cli-contract (imported from .mjs; Node strips the types). Never shipped
// as part of a hook: hooks import only src/core and their own adapter.

import { mkdtempSync, rmSync, writeFileSync, chmodSync } from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";

export type ApproveBehaviour = "yes" | "no" | "none" | "timeout" | "hang" | "close" | "refuse";

export type FakeDaemon = {
  dir: string;
  socket: string;
  token: string;
  chatToken: string;
  /** Every parsed client line, in order, with the connection number. */
  lines: { conn: number; line: Record<string, unknown> }[];
  approvals: () => Record<string, unknown>[];
  events: () => Record<string, unknown>[];
  setApprove: (b: ApproveBehaviour) => void;
  /** Env for a hook that should reach this daemon. */
  env: (withChatToken?: boolean) => Record<string, string>;
  close: () => Promise<void>;
};

const DAEMON_TOKEN = "1".repeat(64);
const CHAT_TOKEN = "2".repeat(64);

export async function startFakeDaemon(opts: { approve?: ApproveBehaviour; root?: string } = {}): Promise<FakeDaemon> {
  // Unix socket paths are limited to about 104 bytes; keep the folder short.
  const dir = mkdtempSync(path.join(opts.root ?? tmpdir(), "tkd-"));
  chmodSync(dir, 0o700);
  const socket = path.join(dir, "daemon.sock");
  writeFileSync(path.join(dir, "daemon.token"), `${DAEMON_TOKEN}\n`, { mode: 0o600 });
  let behaviour: ApproveBehaviour = opts.approve ?? "none";
  const lines: { conn: number; line: Record<string, unknown> }[] = [];
  const sockets = new Set<net.Socket>();
  let conns = 0;

  const server = net.createServer((s) => {
    sockets.add(s);
    s.on("close", () => sockets.delete(s));
    s.on("error", () => {});
    const conn = ++conns;
    let greeted = false;
    let buf = "";
    const send = (o: unknown) => {
      if (!s.destroyed) s.write(`${JSON.stringify(o)}\n`);
    };
    s.on("data", (chunk) => {
      buf += chunk.toString("utf8");
      let nl: number;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const raw = buf.slice(0, nl);
        buf = buf.slice(nl + 1);
        let line: Record<string, unknown>;
        try {
          line = JSON.parse(raw) as Record<string, unknown>;
        } catch {
          send({ ok: false, error: "not JSON" });
          continue;
        }
        lines.push({ conn, line });
        if (!greeted) {
          if (line.op !== "hello") {
            send({ ok: false, error: "hello first" });
            s.end();
            return;
          }
          if (line.token === DAEMON_TOKEN) send({ ok: true, scope: "daemon", daemon: { version: "0.0.0-fake", pid: process.pid, protocol: 1 } });
          else if (line.token === CHAT_TOKEN) send({ ok: true, scope: "chat", chat: "chat-fake" });
          else {
            send({ ok: false, error: "bad token" });
            s.end();
            return;
          }
          greeted = true;
          continue;
        }
        if (line.op === "event") {
          const ev = line.event as Record<string, unknown> | undefined;
          send({ ok: true, id: ev?.id });
        } else if (line.op === "approve") {
          const req = line.request as Record<string, unknown> | undefined;
          const id = req?.id;
          if (behaviour === "yes") send({ ok: true, id, answer: "yes", by: "approver" });
          else if (behaviour === "no") send({ ok: true, id, answer: "no", by: "approver" });
          else if (behaviour === "none") send({ ok: true, id, answer: "no", by: "none" });
          else if (behaviour === "timeout") send({ ok: true, id, answer: "no", by: "timeout" });
          else if (behaviour === "close") s.destroy();
          else if (behaviour === "refuse") send({ ok: false, error: "refused" });
          // "hang": never answer.
        } else {
          send({ ok: false, error: `fake daemon: op ${String(line.op)} not supported` });
        }
      }
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(socket, () => resolve());
  });
  chmodSync(socket, 0o600);
  return {
    dir,
    socket,
    token: DAEMON_TOKEN,
    chatToken: CHAT_TOKEN,
    lines,
    approvals: () => lines.filter((l) => l.line.op === "approve").map((l) => l.line.request as Record<string, unknown>),
    events: () => lines.filter((l) => l.line.op === "event").map((l) => l.line.event as Record<string, unknown>),
    setApprove: (b) => {
      behaviour = b;
    },
    env: (withChatToken = false) => ({ TK_DAEMON_SOCKET: socket, ...(withChatToken ? { TK_CHAT_TOKEN: CHAT_TOKEN } : {}) }),
    close: async () => {
      for (const s of sockets) s.destroy();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      rmSync(dir, { recursive: true, force: true });
    },
  };
}
