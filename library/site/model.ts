// The library browser's data (lane J, T5): one card per catalog entry, built
// from library/catalog/ and contracts/harness-presets.json at build time.
// Everything a card shows is computed here, as plain strings, so the page
// (page.ts) only escapes and lays them out and the browser runs no logic on
// entry text. An entry is a stranger's text once the mirror (lane G) fills
// the catalog: links are kept only when they are https, and nothing here is
// ever written as markup.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { Catalog, Finding, LoadedEntry } from "../validate/index.ts";
import { getMeta } from "../validate/types.ts";
import { REPO_ROOT } from "../validate/schemas.ts";

export type Harness = { id: string; name: string; write: "write" | "print" };

export type AddLine = { harness: string; name: string; line: string; note?: string | undefined };

export type Card = {
  id: string;
  kind: "server" | "skill" | "bundle";
  title: string;
  description: string;
  version: string;
  tier: string;
  status: string;
  runs: string;
  network: string[];
  exposure: string;
  tools: { name: string; readOnly: boolean | undefined }[];
  toolsNote: string;
  allowedTools: string[];
  entrySkill?: string | undefined;
  trust: { label: string; value: string }[];
  warnings: string[];
  add: AddLine[];
  addNote: string;
  links: { label: string; href: string }[];
  sets: string[];
  search: string;
};

export type Site = {
  builtAt: string;
  commit: string;
  cards: Card[];
  sets: { id: string; title: string; description: string }[];
  harnesses: Harness[];
};

const ZERO_HASH = "0".repeat(64);
const PRESETS = join(REPO_ROOT, "contracts", "harness-presets.json");

/** The harnesses `tk connect` knows, in the preset table's order. */
export function loadHarnesses(path = PRESETS): Harness[] {
  const t = JSON.parse(readFileSync(path, "utf8")) as { harnesses: { id: string; name: string; write: string }[] };
  return t.harnesses.map((h) => ({ id: h.id, name: h.name, write: h.write === "print" ? "print" : "write" }));
}

/** A link is kept only as an absolute https URL with no credentials. */
export function safeHref(u: unknown): string | undefined {
  if (typeof u !== "string" || u.length > 2048) return undefined;
  let url: URL;
  try {
    url = new URL(u);
  } catch {
    return undefined;
  }
  if (url.protocol !== "https:" || url.username || url.password) return undefined;
  return url.href;
}

