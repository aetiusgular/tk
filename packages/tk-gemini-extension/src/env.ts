// Gemini CLI strips secret-named variables from a hook's environment when
// `security.environmentVariableRedaction.enabled` is true, and always under
// GitHub Actions (GITHUB_SHA set): every name matching /TOKEN/i, /KEY/i,
// /AUTH/i and others is dropped, so TK_CHAT_TOKEN and TK_CHAT_TOKEN_FILE do
// not reach the hook. Names starting GEMINI_CLI_ are always kept.
// (gemini-cli v0.66.0, packages/core/src/services/environmentSanitization.ts:
// 13-50, 111-122, 153-155; hookRunner.ts spawns with sanitizeEnvironment.
// Run 28 Oct: with redaction on, TK_CHAT_TOKEN_FILE was gone and
// GEMINI_CLI_TK_CHAT_TOKEN_FILE was there.)
//
// So the hook also reads the path from GEMINI_CLI_TK_CHAT_TOKEN_FILE, only
// when TK_CHAT_TOKEN_FILE is absent. Only the file's path is aliased: a raw
// token under a GEMINI_CLI_ name would also pass Gemini's redaction into
// every shell command the agent runs. The core checks the file the same way
// whichever name gave it (absolute, yours, 0600, outside the session folder,
// never daemon.token).

export const TOKEN_FILE_ALIAS = "GEMINI_CLI_TK_CHAT_TOKEN_FILE";

export function hookEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const out: NodeJS.ProcessEnv = { ...env };
  const direct = env.TK_CHAT_TOKEN_FILE?.trim();
  const alias = env[TOKEN_FILE_ALIAS]?.trim();
  if (!direct && alias) out.TK_CHAT_TOKEN_FILE = alias;
  return out;
}
