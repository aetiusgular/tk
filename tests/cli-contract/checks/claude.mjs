// Contract checks against the real Claude Code CLI in MonoCode's spawn shape
// (stream-json in and out, no -p) with a fake Messages server.
// Each check returns { status, finding, seen }, as in checks/codex.mjs.

import path from "node:path";
import { fileURLToPath } from "node:url";
import { startFakeMessages, textBlock, systemText } from "../lib/fakeMessages.mjs";
import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, realpathSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { ClaudeSession, writeProjectFile, DUMMY_KEY } from "../lib/claudeDriver.mjs";
import { makeTempDirs, isolatedEnv } from "../lib/codexDriver.mjs";
import { checkGitAsk } from "./claudeGitAsk.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const STUB_MCP = path.join(HERE, "..", "lib", "stubMcp.mjs");
const APPEND = ["--append-system-prompt", "The code word is PINEAPPLE."];
const SUMMARY = "<summary>The user said turn-1. The assistant answered ok.</summary>";

function script(body) {
  const all = JSON.stringify(body?.messages ?? []);
  // Compaction asks for a summary of the conversation.
  if (/summary of the conversation|create a detailed summary|Your task is to create/i.test(all.slice(-6000)) && (body?.tools ?? []).length === 0) {
    return [textBlock(SUMMARY)];
  }
  return [textBlock("ok")];
}

/** The `turn-N` prompts present in a request, in order. */
function turnTexts(body) {
  const out = [];
  for (const m of body?.messages ?? []) {
    if (m.role !== "user") continue;
    const blocks = typeof m.content === "string" ? [{ text: m.content }] : m.content ?? [];
    for (const b of blocks) {
      const t = (b?.text ?? "").trim();
      if (/^turn-\d+$/.test(t)) out.push(t);
    }
  }
  return out;
}

/** The last model request that carried this prompt. */
function requestFor(fake, prompt) {
  return [...fake.requests].reverse().find((r) => r.kind === "messages" && turnTexts(r.body).includes(prompt));
}

async function session(o) {
  const fake = o.fake ?? (await startFakeMessages({ script: o.script ?? script }));
  const dirs = o.dirs ?? makeTempDirs("cl-");
  const s = new ClaudeSession({ claudePath: o.claudePath, dirs, baseUrl: fake.baseUrl, settings: o.settings, args: o.args, env: o.env });
  return { fake, dirs, s };
}

async function shut(...parts) {
  for (const p of parts) {
    try {
      await p?.s?.close();
    } catch {}
  }
}

// ---------------------------------------------------------------------------

