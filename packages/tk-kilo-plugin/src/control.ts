// Kilo's own control files. The shared core's control list (src/core/
// policy.ts CONTROL_PATHS) names OpenCode's (`opencode.json`,
// `.config/opencode/`, `.cache/opencode/`) but not Kilo's; an allowed call
// whose input names one is asked here instead, as the core does for its own
// list. A request for integration w2 adds them to the core and tk-core.
//
// Why each one (Kilo v7.8.18):
// - `.config/kilo/`: the global config (kilo.json, opencode.jsonc that `kilo
//   plugin --global` writes) and plugin/, where every .ts file is loaded as a
//   plugin (config/plugin.ts:18-29; kilo-docs plugins.md:65-98).
// - `.kilo/`, `.kilocode/`: a project's config and plugin folders, loaded with
//   no trust prompt (config/config.ts:786-796, 826-834).
// - `kilo.json`, `kilo.jsonc`: the config file by name, anywhere.
// - `kilo_config`: KILO_CONFIG_CONTENT and KILO_CONFIG, which replace the config.
// - `kilo_pure`: KILO_PURE=1 skips every external plugin, tk's included.

import { fold, decodeEscapes } from "./core/fold.ts";
import { unquote, type Action } from "./core/policy.ts";

export const KILO_CONTROL_PATHS = [".config/kilo/", ".kilo/", ".kilocode/", "kilo.json", "kilo_config", "kilo_pure"] as const;

export function kiloControlPath(inputText: string): string | undefined {
  const folded = fold(decodeEscapes(inputText));
  const unquoted = unquote(folded);
  return KILO_CONTROL_PATHS.find((p) => folded.includes(p) || unquoted.includes(p));
}

/** Narrows an allow to ask when the input names a Kilo control path. Never widens. */
export function narrowForControl(action: Action, inputText: string): { action: Action; control?: string } {
  if (action !== "allow") return { action };
  const control = kiloControlPath(inputText);
  return control ? { action: "ask", control } : { action };
}
