// A client for the daemon's socket (contracts/daemon-protocol.md). Used for
// `approve` (a policy `ask` on a harness with no ask of its own, or before
// Claude Code's own prompt) and for `event` (the adapter record). Every call
// is bounded in time; a daemon that is down never blocks a turn longer than
// the connect timeout. Canonical copy: packages/tk-claude-plugin/src/core/.

import { constants, lstatSync, readFileSync } from "node:fs";
import net from "node:net";
import { homedir, platform } from "node:os";
import path from "node:path";
import { insideFolder } from "./policy.ts";

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

export { insideFolder };

/**
 * The chat token this hook presents: `TK_CHAT_TOKEN`, else the file
 * `TK_CHAT_TOKEN_FILE` names (`tk run` writes it, for harnesses that strip
 * secret-named variables from a hook's environment, rules case 76). Never the
 * daemon's own `daemon.token`: a hook is not the daemon, and the daemon scope
 * can mint tokens, read every chat and become an approver (integration w1
 * decision). The file must be absolute, a regular file (not a link), yours,
 * readable by you alone, small, outside the session folder, and not named
 * `daemon.token` (security finding S3; `exchange` also closes when the daemon
 * says the token it got has daemon scope).
 */
export function chatToken(env: NodeJS.ProcessEnv = process.env, cwd: string = process.cwd()): string | undefined {
  const fromEnv = env.TK_CHAT_TOKEN?.trim();
  if (fromEnv && HEX64.test(fromEnv)) return fromEnv;
  const file = env.TK_CHAT_TOKEN_FILE?.trim();
  if (!file || !path.isAbsolute(file) || insideFolder(file, cwd)) return undefined;
  if (path.basename(file) === "daemon.token") return undefined;
  try {
    const st = lstatSync(file);
    if (!st.isFile() || st.size > 256) return undefined;
    if (typeof process.getuid === "function" && st.uid !== process.getuid()) return undefined;
    if ((st.mode & 0o077) !== 0) return undefined;
    const t = readFileSync(file, { encoding: "utf8", flag: constants.O_RDONLY | constants.O_NOFOLLOW }).trim();
    return HEX64.test(t) ? t : undefined;
  } catch {
    return undefined;
  }
}

type Reply = Record<string, unknown> & { ok?: boolean; error?: string };

/** One connection: hello, then each request in order, one reply line each. */
export async function exchange(
  requests: readonly Record<string, unknown>[],
  opts: { env?: NodeJS.ProcessEnv; client: string; replyTimeoutMs: number; cwd?: string },
): Promise<{ replies: Reply[] } | { error: string }> {
  const env = opts.env ?? process.env;
  const cwd = opts.cwd ?? process.cwd();
  const sock = socketPath(env);
  if (env.TK_DAEMON_SOCKET?.trim() && insideFolder(sock, cwd)) return { error: "TK_DAEMON_SOCKET is inside the session folder, which a project can set; not used" };
  // Whatever named it (TK_DAEMON_SOCKET, XDG_RUNTIME_DIR, HOME): a project's
  // settings can set the hook's environment, and a socket the agent made in
  // the project would answer its own approvals (security finding S14,
  // integration w2).
  if (!path.isAbsolute(sock)) return { error: "the daemon socket is not an absolute path; not used" };
  if (insideFolder(sock, cwd)) return { error: "the daemon socket is inside the session folder, which a project can set; not used" };
  const token = chatToken(env, cwd);
  if (!token) return { error: "no chat token (start the harness with tk run)" };
  const notOwned = socketLooksOwned(sock);
  if (notOwned) return { error: notOwned };

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
        // A hook is never the daemon: a token that got daemon scope (a copy of
        // daemon.token under another name) is not used (security finding S3).
        if (replies.length === 0 && reply.scope !== "chat") return finish({ error: "the token has daemon scope; a hook presents only a chat token" });
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
