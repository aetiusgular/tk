// Gemini's own control files. The shared core's control list (src/core/
// policy.ts CONTROL_PATHS) predates this adapter and does not name them; an
// allowed call whose input names one is asked here instead, the same rule the
// core applies to its own list. A request for integration w2 adds them to the
// core and to tk-core, after which this file can go.
//
// Why each one (gemini-cli v0.66.0):
// - `.gemini/`: settings.json holds `hooks`, `hooksConfig.enabled` (false runs
//   no hook at all) and `hooksConfig.disabled` (switches a hook off by name;
//   cli/src/config/settingsSchema.ts:2548-2580, hookRegistry.ts:256-270),
//   and `security.*`; a project's .gemini/settings.json outranks the user's in
//   a trusted folder; extensions/ holds this extension and
//   extension-enablement.json; policies/ holds the user tier.
// - `gemini-cli/`: /etc/gemini-cli/settings.json and system-defaults.json, the
//   managed files.
// - `geminicli/`: the macOS managed folder (/Library/Application Support/GeminiCli).
// - `gemini_cli_system`: GEMINI_CLI_SYSTEM_SETTINGS_PATH and its defaults
//   sibling, which move the managed files.

import { fold, decodeEscapes } from "./core/fold.ts";
import { unquote, type Action } from "./core/policy.ts";

export const GEMINI_CONTROL_PATHS = [".gemini/", "gemini-cli/", "geminicli/", "gemini_cli_system"] as const;

/** The first Gemini control path the input names, read the way the core reads its own list. */
export function geminiControlPath(inputText: string): string | undefined {
  const folded = fold(decodeEscapes(inputText));
  const unquoted = unquote(folded);
  return GEMINI_CONTROL_PATHS.find((p) => folded.includes(p) || unquoted.includes(p));
}

/** Narrows an allow to ask when the input names a Gemini control path. Never widens. */
export function narrowForControl(action: Action, inputText: string): { action: Action; control?: string } {
  if (action !== "allow") return { action };
  const control = geminiControlPath(inputText);
  return control ? { action: "ask", control } : { action };
}
