// The memory extension (wave 2, lane K; plans/pi_box_shared_context.md B4,
// section 5.3; note AI section 4.2). One store per project, written only on
// your click, read by Pi through an index injected as a message.
//
// The store is the project's own files (box plan, section 5.2):
//   memory/INDEX.md     one line per entry; injected
//   memory/<topic>.md   bodies, pulled by line range
//   memory/proposed.md  what an agent proposed and you did not accept
//   research/INDEX.md   one line per saved page; injected
//   research/raw/*.md   saved pages, other people's text; never injected
//   research/proposed.md  index lines you did not accept
//   refs/INDEX.md       reference folders; injected
//
// What it does, and the Pi 1.4.0 hook it uses (dist/core/extensions/
// types.d.ts, read 28 Oct 2026; line numbers in tk_lane_K.md, Research):
// - `before_agent_start` (once per run): returns one custom message with the
//   three indexes, capped at 200 lines and 25,000 characters, lines at 150
//   (Qwen Code's `index-budget.ts` limits, note AF), naming what it left
//   out (case 1). Never the system prompt, so the prompt prefix stays
//   byte-stable for llama.cpp's cache (note AF rule 21). Injected on the
//   first run of a session, after a compaction, and when an index changed;
//   otherwise the copy already in the transcript stands.
// - `context` (every model request): when the transcript holds more than one
//   copy, only the newest is sent. This changes the prefix only when a new
//   copy was injected.
// - `session_compact`: the next run injects again. (`session_before_compact`
//   has no instructions field in its result in 1.4.0, so note AI's "the
//   summary must name every saved path" is not done.)
// - `tool_result`: notes which tools that return outside content this
//   session called, for the memory card. It changes no result.
// - Tools: `memory_search` (a literal, case-insensitive search of the three
//   folders; paths, lines and a short match, never bodies), `memory_propose`
//   (one entry, behind `ctx.ui.confirm`), `research_save` (a page the model
//   already fetched with its own tools, saved under research/raw; its index
//   line behind `ctx.ui.confirm`). A yes writes; a no, a timeout or no UI
//   sends the entry to proposed.md with the session id (case 48).
// - `/memory status` and `/memory reindex`, for the TUI. The card is the
//   main path: a command typed in MonoCode's composer may not reach a host
//   session (box plan, section 10).
//
// Corner cases (rules, section 4):
// - 46: the factory only registers (pi.on, pi.registerTool,
//   pi.registerCommand). It reads no file and no environment; the first
//   file read is at the first run.
// - 47: installed globally with tk-pi (`pi install /abs/path/tk-pi`), never
//   from a project's `.pi/extensions`, which RPC mode skips until trusted.
// - 48: nothing reaches an index without your yes. The agent's own write
//   tools can still edit these files: a policy rule (`write` and `edit` on
//   `memory/INDEX.md`: ask) is the guard for that, not this extension.
// No network call: `research_save` takes the page text the model already
// has; tk-pi does not fetch (rules, section 5, item 8).

import { constants, promises as fsp } from "node:fs";
import type { FileHandle } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";
import { askUser, cardText } from "../src/ask.ts";

// ---- The part of Pi's 1.4.0 API this file uses (Pi supplies the real one;
// tk-pi imports nothing from Pi, as src/pi-api.ts explains). ----

export interface MemoryUi {
  confirm(title: string, message: string, opts?: { timeout?: number; signal?: AbortSignal }): Promise<boolean>;
  notify(message: string, type?: "info" | "warning" | "error"): void;
}

export interface MemoryCtx {
  ui: MemoryUi;
  mode: "tui" | "rpc" | "json" | "print";
  hasUI: boolean;
  cwd: string;
  sessionManager: { getSessionId(): string };
  signal: AbortSignal | undefined;
}

export interface TextContent {
  type: "text";
  text: string;
}

export interface ToolResult {
  content: TextContent[];
  details: Record<string, unknown> | undefined;
  isError?: boolean;
}

export interface ToolDefinition {
  name: string;
  label: string;
  description: string;
  promptSnippet?: string;
  parameters: Record<string, unknown>;
  executionMode?: "sequential" | "parallel";
  annotations?: { readOnlyHint?: boolean; destructiveHint?: boolean; idempotentHint?: boolean; openWorldHint?: boolean };
  execute(
    toolCallId: string,
    params: Record<string, unknown>,
    signal: AbortSignal | undefined,
    onUpdate: unknown,
    ctx: MemoryCtx,
  ): Promise<ToolResult>;
}

