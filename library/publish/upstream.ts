// The copy of a server.json that goes to the official registry (lane G, T3.3).
//
// The registry refuses a `_meta["io.modelcontextprotocol.registry/publisher-provided"]`
// object over 4,096 bytes as Go's json.Marshal writes it
// (internal/validators/validators.go:727-737 at 9c4a96c). tk's object grows
// with trust records (lane H's scans), so tk trims its own object, largest
// and most local first, until the whole publisher object fits, and says what
// it took out. Other publishers' keys are never touched.
import { goJsonLength } from "../mirror/canonical.ts";
import { PUBLISHER_PROVIDED, TK_META_KEY, isRecord } from "../validate/types.ts";

export const UPSTREAM_META_CAP = 4096;

/** Paths in tk's object, in the order they are dropped. */
const DROP_ORDER: string[][] = [["trust", "scans"], ["trust", "capabilityDrift"], ["trust"], ["annotationsOverride"], ["harnessPresets"], ["permissions"], ["exposure"]];

export type Trimmed = { server: Record<string, unknown>; dropped: string[]; bytes: number; error?: string };

export function trimForUpstream(server: Record<string, unknown>): Trimmed {
  const copy = structuredClone(server);
  const meta = isRecord(copy["_meta"]) ? (copy["_meta"] as Record<string, unknown>) : undefined;
  const pp = meta && isRecord(meta[PUBLISHER_PROVIDED]) ? (meta[PUBLISHER_PROVIDED] as Record<string, unknown>) : undefined;
  if (!pp) return { server: copy, dropped: [], bytes: 0 };
  const dropped: string[] = [];
  const size = () => goJsonLength(pp);
  const tk = isRecord(pp[TK_META_KEY]) ? (pp[TK_META_KEY] as Record<string, unknown>) : undefined;
  for (const path of DROP_ORDER) {
    if (size() <= UPSTREAM_META_CAP || !tk) break;
    let holder: Record<string, unknown> | undefined = tk;
    for (const p of path.slice(0, -1)) holder = holder && isRecord(holder[p]) ? (holder[p] as Record<string, unknown>) : undefined;
    const last = path.at(-1)!;
    if (holder && last in holder) {
      delete holder[last];
      dropped.push(path.join("."));
    }
  }
  if (size() > UPSTREAM_META_CAP && tk) {
    delete pp[TK_META_KEY];
    dropped.push(TK_META_KEY);
  }
  const bytes = size();
  if (bytes > UPSTREAM_META_CAP) return { server: copy, dropped, bytes, error: `the publisher object is ${bytes} bytes without tk's data; other keys must shrink below ${UPSTREAM_META_CAP}` };
  if (Object.keys(pp).length === 0) {
    delete meta![PUBLISHER_PROVIDED];
    if (Object.keys(meta!).length === 0) delete copy["_meta"];
  }
  return { server: copy, dropped, bytes };
}
