// OpenClaw's own control files. The shared core's control list does not name
// them; an allowed call whose input names one is asked here instead, as the
// core does for its own list. A request for integration w2 adds them to the
// core and tk-core.
//
// Why each one (openclaw 2026.10.8, docs/gateway/configuration.md:10-12,
// docs/help/environment.md:39-41, docs/plugins/manage-plugins.md):
// - `.openclaw/`: ~/.openclaw holds openclaw.json (plugins.entries.tk.enabled,
//   plugins.load.paths, hook timeouts) and extensions/; a workspace's
//   .openclaw/extensions is a plugin root. The config hot-reloads.
// - `openclaw.json`: the config file by name, anywhere.
// - `openclaw_config_path`, `openclaw_state_dir`, `openclaw_home`: move it.

import { fold, decodeEscapes } from "./core/fold.ts";
import { unquote, type Action } from "./core/policy.ts";

export const OPENCLAW_CONTROL_PATHS = [".openclaw/", "openclaw.json", "openclaw_config_path", "openclaw_state_dir", "openclaw_home"] as const;

export function openclawControlPath(inputText: string): string | undefined {
  const folded = fold(decodeEscapes(inputText));
  const unquoted = unquote(folded);
  return OPENCLAW_CONTROL_PATHS.find((p) => folded.includes(p) || unquoted.includes(p));
}

/** Narrows an allow to ask when the input names an OpenClaw control path. Never widens. */
export function narrowForControl(action: Action, inputText: string): { action: Action; control?: string } {
  if (action !== "allow") return { action };
  const control = openclawControlPath(inputText);
  return control ? { action: "ask", control } : { action };
}