export interface AgentMessageLike {
  role: string;
  customType?: string;
}

export interface MemoryApi {
  on(event: string, handler: (event: never, ctx: MemoryCtx) => unknown): () => void;
  registerTool(tool: ToolDefinition): void;
  registerCommand(name: string, options: { description?: string; handler: (args: string, ctx: MemoryCtx) => Promise<void> }): void;
}

// ---- Limits ----

export const MESSAGE_TYPE = "tk-memory-index";
export const FOLDERS = ["memory", "research", "refs"] as const;
export const MAX_LINES = 200;
export const MAX_CHARS = 25_000;
export const MAX_LINE = 150;
/** The longest index file read. */
const MAX_INDEX_BYTES = 1024 * 1024;
const MAX_SEARCH_FILES = 2_000;
const MAX_SEARCH_FILE_BYTES = 1024 * 1024;
const MAX_SEARCH_DEPTH = 6;
export const MAX_RESULTS = 50;
const MAX_SNIPPET = 200;
const MAX_ENTRY = 2_000;
const MAX_PAGE = 500_000;
export const CONFIRM_MS = 60_000;
/** Tools whose results are local files only; any other result counts as
 * outside content for the memory card. */
const LOCAL_TOOLS = new Set(["read", "ls", "find", "grep", "edit", "write"]);

export const HEADER = "Saved context (read before acting; pull files by line range):";

export interface MemoryOptions {
  /** The time, for proposed.md and saved pages. Defaults to the clock. */
  now?: () => Date;
  /** How long a memory card waits. */
  confirmMs?: number;
}

// ---- Files ----

/** A text with control characters (tabs and newlines too) as spaces. */
function oneLine(text: string): string {
  return text.replace(/[\u0000-\u001f\u007f-\u009f\u2028\u2029]/g, " ").replace(/ +/g, " ").trim();
}