async function check2({ claudePath }) {
  const seen = {};
  const fake = await startFakeMessages({ script });
  const a = await session({ claudePath, fake, args: APPEND });
  let b, c, e, f;
  try {
    await a.s.send("turn-1");
    const sid = a.s.sessionId();
    seen.start = systemText(requestFor(fake, "turn-1").body).includes("PINEAPPLE");
    await a.s.close();

    // A restart: MonoCode passes the flag again with --resume.
    b = await session({ claudePath, fake, dirs: a.dirs, args: [...APPEND, "--resume", sid] });
    await b.s.send("turn-2");
    const r2 = requestFor(fake, "turn-2").body;
    seen.resume = { system: systemText(r2).includes("PINEAPPLE"), history: turnTexts(r2) };

    // Compaction, then a turn.
    const before = fake.requests.length;
    const compact = await b.s.send("/compact");
    seen.compactResult = compact.result?.subtype;
    seen.compactRequests = fake.requests.slice(before).filter((r) => r.kind === "messages").length;
    seen.compactBoundary = b.s.events.some((e) => e.type === "system" && /compact/i.test(e.subtype ?? ""));
    await b.s.send("turn-3");
    const r3 = requestFor(fake, "turn-3").body;
    seen.afterCompact = { system: systemText(r3).includes("PINEAPPLE"), history: turnTexts(r3), summaryIn: JSON.stringify(r3.messages).includes("turn-1. The assistant answered") };
    await b.s.close();

    // Resume without the flag: does the text persist on its own?
    c = await session({ claudePath, fake, dirs: a.dirs, args: ["--resume", sid] });
    await c.s.send("turn-4");
    seen.resumeWithoutFlag = systemText(requestFor(fake, "turn-4").body).includes("PINEAPPLE");
    await c.s.close();

    // Resume with different text, as after a changed preamble.
    e = await session({ claudePath, fake, dirs: a.dirs, args: ["--append-system-prompt", "The code word is MANGO.", "--resume", sid] });
    await e.s.send("turn-5");
    const s5 = systemText(requestFor(fake, "turn-5").body);
    seen.resumeNewText = { pineapple: s5.includes("PINEAPPLE"), mango: s5.includes("MANGO") };
    await e.s.close();

    f = await session({ claudePath, fake, dirs: a.dirs, args: ["--append-system-prompt", "The code word is KIWI.", "--resume", sid, "--fork-session"] });
    await f.s.send("turn-6");
    const s6 = systemText(requestFor(fake, "turn-6").body);
    seen.forkNewText = { pineapple: s6.includes("PINEAPPLE"), kiwi: s6.includes("KIWI") };
  } finally {
    await shut(a, b, c, e, f);
    await fake.close();
  }
  const ok = seen.start && seen.resume.system && seen.afterCompact.system;
  return {
    status: ok ? "pass" : "fail",
    finding:
      `--append-system-prompt in MonoCode's spawn shape (no -p): in the first request's system prompt ${seen.start ? "yes" : "NO"}; after --resume with the flag again ${seen.resume.system ? "yes" : "NO"} (history ${seen.resume.history.join(",")}); ` +
      `after /compact (${seen.compactRequests} model request(s), boundary event ${seen.compactBoundary}) ${seen.afterCompact.system ? "yes" : "NO"} (history ${seen.afterCompact.history.join(",") || "summary only"}). ` +
      `Resumed without the flag: ${seen.resumeWithoutFlag ? "still there (restored from the session)" : "gone"}. ` +
      `Resumed with different text: old ${seen.resumeNewText.pineapple ? "kept" : "dropped"}, new ${seen.resumeNewText.mango ? "used" : "IGNORED"}. ` +
      `Forked (--fork-session) with different text: old ${seen.forkNewText.pineapple ? "kept" : "dropped"}, new ${seen.forkNewText.kiwi ? "used" : "IGNORED"}.`,
    resumeNewTextIgnored: !seen.resumeNewText.mango,
    seen,
  };
}

/** Finds the uuid of the assistant event that answered a prompt. */
function assistantUuidAfter(events, prompt) {
  const i = events.findIndex((e) => e.type === "user" && JSON.stringify(e.message?.content ?? "").includes(prompt));
  const reply = events.slice(i + 1).find((e) => e.type === "assistant" && e.uuid);
  return reply?.uuid;
}

