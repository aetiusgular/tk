# Lane I's contract checks (wave 2)

Scaffolded by integration w1. `run.mjs` imports every `.mjs` file here. Each exports `checks`, an array of `{ id, title, harness, run }`; `run({ binPath, version, codexPath, claudePath })` returns `{ status: "pass" | "fail" | "info" | "skip", finding }`. Pass a CLI with `--cli <harness>=<path>` (for example `--cli gemini=/tmp/cli/gemini/node_modules/.bin/gemini`). Fake model servers are in `../../lib/`: Anthropic Messages (`fakeMessages.mjs`), OpenAI Responses (`fakeResponses.mjs`), Gemini (`fakeGemini.mjs`).
