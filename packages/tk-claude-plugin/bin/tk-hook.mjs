#!/usr/bin/env node
// tk hook launcher for Claude Code (T2.1). hooks/hooks.json runs:
//   node --no-warnings "${CLAUDE_PLUGIN_ROOT}/bin/tk-hook.mjs" pre|post
// Plain JavaScript on purpose: it must fail closed even on a Node that cannot
// load the TypeScript hook. A PreToolUse failure exits 2 (block) with the
// reason on stderr; a PostToolUse failure exits 0 and changes nothing.

const event = process.argv[2] ?? "";

function failClosed(reason) {
  if (event === "pre") {
    process.stderr.write(`tk: ${reason}\n`);
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