async function check6({ claudePath }) {
  const seen = {};
  const fake = await startFakeMessages({ script });
  const a = await session({ claudePath, fake });
  let b, c, d;
  try {
    for (const t of ["turn-1", "turn-2", "turn-3"]) await a.s.send(t);
    const sid = a.s.sessionId();
    // A user message echoed back carries no uuid unless asked; the
    // assistant reply does.
    const atUuid = assistantUuidAfter(a.s.events, "turn-1");
    seen.atUuid = atUuid ?? null;
    await a.s.close();

    b = await session({ claudePath, fake, dirs: a.dirs, args: ["--resume", sid, "--fork-session"] });
    await b.s.send("turn-4");
    seen.fork = { newId: b.s.sessionId() && b.s.sessionId() !== sid, history: turnTexts(requestFor(fake, "turn-4").body) };
    await b.s.close();

    if (atUuid) {
      c = await session({ claudePath, fake, dirs: a.dirs, args: ["--resume", sid, "--resume-session-at", atUuid, "--fork-session"] });
      await c.s.send("turn-5");
      seen.forkAt = { newId: c.s.sessionId() !== sid, history: turnTexts(requestFor(fake, "turn-5").body) };
      await c.s.close();
    }

    // The original is untouched by both forks.
    d = await session({ claudePath, fake, dirs: a.dirs, args: ["--resume", sid] });
    await d.s.send("turn-6");
    seen.original = turnTexts(requestFor(fake, "turn-6").body);
  } catch (error) {
    seen.error = error.message;
  } finally {
    await shut(a, b, c, d);
    await fake.close();
  }
  const okFork = seen.fork?.newId && seen.fork.history.join() === "turn-1,turn-2,turn-3,turn-4";
  const okAt = seen.forkAt?.newId && seen.forkAt.history.join() === "turn-1,turn-5";
  const okOrig = seen.original?.join() === "turn-1,turn-2,turn-3,turn-6";
  return {
    status: okFork && okAt && okOrig ? "pass" : "fail",
    finding: seen.error
      ? `Error: ${seen.error}`
      : `--resume --fork-session: new id ${!!seen.fork.newId}, history ${seen.fork.history.join(",")}. ` +
        `With --resume-session-at <turn-1 reply uuid>: ${seen.forkAt ? `new id ${seen.forkAt.newId}, history ${seen.forkAt.history.join(",")}` : "no uuid found in the stream"}. ` +
        `Original after both: ${seen.original?.join(",")}.`,
    seen,
  };
}

const AGENTS_ID = "cc-plugin-agents-md@builtin";
const INSTRUCTION_NAMES = ["CLAUDE.md", path.join(".claude", "CLAUDE.md"), "CLAUDE.local.md"];

/**
 * Instruction files in the folders above `dir`, up to but not including the
 * file system root, as Claude Code's ancestor walk looks for them. In the
 * default mode one of these makes the plugin treat the project as a Claude
 * project, and AGENTS.md is then left out. A CLAUDE.md in the system temp
 * folder or above the runs folder fails the check on that machine only
 * (Mac retest 8.6), so the check finds them and excludes them by setting.
 */
export function ancestorInstructionFiles(dir) {
  const out = [];
  let d = path.dirname(dir);
  while (d !== path.parse(d).root) {
    for (const n of INSTRUCTION_NAMES) {
      const f = path.join(d, n);
      try {
        if (statSync(f).isFile()) out.push(f);
      } catch {}
    }
    d = path.dirname(d);
  }
  return out;
}

/** claudeMdExcludes for the given files, each by its own path and its resolved path. */
function excludesFor(files) {
  const all = new Set();
  for (const f of files) {
    all.add(f);
    try {
      all.add(realpathSync(f));
    } catch {}
  }
  return [...all];
}

/** One AGENTS.md case: which of the code words reach the first model request. */
async function agentsCase({ claudePath, files, settings = {}, excludes = [], at, debugFile }) {
  const fake = await startFakeMessages({ script });
  const dirs = makeTempDirs("cl-");
  const work = at ? at(dirs) : dirs.work;
  mkdirSync(work, { recursive: true });
  for (const [f, body] of Object.entries(files)) writeProjectFile({ ...dirs, work }, f, body);
  const merged = excludes.length ? { ...settings, claudeMdExcludes: [...(settings.claudeMdExcludes ?? []), ...excludes] } : settings;
  const args = debugFile ? ["--debug-file", path.join(dirs.root, "debug.log")] : [];
  const x = await session({ claudePath, fake, dirs: { ...dirs, work }, settings: merged, args });
  const r = {};
  try {
    await x.s.send("turn-1");
    const all = JSON.stringify(requestFor(fake, "turn-1").body);
    Object.assign(r, { agents: all.includes("BANANA"), claude: all.includes("CHERRY"), above: all.includes("PLUM") });
    // Claude Code heads each instruction file it loads with "Contents of <path>".
    r.loaded = [...new Set([...all.matchAll(/Contents of ([^\s:]+)/g)].map((m) => m[1]))];
  } catch (error) {
    r.error = error.message;
  } finally {
    await shut(x);
    await fake.close();
  }
  if (debugFile) {
    try {
      r.debug = readFileSync(path.join(dirs.root, "debug.log"), "utf8")
        .split("\n")
        .filter((l) => /agents-md|fs\.ancestors|walk_failed|\[CLAUDE\.md\]/.test(l))
        .map((l) => l.slice(0, 240))
        .slice(0, 8);
    } catch {}
  }
  return r;
}

