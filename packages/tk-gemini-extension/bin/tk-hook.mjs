#!/usr/bin/env node
// tk hook launcher for Gemini CLI (T2.4). hooks/hooks.json runs, through
// Gemini's shell (bash -c on Linux and macOS), with ${extensionPath} filled in:
//   node --no-warnings "<extension>/bin/tk-hook.mjs" before|after
// Plain JavaScript on purpose: Gemini lets a call through when a hook fails
// (any exit but 0 and 2, a signal, a timeout), so this file must turn every
// failure it can see into a block: exit 2 with the reason on stderr and a
// deny on stdout. AfterTool failures exit 0 and change nothing.

const event = process.argv[2] ?? "";

function failClosed(reason) {
  if (event === "before") {
    const text = `tk: ${reason}`;
    process.stdout.write(`${JSON.stringify({ decision: "deny", reason: text })}\n`);
    process.stderr.write(`${text}\n`);
    process.exitCode = 2;
  } else {
    process.exitCode = 0;
  }
}

if (!process.features || !process.features.typescript) {
  failClosed(`this Node (${process.version}) cannot run the tk hook: it needs type stripping (Node 22.18 or later, not under node_modules)`);
} else {
  try {
    const mod = await import(new URL("../src/hook.ts", import.meta.url).href);
    process.exitCode = await mod.main(event);
  } catch (error) {
    failClosed(`hook error: ${error && error.message ? error.message : String(error)}`);
  }
}
