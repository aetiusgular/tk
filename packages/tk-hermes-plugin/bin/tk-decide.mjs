#!/usr/bin/env node
// Runs one decision for the Hermes plugin (tk_gate.py): JSON on stdin, JSON
// on stdout. Plain JavaScript on purpose: on a Node that cannot load the
// TypeScript it still answers, with a deny, and tk_gate.py blocks on anything
// it cannot read.
if (!process.features || !process.features.typescript) {
  process.stdout.write(`${JSON.stringify({ decision: "deny", reason: `this Node (${process.version}) cannot run the tk hook: it needs type stripping (Node 22.18 or later, not under node_modules)` })}\n`);
} else {
  try {
    const mod = await import(new URL("../src/decide.ts", import.meta.url).href);
    process.exitCode = await mod.main();
  } catch (error) {
    process.stdout.write(`${JSON.stringify({ decision: "deny", reason: `hook error: ${error && error.message ? error.message : String(error)}` })}\n`);
  }
}