async function checkAgentsMd({ claudePath }) {
  const cases = [
    { name: "AGENTS.md only, defaults", files: { "AGENTS.md": "Agents word: BANANA." } },
    { name: "CLAUDE.md and AGENTS.md, defaults", files: { "AGENTS.md": "Agents word: BANANA.", "CLAUDE.md": "Claude word: CHERRY." } },
    {
      name: "CLAUDE.md and AGENTS.md, instructionFiles both",
      files: { "AGENTS.md": "Agents word: BANANA.", "CLAUDE.md": "Claude word: CHERRY." },
      settings: { pluginConfigs: { [AGENTS_ID]: { options: { instructionFiles: "claude-md-and-agents-md" } } } },
    },
    {
      name: "AGENTS.md only, plugin off",
      files: { "AGENTS.md": "Agents word: BANANA." },
      settings: { enabledPlugins: { [AGENTS_ID]: false } },
    },
  ];
  // The result must not depend on where the temp folder is: instruction
  // files above it are this machine's, not the case's.
  const above = ancestorInstructionFiles(makeTempDirs("cl-probe-").work);
  const excludes = excludesFor(above);
  const seen = {};
  for (const k of cases) seen[k.name] = await agentsCase({ claudePath, ...k, excludes, debugFile: true });
  const v = Object.values(seen);
  const ok = v[0].agents && !v[1].agents && v[1].claude && v[2].agents && v[2].claude && !v[3].agents;

  // Behaviour notes, reported as information only.
  const agentsOnly = { "AGENTS.md": "Agents word: BANANA." };
  const info = {
    // A project opened through a symlink (macOS /var is a link to /private/var).
    throughSymlink: await agentsCase({
      claudePath,
      files: agentsOnly,
      excludes,
      at: (d) => {
        const link = path.join(d.root, "work-link");
        symlinkSync(d.work, link);
        return link;
      },
    }),
    // A CLAUDE.md in a folder above the project, not excluded.
    claudeMdAbove: await agentsCase({
      claudePath,
      files: agentsOnly,
      excludes,
      at: (d) => {
        writeFileSync(path.join(d.root, "CLAUDE.md"), "Above word: PLUM.");
        return d.work;
      },
    }),
    // A project under HOME with ~/.claude/CLAUDE.md while CLAUDE_CONFIG_DIR
    // points elsewhere, as for a MonoCode Claude account profile.
    userClaudeMdWithConfigDir: await agentsCase({
      claudePath,
      files: agentsOnly,
      excludes,
      at: (d) => {
        mkdirSync(path.join(d.home, ".claude"), { recursive: true });
        writeFileSync(path.join(d.home, ".claude", "CLAUDE.md"), "Above word: PLUM.");
        return path.join(d.home, "proj");
      },
    }),
  };
  const word = (r) => (r.error ? `error ${r.error}` : r.agents ? "loaded" : "not loaded");
  return {
    status: ok ? "pass" : "fail",
    finding:
      Object.entries(seen).map(([n, r]) => `${n}: AGENTS.md ${word(r)}`).join("; ") +
      `. Plugin id ${AGENTS_ID}; option set through --settings {"pluginConfigs":{"${AGENTS_ID}":{"options":{"instructionFiles":"claude-md-and-agents-md"}}}}. ` +
      `Instruction files above the temp folder: ${above.length ? `${above.join(", ")} (excluded through claudeMdExcludes)` : "none"}. ` +
      `Info: AGENTS.md only, project reached through a symlink: ${word(info.throughSymlink)}; ` +
      `with a CLAUDE.md in a parent folder: AGENTS.md ${word(info.claudeMdAbove)}, parent CLAUDE.md ${info.claudeMdAbove.above ? "loaded" : "not loaded"}; ` +
      `project under HOME with ~/.claude/CLAUDE.md and its own CLAUDE_CONFIG_DIR: AGENTS.md ${word(info.userClaudeMdWithConfigDir)}, ~/.claude/CLAUDE.md ${info.userClaudeMdWithConfigDir.above ? "loaded" : "not loaded"}.` +
      (ok
        ? ""
        : ` Instruction files in each request: ${Object.entries(seen)
            .map(([n, r]) => `${n}: ${r.loaded?.join(", ") || "none"}`)
            .join("; ")}. ${
            !v[0].agents && !v[0].error && !(v[0].loaded ?? []).some((f) => /CLAUDE(\.local)?\.md$/i.test(f))
              ? "No CLAUDE.md reached the first request, so the plugin's own ancestor walk found one that the engine did not load, or the walk failed (the plugin then treats the project as a Claude project). "
              : ""
          }Debug log, first case: ${(v[0].debug ?? []).join(" | ") || "none"}`),
    ancestorInstructionFiles: above,
    seen,
    info,
  };
}

