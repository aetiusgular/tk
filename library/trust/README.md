# library/trust

Trust records for library entries: toolkit plan T4.1 (scanners), T4.2 (drift and pins), T4.3 (names). Built by wave 2 lane H.

Records, never a verdict (plan R7). Nothing here says an entry is "safe", "secure" or "verified", and `card.test.ts` holds every card line to that. Scanners miss what matters (Trail of Bits beat every scanner it tried, 3 Jun 2026) and flag what is fine (up to 46.8% of skills flagged against 0.52% that stay suspicious, arXiv 2603.16572).

## Commands

Run with Node 22.18 or later (type stripping, no build), from the repository root.

| Command | Does | Network or programs |
|---|---|---|
| `node library/trust/cli.ts names [catalog]` | Lookalike, reserved, mixed-script and punycode name rules on every entry; exit 1 on an error | None |
| `node library/trust/cli.ts record [catalog]` | Records each server version's manifest; exit 1 when a recorded version reappears with other content | None |
| `node library/trust/cli.ts drift <id>` | What changed between the last two recorded versions, as the contract's `capabilityDrift` plus details | None |
| `node library/trust/cli.ts pin <id> --harness pi [--tool <name>]...` | Prints policy rules that pin each approved tool's description. Never writes the policy file | None |
| `node library/trust/cli.ts due [catalog]` | Which scans are due: each version once, then daily | None |
| `node library/trust/cli.ts scan [catalog]` | Runs the due scans with the pinned scanners | Only with `TK_TRUST_SCAN=1`. Runs SkillSpector (its OSV lookup reaches `api.osv.dev`) and Cisco's MCP scanner (YARA, offline) |
| `node library/trust/cli.ts osv [catalog]` | Looks up each package version in OSV | Only with `TK_TRUST_ONLINE=1`. Posts package names and versions to `https://api.osv.dev/v1/querybatch` |
| `node library/trust/cli.ts card <id> [catalog]` | The card's trust lines | None |

`TK_TRUST_RECORDS` points the store elsewhere (tests use a scratch folder).

## Files

| File | What |
|---|---|
| `fold.ts` | The gate's own fold (imported from the hook core, Unicode confusables.txt 17.0.0) and the name skeleton |
| `names.ts` | `checkName`: rules `trust/lookalike`, `trust/reserved`, `trust/reserved-prefix`, `trust/mixed-script`, `trust/punycode-namespace` |
| `identity.ts` | What an entry points at; the brand and reserved loaders |
| `reserved.json` | Reserved names: tk's own, the vendors', Claude Code's reserved marketplace names |
| `brands.directory.json`, `brands.docker.json` | Generated brand seeds from Anthropic's connector directory and Docker's catalog (`gen/gen-brands.mjs`); lane E's seed stays in `library/catalog/brands.json` |
| `manifest.ts` | Per-version manifests, the drift diff, `capabilityDrift`, description pins |
| `records.ts` | The record store under `records/`: immutable versions, SARIF files, no link followed |
| `sarif.ts` | The part of SARIF 2.1.0 tk counts |
| `scanners.ts`, `scanners.json` | The two pinned scanners, run by argument list with a fixed environment |
| `osv.ts` | OSV batch lookups, stored as SARIF |
| `card.ts` | Card lines and the rescan schedule |

## Pins

The gate checks `sha256(description)` on a pinned `allow` and asks when it differs (case 57). Only Pi's gate sees a tool's description; Claude Code's, Codex's and OpenCode's hook input carries none, so `pin` writes rules for `--harness pi` only and refuses the others. Pi trims an MCP tool's description before it shows it, so the pin is of the trimmed text.

## Scanners

| Scanner | Version | Reads | Off switch | Outbound |
|---|---|---|---|---|
| NVIDIA SkillSpector | 2.13.2 (git `abd8293`; not on PyPI) | a skill folder, `--no-llm`, SARIF | `TK_TRUST_SCAN` | `api.osv.dev` (its OSV lookup has no off switch) |
| Cisco AI Defense MCP Scanner | 4.8.7 (PyPI `cisco-ai-mcp-scanner`) | a server's recorded `tools/list`, `--analyzers yara static`, converted to SARIF | `TK_TRUST_SCAN` | None |

A scanner sees no variable of yours but `PATH` and `HOME`, so no key reaches it. A scan of a tool record that holds names only is not run: it would read as a clean result.
