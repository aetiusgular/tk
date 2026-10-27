// Mac test 10.1, second point (retest 2, 13 Oct): Claude Code runs a
// read-only git command such as `git show` itself, with no permission
// request, so a MonoCode card or rule never decides. On a partial clone that
// read lazily fetches a missing blob, and the fetch runs the program the
// repository's own config names (`remote.origin.uploadpack`).
//
// This check runs the real Claude Code against the fake Messages server in a
// partial clone with a planted `uploadpack` script, once per case:
//   - no ask rule, default mode: which git forms Claude Code runs unasked
//     (the finding, reproduced; a control for every other case)
//   - `permissions.ask: ["Bash(git:*)"]` through `--settings`, in each
//     permission mode MonoCode uses for a card: every git form must reach the
//     permission prompt tool as a `can_use_tool` request before it runs.
// A third part starts Claude Code in a repository with a clean filter and a
// stat-dirty file: its own start-up `git status` runs the filter unless
// `includeGitInstructions` is false, as MonoCode sets it with the rule.
// Every request is denied, so a marker file means a command ran unasked. The
// first command that runs fetches the missing blobs, so later commands in the
// same case find them and run no fetch: "asked" is what each row proves.

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync, chmodSync, utimesSync } from "node:fs";
import path from "node:path";
import { startFakeMessages, textBlock, toolUse } from "../lib/fakeMessages.mjs";
import { ClaudeSession } from "../lib/claudeDriver.mjs";
import { makeTempDirs } from "../lib/codexDriver.mjs";

/** The rule MonoCode passes when a repository's config names commands. */
export const GIT_ASK_RULE = "Bash(git:*)";

/** What MonoCode puts in `--settings` then (claude.ts `launchOptions`). */
export const MONOCODE_GIT_ASK_SETTINGS = {
  permissions: { ask: [GIT_ASK_RULE] },
  includeGitInstructions: false,
};

const COMMANDS = [
  "git show --stat HEAD",
  "git log -p -1",
  "git -C . show --stat HEAD",
  "GIT_PAGER=cat git show --stat HEAD",
  "cd . && git show --stat HEAD",
  "git --no-pager diff HEAD~1 HEAD",
  "git status",
];

function git(cwd, args, env = {}) {
  return execFileSync("git", args, {
    cwd,
    env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", HOME: cwd, ...env },
    stdio: ["ignore", "pipe", "pipe"],
  }).toString();
}

