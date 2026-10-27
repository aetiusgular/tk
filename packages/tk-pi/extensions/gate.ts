// The tool_call gate (lane A, T0.2). STUB: registers nothing. Contract:
// contracts/policy.md. Corner case 46: nothing at load, no file read until
// the first tool_call. Pi loads an extension's default export with the
// extension API (Pi docs/extensions.md at 1.4.0; lane A types it).
export default function gate(_pi: unknown): void {}
