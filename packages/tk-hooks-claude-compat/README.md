# tk-hooks-claude-compat

Scaffolded by integration w2 for wave 4, lane L (`plans/parallel_build_tk.md`, section 7a). Command hooks with Claude-format replies for OpenClaude, Qwen Code, Droid, Command Code, Kimi Code, Auggie, Copilot CLI and Grok Build: lane F's gate core plus one dialect per agent (toolkit plan T2.5).

Nothing here runs yet. `src/core/` is the canonical hook core, copied by `packages/tk-claude-plugin/scripts/sync-core.mjs`; never edit it here. The agents' contracts and fixtures are in `contracts/hook-dialects.md` and `contracts/fixtures/hook-dialects/`. Lane L owns this folder (except `src/core/`) and replaces this README.