/** A partial clone in `work` whose `uploadpack` touches `marker`. */
function plantPartialClone(root, work, marker) {
  const src = path.join(root, "src");
  mkdirSync(src, { recursive: true });
  git(src, ["init", "-q", "-b", "main"]);
  git(src, ["config", "uploadpack.allowFilter", "true"]);
  for (const [n, text] of [
    [1, "one\n"],
    [2, "two\n"],
  ]) {
    writeFileSync(path.join(src, "a.txt"), text);
    git(src, ["add", "a.txt"]);
    git(src, ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", `c${n}`]);
  }
  rmSync(work, { recursive: true, force: true });
  git(root, ["-c", "protocol.file.allow=always", "clone", "-q", "--filter=blob:none", "--no-checkout", `file://${src}`, work]);
  const script = path.join(root, "uploadpack.sh");
  writeFileSync(script, `#!/bin/sh\ntouch '${marker}'\nexec git-upload-pack "$@"\n`);
  chmodSync(script, 0o755);
  git(work, ["config", "protocol.file.allow", "always"]);
  git(work, ["config", "remote.origin.uploadpack", script]);
}

/** Model: one Bash call per turn, the command named in the prompt. */
function scriptFor() {
  return (body) => {
    // Side calls (titles, summaries) carry no tools.
    if ((body?.tools ?? []).length === 0) return [textBlock("ok")];
    const msgs = body?.messages ?? [];
    const last = msgs.at(-1);
    const blocks = Array.isArray(last?.content) ? last.content : [];
    if (blocks.some((b) => b?.type === "tool_result")) return [textBlock("done")];
    const text = typeof last?.content === "string" ? last.content : blocks.map((b) => b?.text ?? "").join("\n");
    const m = /^run: (.+)$/m.exec(text);
    if (!m) return [textBlock("ok")];
    return [toolUse("Bash", { command: m[1], description: "Run a git command" })];
  };
}

async function runCase({ claudePath, mode, settings, label }) {
  // Claude Code refuses bypassPermissions as root unless told it runs in a
  // sandbox (cloud workspaces run as root).
  const env = mode === "bypassPermissions" && process.getuid?.() === 0 ? { IS_SANDBOX: "1" } : {};
  const fake = await startFakeMessages({ script: scriptFor() });
  const dirs = makeTempDirs("cl-gitask-");
  const marker = path.join(dirs.root, "uploadpack-ran");
  plantPartialClone(dirs.root, dirs.work, marker);
  const asked = [];
  const s = new ClaudeSession({
    claudePath,
    dirs,
    baseUrl: fake.baseUrl,
    settings,
    args: ["--permission-mode", mode],
    env,
    onControl: (req) => {
      const r = req.request ?? {};
      if (r.subtype === "can_use_tool") asked.push(String(r.input?.command ?? r.tool_name ?? "?"));
      return { behavior: "deny", message: "Denied by the contract check." };
    },
  });
  const rows = [];
  try {
    for (const command of COMMANDS) {
      rmSync(marker, { force: true });
      const before = asked.length;
      await s.send(`run: ${command}`, 90000);
      rows.push({ command, asked: asked.length > before, ran: existsSync(marker) });
    }
  } finally {
    await s.close().catch(() => {});
    await fake.close();
  }
  return { label, mode, rows };
}

function control() {
  // Proves the plant is live: plain git on a second partial clone runs it.
  const dirs = makeTempDirs("cl-gitask-ctl-");
  const marker = path.join(dirs.root, "uploadpack-ran");
  plantPartialClone(dirs.root, dirs.work, marker);
  try {
    git(dirs.work, ["show", "--stat", "HEAD"]);
  } catch {}
  return existsSync(marker);
}

/**
 * Claude Code's own start-up git calls (`git status --short`, `git log`, for
 * its system prompt) run outside any tool call. A stat-dirty file of the same
 * size makes `git status` run the repository's clean filter. Returns whether
 * the filter ran during one plain turn.
 */
async function startupFilterRan({ claudePath, settings }) {
  const fake = await startFakeMessages({ script: () => [textBlock("ok")] });
  const dirs = makeTempDirs("cl-gitask-start-");
  const marker = path.join(dirs.root, "filter-ran");
  git(dirs.work, ["init", "-q", "-b", "main"]);
  writeFileSync(path.join(dirs.work, "a.txt"), "one\n");
  git(dirs.work, ["add", "a.txt"]);
  git(dirs.work, ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c1"]);
  writeFileSync(path.join(dirs.work, "a.txt"), "ONE\n");
  utimesSync(path.join(dirs.work, "a.txt"), new Date("2001-01-01"), new Date("2001-01-01"));
  const script = path.join(dirs.root, "clean.sh");
  writeFileSync(script, `#!/bin/sh\ntouch '${marker}'\ncat\n`);
  chmodSync(script, 0o755);
  git(dirs.work, ["config", "filter.x.clean", script]);
  writeFileSync(path.join(dirs.work, ".git", "info", "attributes"), "* filter=x\n");
  const s = new ClaudeSession({
    claudePath,
    dirs,
    baseUrl: fake.baseUrl,
    settings,
    args: ["--permission-mode", "default"],
    onControl: () => ({ behavior: "deny", message: "Denied by the contract check." }),
  });
  try {
    await s.send("hello", 90000);
  } finally {
    await s.close().catch(() => {});
    await fake.close();
  }
  return existsSync(marker);
}

export async function checkGitAsk({ claudePath }) {
  if (!control()) {
    return { status: "error", finding: "The planted uploadpack did not run under plain git, so this check proves nothing.", seen: {} };
  }
  const baseline = await runCase({ claudePath, mode: "default", settings: {}, label: "no rule" });
  const ruled = [];
  for (const mode of ["default", "acceptEdits", "plan", "auto", "bypassPermissions"]) {
    ruled.push(await runCase({ claudePath, mode, settings: MONOCODE_GIT_ASK_SETTINGS, label: `ask ${GIT_ASK_RULE}` }));
  }
  const startupControl = await startupFilterRan({ claudePath, settings: { permissions: { ask: [GIT_ASK_RULE] } } });
  const startupMonoCode = await startupFilterRan({ claudePath, settings: MONOCODE_GIT_ASK_SETTINGS });
  const unaskedBaseline = baseline.rows.filter((r) => !r.asked).map((r) => r.command);
  const leaks = ruled.flatMap((c) => c.rows.filter((r) => !r.asked || r.ran).map((r) => `${c.mode}: ${r.command}${r.ran ? " (ran the plant)" : ""}`));
  const seen = {
    baseline: baseline.rows,
    ruled: ruled.map((c) => ({ mode: c.mode, rows: c.rows })),
    startup: { ruleOnlyRanFilter: startupControl, monocodeSettingsRanFilter: startupMonoCode },
  };
  if (startupMonoCode) {
    return { status: "fail", finding: "With MonoCode's settings Claude Code's own start-up git still ran the repository's clean filter.", seen };
  }
  if (unaskedBaseline.length === 0) {
    return { status: "info", finding: "Claude Code asked about every git form with no rule; the rule is not needed on this version.", seen };
  }
  if (leaks.length > 0) {
    return { status: "fail", finding: `With ${GIT_ASK_RULE} some git forms ran or were not asked: ${leaks.join("; ")}`, seen };
  }
  return {
    status: "pass",
    finding: `Without a rule Claude Code ran ${unaskedBaseline.length} of ${COMMANDS.length} git forms unasked (${unaskedBaseline.join("; ")}). With ${GIT_ASK_RULE} every form reached can_use_tool first, in default, acceptEdits, plan, auto and bypassPermissions, and none ran the plant when denied. At start, Claude Code's own git ran a clean filter with the rule alone: ${startupControl ? "yes" : "no"}; with includeGitInstructions false as well: no.`,
    seen,
  };
}
