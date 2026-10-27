// The text the reject list and the shell check run over, for one Pi call.
//
// A shell tool (`bash`, `powershell`: Pi 1.4.0 BashToolInput and
// PowerShellToolInput both carry `command`, types.d.ts:942-949) gives its
// command line as it is, so the shell check sees what the shell will read.
// Any other tool gives every key and every string, number and boolean value
// of its input, one per line, unescaped: a JSON string would add backslashes
// (which the shell check reads as unreadable) and hide line breaks.

import { SHELL_TOOLS } from "./policy.ts";

const MAX_DEPTH = 64;

export function inputTextFor(normalizedTool: string, input: unknown): string {
  if (SHELL_TOOLS.has(normalizedTool) && isRecord(input) && typeof input.command === "string") {
    // The command, then the values of every other field except the free-text
    // description, so a working folder is checked too; a folder value is also
    // read with a trailing `/` (integration w1, security finding S7; the hook
    // core does the same).
    const rest: string[] = [];
    for (const [k, v] of Object.entries(input)) if (k !== "command" && k !== "description") collectValues(v, rest, 0);
    for (const v of [...rest]) if (v.includes("/") && !v.endsWith("/")) rest.push(`${v}/`);
    return rest.length ? `${input.command}\n${rest.join("\n")}` : input.command;
  }
  const parts: string[] = [];
  collect(input, parts, 0);
  return parts.join("\n");
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function collect(value: unknown, parts: string[], depth: number): void {
  if (depth > MAX_DEPTH) {
    // Deeper than any real tool input. Throwing makes the gate block the
    // call (it fails closed on any error), rather than check half of it.
    throw new Error("tool input nested too deep to check");
  }
  if (typeof value === "string") parts.push(value);
  else if (typeof value === "number" || typeof value === "boolean") parts.push(String(value));
  else if (Array.isArray(value)) for (const v of value) collect(v, parts, depth + 1);
  else if (isRecord(value)) {
    for (const [k, v] of Object.entries(value)) {
      parts.push(k);
      collect(v, parts, depth + 1);
    }
  }
}

function collectValues(value: unknown, parts: string[], depth: number): void {
  if (depth > MAX_DEPTH) throw new Error("tool input nested too deep to check");
  if (typeof value === "string") parts.push(value);
  else if (typeof value === "number" || typeof value === "boolean") parts.push(String(value));
  else if (Array.isArray(value)) for (const v of value) collectValues(v, parts, depth + 1);
  else if (isRecord(value)) for (const v of Object.values(value)) collectValues(v, parts, depth + 1);
}