async function checkToolSearch({ claudePath }) {
  const mcp = JSON.stringify({ mcpServers: { stub: { type: "stdio", command: "node", args: [STUB_MCP] } } });
  const seen = {};
  for (const value of ["unset", "true", "false"]) {
    const fake = await startFakeMessages({ script });
    const x = await session({
      claudePath,
      fake,
      args: ["--mcp-config", mcp],
      settings: value === "unset" ? {} : { env: { ENABLE_TOOL_SEARCH: value } },
    });
    try {
      await x.s.send("turn-1");
      const body = requestFor(fake, "turn-1").body;
      const tools = body.tools ?? [];
      const mcpTools = tools.filter((t) => /stub/.test(t.name ?? ""));
      seen[value] = {
        tools: tools.length,
        mcpListed: mcpTools.map((t) => t.name),
        mcpDeferred: mcpTools.filter((t) => t.defer_loading).map((t) => t.name),
        searchTool: tools.filter((t) => /tool_search|ToolSearch/i.test(`${t.name ?? ""} ${t.type ?? ""}`)).map((t) => t.name ?? t.type),
        stubNamedAnywhere: JSON.stringify(body).includes("fetch_url"),
      };
    } catch (error) {
      seen[value] = { error: error.message };
    } finally {
      await shut(x);
      await fake.close();
    }
  }
  const d = (r) => (r.error ? `error ${r.error}` : `${r.tools} tools, stub tools sent ${r.mcpListed.join(",") || "none"}, deferred ${r.mcpDeferred.join(",") || "none"}, search tool ${r.searchTool.join(",") || "none"}`);
  return {
    status: "info",
    finding: `ENABLE_TOOL_SEARCH in --settings env with a stdio MCP server, against a non-Anthropic base URL: unset → ${d(seen.unset)}; "true" → ${d(seen.true)}; "false" → ${d(seen.false)}.`,
    seen,
  };
}