/** POSIX shell quoting for a path in a printed command line. */
export function shellQuote(s: string): string {
  return /^[A-Za-z0-9_./-]+$/.test(s) ? s : `'${s.replaceAll("'", `'\\''`)}'`;
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function runsText(entry: LoadedEntry): string {
  const meta = getMeta(entry.kind, entry.data);
  const r = meta?.runner;
  if (!r) return "not stated";
  switch (r.kind) {
    case "remote": {
      const hosts = remoteHosts(entry);
      return hosts.length ? `remote: ${hosts.join(", ")}` : "remote";
    }
    case "image":
      return `container image ${r.image}${r.imageDigest ? `@${r.imageDigest}` : " (no digest pinned)"}`;
    case "srt":
      return `under srt, profile ${r.srtProfile}`;
    case "host":
      return "on your machine, as you (no sandbox)";
    default:
      return "not stated";
  }
}

function remoteHosts(entry: LoadedEntry): string[] {
  const remotes = entry.data["remotes"];
  if (!Array.isArray(remotes)) return [];
  const out: string[] = [];
  for (const r of remotes) {
    const href = safeHref((r as Record<string, unknown>)?.["url"]);
    if (href) out.push(new URL(href).host);
    else if (typeof (r as Record<string, unknown>)?.["url"] === "string") out.push("a non-https url");
  }
  return [...new Set(out)];
}

function exposureText(entry: LoadedEntry): string {
  const ex = getMeta(entry.kind, entry.data)?.exposure;
  if (!ex) return "not stated";
  const others = Object.entries(ex)
    .filter(([k]) => k !== "default")
    .map(([k, v]) => `${k}: ${v}`);
  return others.length ? `${ex.default} (${others.join(", ")})` : String(ex.default);
}

function trustRows(entry: LoadedEntry): { label: string; value: string }[] {
  const t = getMeta(entry.kind, entry.data)?.trust as Record<string, unknown> | undefined;
  if (!t) return [{ label: "records", value: "none" }];
  const rows: { label: string; value: string }[] = [];
  rows.push({ label: "namespace proof", value: str(t["namespaceProof"]) || "none" });
  const prov = t["provenance"] as Record<string, unknown> | undefined;
  rows.push({ label: "provenance", value: str(prov?.["kind"]) || "none" });
  const hash = str(t["contentSha256"]);
  rows.push({ label: "content hash", value: hash === ZERO_HASH || !hash ? "placeholder, not computed yet" : `sha256 ${hash}` });
  const scans = Array.isArray(t["scans"]) ? (t["scans"] as Record<string, unknown>[]) : [];
  if (scans.length === 0) rows.push({ label: "scans", value: "none recorded" });
  for (const s of scans) {
    const n = typeof s["findings"] === "number" ? s["findings"] : "?";
    rows.push({ label: "scan", value: `${str(s["scanner"])} ${str(s["scannerVersion"])} on ${str(s["date"])}: ${n} findings` });
  }
  const live = t["liveness"] as Record<string, unknown> | undefined;
  const state = str(live?.["state"]) || "unknown";
  rows.push({ label: "liveness", value: live?.["checkedAt"] ? `${state}, checked ${str(live["checkedAt"])}` : `${state}, never checked` });
  const drift = t["capabilityDrift"] as Record<string, unknown> | undefined;
  if (drift) {
    const changed = Array.isArray(drift["changed"]) ? (drift["changed"] as unknown[]).map(str).join(", ") : "";
    rows.push({ label: "changed since", value: `${str(drift["fromVersion"])} to ${str(drift["toVersion"])}: ${changed}` });
  }
  return rows;
}

/** The `tk connect` line per harness for a server, from the root of a tk
 *  clone. A host-runner server (the gateway, computer use) has no `--from`
 *  form (integration w1, finding X5); skills and bundles install through an
 *  export. */
export function addLines(entry: LoadedEntry, harnesses: Harness[]): { lines: AddLine[]; note: string } {
  const rel = `library/catalog/${entry.id}/${entry.file}`;
  if (entry.kind !== "server") {
    return {
      lines: [],
      note: `A ${entry.kind} installs through an export: node library/cli.ts export <set> <out>, then pi install <out>/<set>/pi/<bundle> or claude plugin marketplace add <out>/<set>/claude.`,
    };
  }
  const runner = getMeta(entry.kind, entry.data)?.runner;
  if (runner?.kind === "host") {
    if (entry.id === "docker-mcp-gateway") {
      return {
        lines: harnesses.map((h) => ({
          harness: h.id,
          name: h.name,
          line: `tk connect ${h.id} --gateway docker --profile <profile>`,
          note: h.write === "print" ? "prints the entry; paste it yourself" : undefined,
        })),
        note: "The gateway is connected by profile, not from this file. Dry run by default; --write asks first.",
      };
    }
    return { lines: [], note: "Runs on your machine as you; it has no tk connect --from line yet (wave 2 lane K for computer use)." };
  }
  return {
    lines: harnesses.map((h) => ({
      harness: h.id,
      name: h.name,
      line: `tk connect ${h.id} --from ${shellQuote(rel)}`,
      note: h.write === "print" ? "prints the entry; paste it yourself" : undefined,
    })),
    note: "Run from the root of a tk clone. Each line is a dry run: it prints the file and the diff. Add --write to apply it to your user-level file after a yes.",
  };
}

export function buildCard(entry: LoadedEntry, harnesses: Harness[], findings: Finding[], sets: string[]): Card {
  const d = entry.data;
  const meta = getMeta(entry.kind, d);
  const tools = (entry.tools?.tools ?? []).map((t) => {
    const override = meta?.annotationsOverride?.[t.name]?.readOnlyHint;
    return { name: t.name, readOnly: override ?? t.annotations?.readOnlyHint };
  });
  const toolsNote = entry.tools ? `recorded ${entry.tools.recordedAt}${entry.tools.dynamic ? ", the list can change at run time" : ""}` : "";
  const allowed = Array.isArray(d["allowedTools"]) ? (d["allowedTools"] as unknown[]).map(str).filter(Boolean) : [];
  const links: { label: string; href: string }[] = [];
  const web = safeHref(d["websiteUrl"]);
  if (web) links.push({ label: "website", href: web });
  const repo = safeHref((d["repository"] as Record<string, unknown> | undefined)?.["url"]);
  if (repo) links.push({ label: "source", href: repo });
  const add = addLines(entry, harnesses);
  const title = str(d["title"]) || entry.id;
  const description = str(d["description"]);
  const card: Card = {
    id: entry.id,
    kind: entry.kind,
    title,
    description,
    version: str(d["version"]),
    tier: str(meta?.tier) || "not stated",
    status: str(meta?.status) || "not stated",
    runs: runsText(entry),
    network: meta?.permissions?.network?.outbound?.allow_host ?? [],
    exposure: exposureText(entry),
    tools,
    toolsNote,
    allowedTools: allowed,
    entrySkill: meta?.entrySkill,
    trust: trustRows(entry),
    warnings: findings.map((f) => `${f.severity}: [${f.rule}] ${f.message}`),
    add: add.lines,
    addNote: add.note,
    links,
    sets,
    search: "",
  };
  card.search = [entry.id, title, description, ...tools.map((t) => t.name), ...allowed, ...sets].join(" ").toLowerCase();
  return card;
}

export function buildSite(catalog: Catalog, findings: Map<string, Finding[]>, harnesses: Harness[], commit: string, now = new Date()): Site {
  const setsOf = (id: string) =>
    Object.entries(catalog.sets.sets)
      .filter(([, s]) => s.entries.includes(id))
      .map(([k]) => k);
  return {
    builtAt: now.toISOString().replace(/\.\d{3}Z$/, "Z"),
    commit,
    cards: catalog.entries.map((e) => buildCard(e, harnesses, findings.get(e.id) ?? [], setsOf(e.id))),
    sets: Object.entries(catalog.sets.sets).map(([id, s]) => ({ id, title: s.title, description: s.description })),
    harnesses,
  };
}
