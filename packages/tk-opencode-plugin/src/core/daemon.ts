// A client for the daemon's socket (contracts/daemon-protocol.md). Used for
// `approve` (a policy `ask` on a harness with no ask of its own, or before
// Claude Code's own prompt) and for `event` (the adapter record). Every call
// is bounded in time; a daemon that is down never blocks a turn longer than
// the connect timeout. Canonical copy: packages/tk-claude-plugin/src/core/.

import { lstatSync, readFileSync } from "node:fs";
import net from "node:net";
import { homedir, platform } from "node:os";
import path from "node:path";

export const CONNECT_TIMEOUT_MS = 800;
const MAX_LINE_BYTES = 1024 * 1024;

/** contracts/daemon-protocol.md section 1, with `TK_DAEMON_SOCKET` as an override for tests and `tk run`. */
export function socketPath(env: NodeJS.ProcessEnv = process.env): string {
  if (env.TK_DAEMON_SOCKET && env.TK_DAEMON_SOCKET.trim() !== "") return env.TK_DAEMON_SOCKET;
  if (platform() === "darwin") return path.join(homedir(), "Library", "Application Support", "tk", "daemon.sock");
  const runtime = env.XDG_RUNTIME_DIR;
  if (runtime && runtime.trim() !== "") return path.join(runtime, "tk", "daemon.sock");
  return path.join(homedir(), ".tk", "run", "tk", "daemon.sock");
}

/**
 * The socket is trusted only when it is a socket owned by this user in a
 * folder owned by this user that no one else can write. Another user could
 * otherwise plant a socket that answers `yes` to every approval.
 */
export function socketLooksOwned(sock: string): string | undefined {
  if (typeof process.getuid !== "function") return undefined;
  const uid = process.getuid();
  try {
    const s = lstatSync(sock);
    if (!s.isSocket()) return "not a socket";
    if (s.uid !== uid) return "socket owned by another user";
    const d = lstatSync(path.dirname(sock));
    if (!d.isDirectory()) return "socket folder is not a folder";
    if (d.uid !== uid) return "socket folder owned by another user";
    if ((d.mode & 0o022) !== 0) return "socket folder writable by others";
    return undefined;
  } catch {
    return "no daemon socket";
  }
}

const HEX64 = /^[0-9a-f]{64}$/;

/** `TK_CHAT_TOKEN` (chat scope), else the daemon's token file beside the socket. */
export function daemonToken(sock: string, env: NodeJS.ProcessEnv = process.env): string | undefined {
  const fromEnv = env.TK_CHAT_TOKEN?.trim();
  if (fromEnv && HEX64.test(fromEnv)) return fromEnv;
  try {
    const t = readFileSync(path.join(path.dirname(sock), "daemon.token"), "utf8").trim();
    return HEX64.test(t) ? t : undefined;
  } catch {
    return undefined;
  }
}

type Reply = Record<string, unknown> & { ok?: boolean; error?: string };

/** One connection: hello, then each request in order, one reply line each. */
export async function exchange(
  requests: readonly Record<string, unknown>[],
  opts: { env?: NodeJS.ProcessEnv; client: string; replyTimeoutMs: number },
): Promise<{ replies: Reply[] } | { error: string }> {
  const env = opts.env ?? process.env;
  const sock = socketPath(env);
  const notOwned = socketLooksOwned(sock);
  if (notOwned) return { error: notOwned };
  const token = daemonToken(sock, env);
  if (!token) return { error: "no daemon token" };

  return await new Promise((resolve) => {
    const replies: Reply[] = [];
    const lines = [{ op: "hello", token, client: opts.client }, ...requests];
    let buf = "";
    let settled = false;
    let timer: NodeJS.Timeout | undefined;
    const socket = net.createConnection({ path: sock });
    const finish = (out: { replies: Reply[] } | { error: string }) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      socket.destroy();
      resolve(out);
    };
    const arm = (ms: number, label: string) => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => finish({ error: label }), ms);
    };
    arm(CONNECT_TIMEOUT_MS, "daemon connect timed out");
    socket.on("error", (e: NodeJS.ErrnoException) => finish({ error: `daemon unreachable (${e.code ?? "error"})` }));
    socket.on("close", () => finish({ error: "daemon closed the connection" }));
    socket.on("connect", () => {
      socket.write(`${JSON.stringify(lines[0])}\n`);
      arm(2000, "daemon hello timed out");
    });
    socket.on("data", (chunk) => {
      buf += chunk.toString("utf8");
      if (buf.length > MAX_LINE_BYTES) return finish({ error: "daemon line too long" });
      let nl: number;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (line === "") continue;
        let reply: Reply;
        try {
          reply = JSON.parse(line) as Reply;
        } catch {
          return finish({ error: "daemon sent a line that is not JSON" });
        }
        if (typeof reply !== "object" || reply === null) return finish({ error: "daemon sent a bad line" });
        if (replies.length === 0 && reply.ok !== true) return finish({ error: `daemon refused hello: ${String(reply.error ?? "no reason")}` });
        replies.push(reply);
        if (replies.length === lines.length) return finish({ replies: replies.slice(1) });
        socket.write(`${JSON.stringify(lines[replies.length])}\n`);
        arm(opts.replyTimeoutMs, "daemon reply timed out");
      }
    });
  });
}

export type ApproveRequest = {
  id: string;
  chat: string;
  harness: string;
  tool: string;
  summary: string;
  timeoutSeconds: number;
};

export type ApproveOutcome =
  | { answer: "yes" | "no"; by: "approver" | "timeout" | "none" }
  | { unreachable: string };

/** Sends `approve` and waits for the answer (contract section 2, rule 3). */
export async function approve(request: ApproveRequest, env?: NodeJS.ProcessEnv): Promise<ApproveOutcome> {
  const result = await exchange([{ op: "approve", request }], {
    client: `tk hook (${request.harness})`,
    replyTimeoutMs: (request.timeoutSeconds + 5) * 1000,
    ...(env ? { env } : {}),
  });
  if ("error" in result) return { unreachable: result.error };
  const reply = result.replies[0];
  if (!reply || reply.ok !== true) return { unreachable: `daemon refused approve: ${String(reply?.error ?? "no reply")}` };
  const answer = reply.answer === "yes" ? "yes" : "no";
  const by = reply.by === "approver" || reply.by === "timeout" || reply.by === "none" ? reply.by : "approver";
  // Only an explicit yes from an approver counts as yes.
  if (answer === "yes" && by !== "approver") return { answer: "no", by };
  return { answer, by };
}

export type AdapterEvent = {
  id: string;
  chatId: string;
  harness: string;
  kind: "tool.call" | "approval";
  summary: string;
  detail: Record<string, unknown>;
};

/** Posts one adapter record (contracts/events.md section 4). Best effort; never throws. */
export async function postEvent(event: AdapterEvent, env?: NodeJS.ProcessEnv): Promise<boolean> {
  try {
    const result = await exchange(
      [
        {
          op: "event",
          event: { ...event, at: 0, source: "adapter", summary: event.summary.slice(0, 200), detail: { trust: "client", ...event.detail } },
        },
      ],
      { client: `tk hook (${event.harness})`, replyTimeoutMs: 1500, ...(env ? { env } : {}) },
    );
    return "replies" in result && result.replies[0]?.ok === true;
  } catch {
    return false;
  }
}