async function checkPromptSuggestions({ claudePath }) {
  const fake = await startFakeMessages({ script });
  const x = await session({ claudePath, fake, args: ["--prompt-suggestions", "true"] });
  const seen = {};
  try {
    const r = await x.s.send("turn-1", 30000);
    await new Promise((res) => setTimeout(res, 2000));
    seen.result = r.result?.subtype;
    seen.suggestions = x.s.events.filter((e) => e.type === "prompt_suggestion").map((e) => e.suggestion);
  } catch (error) {
    seen.error = error.message;
  } finally {
    seen.exit = await Promise.race([x.s.exited, new Promise((res) => setTimeout(() => res("running"), 100))]);
    seen.stderr = x.s.stderr.slice(-300);
    await shut(x);
    await fake.close();
  }
  seen.printMode = await printModeSuggestions(claudePath);
  const refused = /requires --print/.test(seen.stderr + (seen.error ?? ""));
  return {
    status: "info",
    finding: refused
      ? `--prompt-suggestions without --print is refused at startup: "${seen.stderr.trim().split("\n").at(-1)}". MonoCode's spawn cannot use it.`
      : `--prompt-suggestions without --print is accepted: turn ${seen.result ?? seen.error}, prompt_suggestion events ${seen.suggestions?.length ?? 0}. ` +
        `With -p in this setup: exit ${seen.printMode.exit}, ${seen.printMode.requests} model request(s), ${seen.printMode.suggestions} suggestion(s). ` +
        `${seen.printMode.suggestions === 0 ? "Suggestions do not appear in print mode here either (gated off with non-essential traffic disabled and a non-Anthropic base URL, likely), so this setup cannot say whether MonoCode's shape would emit them." : ""}`,
    refused,
    seen,
  };
}

async function printModeSuggestions(claudePath) {
  const fake = await startFakeMessages({ script });
  const dirs = makeTempDirs("cl-");
  const cfg = path.join(dirs.root, "cfg");
  mkdirSync(cfg);
  const n = fake.requests.length;
  const ch = spawn(claudePath, ["-p", "turn-9", "--output-format", "stream-json", "--verbose", "--model", "claude-sonnet-4-5", "--prompt-suggestions", "true"], {
    cwd: dirs.work,
    stdio: ["ignore", "pipe", "pipe"],
    env: isolatedEnv({ home: dirs.home, extra: { CLAUDE_CONFIG_DIR: cfg, ANTHROPIC_BASE_URL: fake.baseUrl, ANTHROPIC_API_KEY: DUMMY_KEY, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1" } }),
  });
  let out = "";
  ch.stdout.on("data", (d) => (out += d));
  const exit = await new Promise((res) => {
    const t = setTimeout(() => {
      ch.kill("SIGKILL");
      res("killed");
    }, 45000);
    ch.on("exit", (c) => {
      clearTimeout(t);
      res(c);
    });
  });
  await fake.close();
  const events = out.split("\n").filter((l) => l.startsWith("{")).map((l) => {
    try {
      return JSON.parse(l);
    } catch {
      return {};
    }
  });
  return { exit, requests: fake.requests.length - n, suggestions: events.filter((e) => e.type === "prompt_suggestion").length };
}

export const claudeChecks = [
  { id: "check-2", title: "Claude Code --append-system-prompt without -p, after resume and compaction", flag: "capabilities: claude instructions \"system\"", run: check2 },
  { id: "check-6", title: "Claude Code --resume with --fork-session and --resume-session-at", flag: "capabilities: claude fork \"atTurn\", rewind \"anyTurn\"", run: check6 },
  { id: "agents-md", title: "Claude Code AGENTS.md plugin and its option", flag: "#27", run: checkAgentsMd },
  { id: "tool-search", title: "Claude Code ENABLE_TOOL_SEARCH with an MCP server", flag: "#42 loading mode", run: checkToolSearch },
  { id: "git-ask", title: "Claude Code asks before git with Bash(git:*) in --settings, also for its read-only git commands", flag: "Mac test 10.1: claudeGitAsk.ts", run: checkGitAsk },
  { id: "prompt-suggestions", title: "Claude Code --prompt-suggestions without --print", flag: "capabilities: claude promptSuggestions", run: checkPromptSuggestions },
];
