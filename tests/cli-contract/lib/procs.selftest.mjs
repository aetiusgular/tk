// node tests/cli-contract/lib/procs.selftest.mjs
// Mac test w2, MW6, reproduced on Linux: a shim and a child under it that
// both ignore SIGTERM, as a slow `codex app-server` under its npm shim. The
// old stop (SIGTERM to the shim, SIGKILL to the shim after 3 s) leaves the
// child running; `stopGroup` leaves nothing.
import { spawn, spawnSync } from "node:child_process";
import { spawnGroup, stopGroup } from "./procs.mjs";

const SCRIPT = `trap '' TERM; (trap '' TERM; exec sleep 60) & echo $!; wait`;
// Running, not a zombie: a container's pid 1 may never reap an orphan, and
// signal 0 still answers for a zombie.
const alive = (pid) => {
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  const r = spawnSync("ps", ["-o", "stat=", "-p", String(pid)], { encoding: "utf8" });
  const stat = r.stdout.trim();
  return stat !== "" && !stat.startsWith("Z");
};
const firstLine = (child) => new Promise((resolve) => child.stdout.once("data", (d) => resolve(Number(String(d).trim()))));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
const check = (name, ok, detail) => {
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failed++;
};

{
  // The old way, as codexDriver.close() and computerUse.mjs did it.
  const shim = spawn("sh", ["-c", SCRIPT], { stdio: ["pipe", "pipe", "ignore"] });
  const grandchild = await firstLine(shim);
  shim.stdin.end();
  shim.kill("SIGTERM");
  const exited = new Promise((r) => shim.once("exit", r));
  const t = setTimeout(() => shim.kill("SIGKILL"), 1000);
  await exited;
  clearTimeout(t);
  await sleep(100);
  check("the old stop leaves the child running (the cause)", alive(grandchild), `pid ${grandchild}`);
  try {
    process.kill(grandchild, "SIGKILL");
  } catch {}
}

{
  const shim = spawnGroup("sh", ["-c", SCRIPT], { stdio: ["pipe", "pipe", "ignore"] });
  const grandchild = await firstLine(shim);
  const started = Date.now();
  await stopGroup(shim, 1000);
  await sleep(100);
  check("stopGroup leaves nothing of the group", !alive(grandchild) && !alive(shim.pid), `child ${grandchild} ${alive(grandchild) ? "alive" : "gone"}, ${Date.now() - started} ms`);
}

{
  // The leader already gone (SIGKILLed alone), its child still running.
  const shim = spawnGroup("sh", ["-c", SCRIPT], { stdio: ["pipe", "pipe", "ignore"] });
  const grandchild = await firstLine(shim);
  shim.kill("SIGKILL");
  await new Promise((r) => shim.once("exit", r));
  check("a child outlives its killed leader", alive(grandchild));
  await stopGroup(shim, 200);
  await sleep(100);
  check("stopGroup still ends a group whose leader is gone", !alive(grandchild));
}

process.exit(failed ? 1 : 0);
