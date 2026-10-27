#!/usr/bin/env node
// codex-mock: a fake Responses API model for a third Codex home, so approval
// cards, file edits and commands can be seen in MonoCode without a GPU.
//
//   node tests/cli-contract/codex-mock/serve.mjs [--port 11436]
//
// It listens on 127.0.0.1 only. Each prompt is read as one instruction:
//
//   edit <file>: <old line> => <new line>   one-line change through apply_patch
//   write <file>: <text>                    a new file through apply_patch
//   run <command>                           a shell command through exec_command
//   anything else                           a text reply quoting the prompt
//
// File paths are relative to the session folder, as a model would send them.
// After a tool call returns, the mock replies with the first line of its
// output. It keeps no state and never reads your files itself.

import { startFakeResponses } from "../lib/fakeResponses.mjs";
import { mockReply } from "./mockReply.mjs";

const args = process.argv.slice(2);
const portArg = args.indexOf("--port");
const port = portArg >= 0 ? Number(args[portArg + 1]) : 11436;
if (!Number.isInteger(port) || port < 1024 || port > 65535) {
  console.error("--port must be a number from 1024 to 65535");
  process.exit(2);
}

const server = await startFakeResponses({ script: (body) => mockReply(body), model: "codex-mock", port });
console.log(`codex-mock listening on ${server.baseUrl}`);
process.on("SIGINT", async () => {
  await server.close();
  process.exit(0);
});
