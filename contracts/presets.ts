// The harness preset table, assembled from one file per agent in
// crates/tk-cli/src/connect/presets/ (integration w2 split the old
// contracts/harness-presets.json so each wave 4 lane owns its agents' rows
// by file). index.json names the two lists and their order. The same rules
// as `assemble` in crates/tk-cli/src/connect/presets.rs: every listed id has
// a file whose `id` is its name, no id is listed twice, and every file is
// listed. The assembled object follows harness-presets.schema.json.

import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

export const PRESETS_DIR = fileURLToPath(new URL("../crates/tk-cli/src/connect/presets/", import.meta.url));

export type PresetTable = { $comment?: string; version: number; harnesses: Record<string, unknown>[]; planned: Record<string, unknown>[] };

export function loadPresetTable(dir: string = PRESETS_DIR): PresetTable {
  const index = JSON.parse(readFileSync(join(dir, "index.json"), "utf8")) as { $comment?: string; version: number; harnesses: string[]; planned: string[] };
  const files = new Set(readdirSync(dir).filter((f) => f.endsWith(".json") && f !== "index.json").map((f) => f.slice(0, -".json".length)));
  const seen = new Set<string>();
  const list = (ids: string[]): Record<string, unknown>[] =>
    ids.map((id) => {
      if (seen.has(id)) throw new Error(`index.json: ${id} listed twice`);
      seen.add(id);
      if (!files.has(id)) throw new Error(`index.json names ${id}, which has no row file`);
      const row = JSON.parse(readFileSync(join(dir, `${id}.json`), "utf8")) as Record<string, unknown>;
      if (row.id !== id) throw new Error(`${id}.json: its id is not ${id}`);
      return row;
    });
  const table: PresetTable = { version: index.version, harnesses: list(index.harnesses), planned: list(index.planned) };
  if (index.$comment !== undefined) table.$comment = index.$comment;
  for (const f of files) if (!seen.has(f)) throw new Error(`${f}.json is in no list of index.json`);
  return table;
}