function clip(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/** `rel` under `root`, or undefined when it would leave it. */
function under(root: string, rel: string): string | undefined {
  const abs = path.resolve(root, rel);
  const back = path.relative(root, abs);
  if (back === "" || back.startsWith("..") || path.isAbsolute(back)) return undefined;
  return abs;
}

/**
 * Reads a regular file, never through a link, at most `max` bytes. The
 * folders between `root` and the file must be real folders too: a project
 * whose `memory/` links to `~/.ssh` gets nothing read or injected.
 */
async function readPlain(root: string, rel: string, max: number): Promise<string | undefined> {
  const abs = under(root, rel);
  if (!abs || !(await realFolders(root, path.dirname(abs), false))) return undefined;
  let handle: FileHandle | undefined;
  try {
    handle = await fsp.open(abs, constants.O_RDONLY | constants.O_NOFOLLOW);
    const st = await handle.stat();
    if (!st.isFile()) return undefined;
    const buf = Buffer.alloc(Math.min(st.size, max));
    const { bytesRead } = await handle.read(buf, 0, buf.length, 0);
    return buf.subarray(0, bytesRead).toString("utf8");
  } catch {
    return undefined;
  } finally {
    await handle?.close();
  }
}

/** Every folder from `root` down to `dir` is a real folder (no link), made
 * when `make` and missing. */
async function realFolders(root: string, dir: string, make: boolean): Promise<boolean> {
  const rel = path.relative(root, dir);
  if (rel.startsWith("..") || path.isAbsolute(rel)) return false;
  let at = root;
  for (const part of rel === "" ? [] : rel.split(path.sep)) {
    at = path.join(at, part);
    try {
      const st = await fsp.lstat(at);
      if (!st.isDirectory() || st.isSymbolicLink()) return false;
    } catch (e) {
      if (!make || (e as NodeJS.ErrnoException).code !== "ENOENT") return false;
      try {
        await fsp.mkdir(at, { mode: 0o755 });
      } catch {
        return false;
      }
    }
  }
  return true;
}

/** Appends `text` to `rel` under `root`: real folders only, never through a
 * link, the file made if missing. */
async function appendPlain(root: string, rel: string, text: string): Promise<boolean> {
  const abs = under(root, rel);
  if (!abs || !(await realFolders(root, path.dirname(abs), true))) return false;
  let handle: FileHandle | undefined;
  try {
    handle = await fsp.open(abs, constants.O_WRONLY | constants.O_APPEND | constants.O_CREAT | constants.O_NOFOLLOW, 0o644);
    const st = await handle.stat();
    if (!st.isFile()) return false;
    await handle.write(text);
    return true;
  } catch {
    return false;
  } finally {
    await handle?.close();
  }
}

/** Writes a new file at `rel` (never over one), or undefined. */
async function createPlain(root: string, rel: string, text: string): Promise<boolean> {
  const abs = under(root, rel);
  if (!abs || !(await realFolders(root, path.dirname(abs), true))) return false;
  let handle: FileHandle | undefined;
  try {
    handle = await fsp.open(abs, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o644);
    await handle.write(text);
    return true;
  } catch {
    return false;
  } finally {
    await handle?.close();
  }
}

// ---- The index ----

export interface IndexText {
  text: string;
  /** Lines and characters left out by the caps. */
  leftOut: { lines: number; files: string[] };
  found: string[];
}

/** The message text from the three indexes under `cwd`, or undefined when
 * none exists. Pure over what it reads. */
export async function buildIndex(cwd: string): Promise<IndexText | undefined> {
  const parts: string[] = [HEADER];
  let lines = 0;
  let chars = HEADER.length;
  const leftOut = { lines: 0, files: [] as string[] };
  const found: string[] = [];
  for (const folder of FOLDERS) {
    const rel = `${folder}/INDEX.md`;
    const raw = await readPlain(cwd, rel, MAX_INDEX_BYTES);
    if (raw === undefined) continue;
    const body = raw
      .split(/\r?\n/)
      .map((l) => oneLine(l))
      .filter((l) => l !== "");
    if (body.length === 0) continue;
    found.push(rel);
    const heading = `\n\n## ${rel}`;
    if (lines >= MAX_LINES || chars + heading.length > MAX_CHARS) {
      leftOut.lines += body.length;
      leftOut.files.push(rel);
      continue;
    }
    parts.push(heading);
    chars += heading.length;
    let dropped = 0;
    for (const line of body) {
      const shown = `\n${clip(line, MAX_LINE)}`;
      if (lines >= MAX_LINES || chars + shown.length > MAX_CHARS) {
        dropped += 1;
        continue;
      }
      parts.push(shown);
      lines += 1;
      chars += shown.length;
    }
    if (dropped > 0) {
      leftOut.lines += dropped;
      leftOut.files.push(rel);
    }
  }
  if (found.length === 0) return undefined;
  if (leftOut.lines > 0) {
    parts.push(`\n\n(${leftOut.lines} more index lines left out; read ${leftOut.files.join(", ")} by line range.)`);
  }
  return { text: parts.join(""), leftOut, found };
}

function sha(text: string): string {
  return createHash("sha256").update(text).digest("hex");
}

// ---- Search ----

export interface Hit {
  path: string;
  line: number;
  text: string;
}

async function walk(root: string, rel: string, depth: number, out: string[]): Promise<void> {
  if (out.length >= MAX_SEARCH_FILES || depth > MAX_SEARCH_DEPTH) return;
  const abs = under(root, rel);
  if (!abs) return;
  let entries: import("node:fs").Dirent[];
  try {
    const st = await fsp.lstat(abs);
    if (!st.isDirectory() || st.isSymbolicLink()) return;
    entries = await fsp.readdir(abs, { withFileTypes: true });
  } catch {
    return;
  }
  entries.sort((a, b) => a.name.localeCompare(b.name));
  for (const e of entries) {
    if (out.length >= MAX_SEARCH_FILES) return;
    const child = `${rel}/${e.name}`;
    if (e.isDirectory()) await walk(root, child, depth + 1, out);
    else if (e.isFile()) out.push(child);
  }
}

/** A literal, case-insensitive search of the store. No pattern language, so
 * no query can make it slow (ReDoS). */
export async function search(cwd: string, query: string, folder?: string): Promise<{ hits: Hit[]; files: number; more: boolean }> {
  const needle = query.toLowerCase();
  const files: string[] = [];
  for (const f of FOLDERS) {
    if (folder === undefined || folder === f) await walk(cwd, f, 0, files);
  }
  const hits: Hit[] = [];
  let more = false;
  for (const rel of files) {
    const text = await readPlain(cwd, rel, MAX_SEARCH_FILE_BYTES);
    if (text === undefined || text.includes("\u0000")) continue;
    const lines = text.split(/\r?\n/);
    for (const [i, line] of lines.entries()) {
      if (!line.toLowerCase().includes(needle)) continue;
      if (hits.length >= MAX_RESULTS) {
        more = true;
        break;
      }
      hits.push({ path: rel, line: i + 1, text: clip(oneLine(line), MAX_SNIPPET) });
    }
    if (more) break;
  }
  return { hits, files: files.length, more };
}

// ---- Entries ----

/** A topic file name: lower-case letters, digits and `-`, 1 to 40, else `notes`. */
export function topicName(raw: unknown): string {
  const t = typeof raw === "string" ? raw.trim().toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "") : "";
  return t === "" || t === "index" || t === "proposed" ? "notes" : t.slice(0, 40);
}

