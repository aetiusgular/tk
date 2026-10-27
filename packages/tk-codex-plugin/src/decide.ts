// Codex plugin: PreToolUse hook and a requirements.toml fragment (lane F, T2.2). STUB. Contract: contracts/policy.md. Until lane F replaces it, the
// stub answers "deny" so that nothing installed by mistake allows a call.
export type Decision = { action: "allow" | "ask" | "deny"; reason: string };

export function decide(_input: unknown): Decision {
  return { action: "deny", reason: "tk-codex-plugin not implemented (session 0 stub; lane F)" };
}
