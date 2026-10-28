// Hermes's own control files. The shared core's control list does not name
// them; an allowed call whose input names one is asked here instead, as the
// core does for its own list. A request for integration w2 adds them to the
// core and tk-core.
//
// Why each one (Hermes v0.22.5, bf0203a7):
// - `.hermes/`: HERMES_HOME's default, ~/.hermes: config.yaml holds
//   plugins.enabled and plugins.disabled (disabled always wins,
//   hermes_cli/plugins_discovery.py:112-131), approvals.mode (`off` makes
//   Hermes's gate approve everything, tools/approval.py:990-991), the
//   plugins/ folder with this plugin, profiles/, and
//   shell-hooks-allowlist.json; a project's .hermes/plugins/ loads with
//   HERMES_ENABLE_PROJECT_PLUGINS.
// - `hermes_home`: HERMES_HOME moves all of it (hermes_constants.py:188-195).
// - `hermes_safe_mode`: HERMES_SAFE_MODE=1 skips plugin discovery
//   (hermes_cli/plugins.py:1259).
// - `hermes_yolo_mode`: HERMES_YOLO_MODE turns Hermes's own approvals off
//   (tools/approval.py:46).
// - `hermes_enable_project_plugins`, `hermes_accept_hooks`: load a project's
//   plugins, accept shell hooks without a prompt.

import { fold, decodeEscapes } from "./core/fold.ts";
import { unquote, type Action } from "./core/policy.ts";

export const HERMES_CONTROL_PATHS = [".hermes/", "hermes_home", "hermes_safe_mode", "hermes_yolo_mode", "hermes_enable_project_plugins", "hermes_accept_hooks"] as const;

export function hermesControlPath(inputText: string): string | undefined {
  const folded = fold(decodeEscapes(inputText));
  const unquoted = unquote(folded);
  return HERMES_CONTROL_PATHS.find((p) => folded.includes(p) || unquoted.includes(p));
}

/** Narrows an allow to ask when the input names a Hermes control path. Never widens. */
export function narrowForControl(action: Action, inputText: string): { action: Action; control?: string } {
  if (action !== "allow") return { action };
  const control = hermesControlPath(inputText);
  return control ? { action: "ask", control } : { action };
}