function slug(text: string): string {
  const s = text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 60);
  return s === "" ? "page" : s;
}

function day(d: Date): string {
  return d.toISOString().slice(0, 10);
}

function text(t: string, details?: Record<string, unknown>, isError = false): ToolResult {
  return { content: [{ type: "text", text: t }], details, ...(isError ? { isError: true } : {}) };
}

function sessionOf(ctx: MemoryCtx): string {
  try {
    return oneLine(ctx.sessionManager.getSessionId()).slice(0, 80) || "unknown";
  } catch {
    return "unknown";
  }
}

// ---- The extension ----

export function createMemory(options: MemoryOptions = {}) {
  const now = options.now ?? (() => new Date());
  const confirmMs = options.confirmMs ?? CONFIRM_MS;
  // Per runtime; reset at each session_start.
  let lastHash: string | undefined;
  let mustInject = true;
  let outside = new Set<string>();

  async function ask(ctx: MemoryCtx, title: string, body: string): Promise<"yes" | "no" | "timeout" | "no_ui" | "aborted"> {
    // tk-pi's own asker: Pi resolves `confirm` false on its timeout; this
    // also ends a client that never answers (src/ask.ts).
    return askUser(ctx, title, body, confirmMs);
  }

  function outsideNote(): string {
    if (outside.size === 0) return "This session has called no tool that returns outside content.";
    return `This session read outside content through: ${[...outside].sort().slice(0, 8).join(", ")}. Text from a page, an email or a tool result can carry instructions.`;
  }

  async function propose(params: Record<string, unknown>, ctx: MemoryCtx): Promise<ToolResult> {
    const raw = typeof params.text === "string" ? params.text : "";
    const entry = raw.replace(/\r\n?/g, "\n").replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, " ").trim();
    if (entry === "") return text("memory_propose needs text.", undefined, true);
    if (entry.length > MAX_ENTRY) return text(`An entry holds at most ${MAX_ENTRY} characters; split it.`, undefined, true);
    const topic = topicName(params.topic);
    const line = `- [${topic}](${topic}.md): ${clip(oneLine(entry), MAX_LINE - topic.length - 10)}`;
    const body = [
      `Adds this line to memory/INDEX.md:`,
      cardText(line, 300),
      ``,
      `and this to memory/${topic}.md:`,
      cardText(entry, 1200),
      ``,
      outsideNote(),
    ].join("\n");
    const answer = await ask(ctx, "Save to tk memory?", body);
    if (answer === "yes") {
      const okTopic = await appendPlain(ctx.cwd, `memory/${topic}.md`, `\n${entry}\n`);
      const okIndex = okTopic && (await appendPlain(ctx.cwd, "memory/INDEX.md", `${line}\n`));
      if (!okIndex) return text("The user said yes, but memory/ could not be written (a link, or no permission). Nothing was saved.", { answer, saved: false }, true);
      mustInject = true;
      return text(`Saved: memory/${topic}.md and one line in memory/INDEX.md.`, { answer, saved: true, topic });
    }
    const stamp = `${now().toISOString()} session ${sessionOf(ctx)}`;
    const ok = await appendPlain(ctx.cwd, "memory/proposed.md", `- ${stamp} [${topic}] (${answer}): ${oneLine(entry)}\n`);
    return text(
      ok
        ? `Not saved (${answer === "no_ui" ? "no one to ask" : answer}). The entry is in memory/proposed.md for the user to promote.`
        : `Not saved (${answer}), and memory/proposed.md could not be written.`,
      { answer, saved: false, proposed: ok },
    );
  }

  async function saveResearch(params: Record<string, unknown>, ctx: MemoryCtx): Promise<ToolResult> {
    const url = typeof params.url === "string" ? params.url.trim() : "";
    const title = oneLine(typeof params.title === "string" ? params.title : "");
    const page = typeof params.text === "string" ? params.text : "";
    let parsed: URL;
    try {
      parsed = new URL(url);
    } catch {
      return text("research_save needs the page's http or https url.", undefined, true);
    }
    if ((parsed.protocol !== "http:" && parsed.protocol !== "https:") || url.length > 2_000) {
      return text("research_save takes http and https urls only.", undefined, true);
    }
    if (page.trim() === "") return text("research_save needs the page text you fetched.", undefined, true);
    if (page.length > MAX_PAGE) return text(`A saved page holds at most ${MAX_PAGE} characters.`, undefined, true);
    const shownTitle = clip(title || parsed.hostname, 120);
    const date = day(now());
    let file = "";
    for (let n = 1; n <= 50 && file === ""; n += 1) {
      const name = `${date}-${slug(shownTitle)}${n > 1 ? `-${n}` : ""}.md`;
      const front = [
        "---",
        `source: ${JSON.stringify(parsed.toString())}`,
        `fetched: ${JSON.stringify(now().toISOString())}`,
        `title: ${JSON.stringify(shownTitle)}`,
        `savedBy: "model"`,
        "---",
        "",
      ].join("\n");
      if (await createPlain(ctx.cwd, `research/raw/${name}`, `${front}${page.replace(/\r\n?/g, "\n")}\n`)) file = name;
    }
    if (file === "") return text("research/raw/ could not be written (a link, or no permission). Nothing was saved.", undefined, true);
    const line = `- [${clip(shownTitle, 80)}](raw/${file}): ${clip(parsed.toString(), 60)}`;
    const answer = await ask(
      ctx,
      "Add a saved page to the research index?",
      [`Saved research/raw/${file} from ${cardText(parsed.toString(), 300)}.`, ``, `Adds this line to research/INDEX.md:`, cardText(line, 300), ``, outsideNote()].join("\n"),
    );
    if (answer === "yes" && (await appendPlain(ctx.cwd, "research/INDEX.md", `${line}\n`))) {
      mustInject = true;
      return text(`Saved research/raw/${file} and indexed it.\nFirst lines:\n${clip(oneLine(page.slice(0, 400)), 200)}`, { answer, file, indexed: true });
    }
    const stamp = `${now().toISOString()} session ${sessionOf(ctx)}`;
    await appendPlain(ctx.cwd, "research/proposed.md", `- ${stamp} (${answer}): ${line.slice(2)}\n`);
    return text(`Saved research/raw/${file}; not indexed (${answer}). The index line is in research/proposed.md.`, { answer, file, indexed: false });
  }

  return function memory(pi: MemoryApi): void {
    pi.on("session_start", () => {
      lastHash = undefined;
      mustInject = true;
      outside = new Set();
    });

    pi.on("session_compact", () => {
      mustInject = true;
    });

    pi.on("before_agent_start", async (event: never, ctx: MemoryCtx) => {
      const e = event as { messageType?: string };
      if (e.messageType !== "user") return undefined;
      const index = await buildIndex(ctx.cwd);
      if (!index) return undefined;
      const hash = sha(index.text);
      if (!mustInject && hash === lastHash) return undefined;
      mustInject = false;
      lastHash = hash;
      return { message: { customType: MESSAGE_TYPE, content: index.text, display: false } };
    });

    pi.on("context", (event: never) => {
      const messages = (event as { messages?: AgentMessageLike[] }).messages;
      if (!Array.isArray(messages)) return undefined;
      const copies = messages.filter((m) => m?.role === "custom" && m.customType === MESSAGE_TYPE);
      if (copies.length <= 1) return undefined;
      const keep = copies[copies.length - 1];
      return { messages: messages.filter((m) => !(m?.role === "custom" && m.customType === MESSAGE_TYPE) || m === keep) };
    });

    pi.on("tool_result", (event: never) => {
      const name = (event as { toolName?: unknown }).toolName;
      if (typeof name === "string" && !LOCAL_TOOLS.has(name)) outside.add(clip(oneLine(name), 60));
      return undefined;
    });

    pi.registerTool({
      name: "memory_search",
      label: "Memory search",
      description:
        "Search the project's saved context (memory/, research/, refs/) for literal text. Returns file paths, line numbers and the matching line; read the file by line range for more.",
      promptSnippet: "memory_search: find saved notes and pages by literal text",
      parameters: {
        type: "object",
        properties: {
          query: { type: "string", minLength: 1, maxLength: 200, description: "Literal text, case-insensitive" },
          folder: { type: "string", enum: [...FOLDERS], description: "Only this folder" },
        },
        required: ["query"],
        additionalProperties: false,
      },
      annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false },
      async execute(_id, params, _signal, _onUpdate, ctx) {
        const query = typeof params.query === "string" ? params.query : "";
        if (query.trim() === "") return text("memory_search needs a query.", undefined, true);
        const folder = typeof params.folder === "string" && (FOLDERS as readonly string[]).includes(params.folder) ? params.folder : undefined;
        const { hits, files, more } = await search(ctx.cwd, query.slice(0, 200), folder);
        if (hits.length === 0) return text(`No match in ${files} saved files.`, { hits: 0, files });
        const lines = hits.map((h) => `${h.path}:${h.line}: ${h.text}`);
        if (more) lines.push(`(more matches left out after ${MAX_RESULTS}; narrow the query or the folder)`);
        return text(lines.join("\n"), { hits: hits.length, files, more });
      },
    });

    pi.registerTool({
      name: "memory_propose",
      label: "Propose a memory",
      description:
        "Propose one entry for the project's memory. The user approves or declines it; a declined or unanswered entry goes to memory/proposed.md. Give the text and a short topic (a file name such as build or style).",
      promptSnippet: "memory_propose: ask the user to save one note to the project's memory",
      parameters: {
        type: "object",
        properties: {
          text: { type: "string", minLength: 1, maxLength: MAX_ENTRY },
          topic: { type: "string", maxLength: 40 },
        },
        required: ["text"],
        additionalProperties: false,
      },
      executionMode: "sequential",
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      execute: (_id, params, _signal, _onUpdate, ctx) => propose(params, ctx),
    });

    pi.registerTool({
      name: "research_save",
      label: "Save a page",
      description:
        "Save a web page you already fetched to research/raw/ with its url, and propose a line for research/INDEX.md, which the user approves. Pass the page's url, a title and the text.",
      promptSnippet: "research_save: keep a fetched page under research/ for later",
      parameters: {
        type: "object",
        properties: {
          url: { type: "string", minLength: 1, maxLength: 2000 },
          title: { type: "string", maxLength: 200 },
          text: { type: "string", minLength: 1, maxLength: MAX_PAGE },
        },
        required: ["url", "text"],
        additionalProperties: false,
      },
      executionMode: "sequential",
      annotations: { readOnlyHint: false, destructiveHint: false, idempotentHint: false, openWorldHint: false },
      execute: (_id, params, _signal, _onUpdate, ctx) => saveResearch(params, ctx),
    });

    pi.registerCommand("memory", {
      description: "tk memory: status, or reindex (inject the index again on the next prompt)",
      handler: async (args, ctx) => {
        const word = args.trim().toLowerCase();
        if (word === "reindex") {
          mustInject = true;
          ctx.ui.notify("tk memory: the index goes in again with the next prompt.", "info");
          return;
        }
        const index = await buildIndex(ctx.cwd);
        const pending = await readPlain(ctx.cwd, "memory/proposed.md", MAX_INDEX_BYTES);
        const proposed = pending ? pending.split("\n").filter((l) => l.startsWith("- ")).length : 0;
        const found = index ? index.found.join(", ") : "no INDEX.md in memory/, research/ or refs/";
        const left = index && index.leftOut.lines > 0 ? `; ${index.leftOut.lines} lines over the cap` : "";
        ctx.ui.notify(`tk memory: ${found}${left}; ${proposed} proposed entries waiting in memory/proposed.md.`, "info");
      },
    });
  };
}

export default createMemory();
