// Shared types of the tk library validator (lane E, T3.1).

export type Kind = "server" | "skill" | "bundle";

export type Severity = "error" | "warning";

/** One validation finding. `rule` is stable and tested by name; `path` is a
 *  JSON path inside the entry (empty for the whole entry); `message` never
 *  repeats a secret-like value. */
export type Finding = { rule: string; severity: Severity; path: string; message: string };

export const TK_META_KEY = "io.github.aetiusgular.tk";
export const PUBLISHER_PROVIDED = "io.modelcontextprotocol.registry/publisher-provided";

/** The tk data object of contracts/entry.schema.json, typed loosely: the
 *  schema is the contract; this type names the fields the rules read. */
export type TkMeta = {
  kind: Kind;
  tier: "own" | "curated" | "mirrored";
  status: "active" | "deprecated" | "deleted";
  runner: { kind: "image"; image: string; imageDigest?: string } | { kind: "srt"; srtProfile: string } | { kind: "host"; note: "runs as you" } | { kind: "remote" };
  exposure: { default: Exposure } & Record<string, Exposure>;
  entrySkill?: string;
  annotationsOverride?: Record<string, ToolAnnotations>;
  permissions?: { network?: { outbound?: { allow_host?: string[]; allow_port?: number[] } } };
  trust: { contentSha256: string; scans: unknown[]; liveness: { state: string; checkedAt?: string }; [k: string]: unknown };
  harnessPresets?: Record<string, Record<string, unknown>>;
};

export type Exposure = "codemode" | "deferred" | "direct" | "hidden";

export type ToolAnnotations = { title?: string; readOnlyHint?: boolean; destructiveHint?: boolean; idempotentHint?: boolean; openWorldHint?: boolean };

/** library/schema/tools.schema.json */
export type ToolsRecord = {
  source: string;
  recordedAt: string;
  dynamic?: boolean;
  tools: { name: string; title?: string; description?: string; inputSchema?: object; outputSchema?: object; annotations?: ToolAnnotations }[];
};

/** A catalog entry as loaded from library/catalog/<id>/. */
export type LoadedEntry = {
  /** The folder name under library/catalog/: the short id used by sets and exports. */
  id: string;
  kind: Kind;
  /** Absolute folder of the entry. */
  dir: string;
  /** The manifest file name inside `dir`. */
  file: string;
  /** The parsed manifest (server.json, skill.json or .claude-plugin/plugin.json). */
  data: Record<string, unknown>;
  /** tools.json beside a server.json, when present. */
  tools?: ToolsRecord;
  /** The text of SKILL.md for a skill whose package points into this repository, when found. */
  skillMd?: { path: string; text: string };
};

export type Catalog = {
  dir: string;
  entries: LoadedEntry[];
  sets: { version: 1; sets: Record<string, { title: string; description: string; entries: string[] }> };
  brands: string[];
};

export function getMeta(kind: Kind, data: Record<string, unknown>): TkMeta | undefined {
  if (kind === "bundle") {
    const metadata = data["metadata"];
    if (!isRecord(metadata)) return undefined;
    const m = metadata[TK_META_KEY];
    return isRecord(m) ? (m as unknown as TkMeta) : undefined;
  }
  const meta = data["_meta"];
  if (!isRecord(meta)) return undefined;
  const pp = meta[PUBLISHER_PROVIDED];
  if (!isRecord(pp)) return undefined;
  const m = pp[TK_META_KEY];
  return isRecord(m) ? (m as unknown as TkMeta) : undefined;
}

export function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

export function finding(rule: string, severity: Severity, path: string, message: string): Finding {
  return { rule, severity, path, message };
}

export function hasErrors(findings: Finding[]): boolean {
  return findings.some((f) => f.severity === "error");
}
