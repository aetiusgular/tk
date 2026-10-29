// Starting and stopping a CLI with everything it starts (Mac test w2, MW6).
//
// The npm `codex` command is a Node shim that starts the native binary and
// forwards SIGINT, SIGTERM and SIGHUP to it (@openai/codex 0.163.0
// bin/codex.js:241-292). A SIGKILL to the shim, or a runner that exits while
// the shim waits, leaves the native `codex app-server` running, reparented to
// init or launchd, still writing into the run's temp home; on the Mac that
// crashed the runner's cleanup with ENOTEMPTY. So each CLI starts in its own
// process group (`detached`, which calls setsid on POSIX: Node's
// child_process docs), and stopping it signals the whole group: SIGTERM, a
// grace period, then SIGKILL to whatever is left, the leader gone or not (a
// group outlives its leader while members remain). The runner kills every
// group it still knows of when it exits.

import { spawn } from "node:child_process";

const live = new Set();
let hooked = false;

function signalGroup(child, signal) {
  if (child.pid === undefined) return;
  try {
    process.kill(-child.pid, signal);
  } catch {}
  try {
    child.kill(signal);
  } catch {}
}

/** `spawn`, in a process group of its own, remembered until it exits. */
export function spawnGroup(command, args, options = {}) {
  const child = spawn(command, args, { ...options, detached: process.platform !== "win32" });
  live.add(child);
  child.once("exit", () => live.delete(child));
  if (!hooked) {
    hooked = true;
    process.once("exit", () => {
      for (const c of live) signalGroup(c, "SIGKILL");
    });
  }
  return child;
}

/** Ends stdin, sends SIGTERM to the group, waits up to `graceMs` for the
 *  leader, then SIGKILLs the group so nothing under it outlives the check. */
export async function stopGroup(child, graceMs = 3000) {
  try {
    child.stdin?.end();
  } catch {}
  const gone = child.exitCode !== null || child.signalCode !== null;
  const exited = gone ? Promise.resolve() : new Promise((resolve) => child.once("exit", resolve));
  if (!gone) signalGroup(child, "SIGTERM");
  let timer;
  await Promise.race([exited, new Promise((resolve) => (timer = setTimeout(resolve, graceMs)))]);
  clearTimeout(timer);
  signalGroup(child, "SIGKILL");
  await exited;
}
