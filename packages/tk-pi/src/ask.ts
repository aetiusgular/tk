// Asking the user about a call (contracts/policy.md, section 4, step 8;
// rules section 4, case 63).
//
// Pi 1.4.0: `ctx.ui.confirm(title, message, { timeout, signal })` resolves
// `false` when the timeout expires or the signal aborts, in RPC mode
// (dist/modes/rpc/rpc-mode.js:47-85, `createDialogPromise(opts, false, ...)`)
// and in the TUI ("Dialog auto-dismisses with live countdown display",
// types.d.ts:43-44). `ctx.hasUI` is false in JSON and print modes
// (types.d.ts:219, docs/extensions.md:363-366). Orca maps `confirm` to an
// approval card (note AP section 7, `rpc-extension-dialogs.ts:25-91`).
//
// tk-pi keeps its own timer as well, so a client that never answers and a
// Pi that never resolves both end in `timeout` (deny), and a timeout is told
// apart from a "No".

import type { PiContext } from "./pi-api.ts";

export type AskOutcome = "yes" | "no" | "timeout" | "no_ui" | "aborted";

export async function askUser(
  ctx: PiContext,
  title: string,
  message: string,
  timeoutMs: number,
): Promise<AskOutcome> {
  if (!ctx.hasUI) return "no_ui";
  if (ctx.signal?.aborted) return "aborted";
  const controller = new AbortController();
  let timedOut = false;
  let aborted = false;
  const onTurnAbort = () => {
    aborted = true;
    controller.abort();
  };
  ctx.signal?.addEventListener("abort", onTurnAbort, { once: true });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const expiry = new Promise<false>((resolve) => {
    timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
      resolve(false);
    }, timeoutMs);
    timer.unref?.();
  });
  let answer: boolean;
  try {
    answer = await Promise.race([
      Promise.resolve(ctx.ui.confirm(title, message, { timeout: timeoutMs, signal: controller.signal })),
      expiry,
    ]);
  } catch {
    answer = false;
  } finally {
    if (timer) clearTimeout(timer);
    ctx.signal?.removeEventListener("abort", onTurnAbort);
  }
  if (timedOut) return "timeout";
  if (aborted) return "aborted";
  return answer === true ? "yes" : "no";
}

// Characters that could spoof the card or the terminal: C0 and C1 controls
// (escape sequences), bidi overrides and isolates, and zero-width marks.
// Newlines and tabs are kept.
const UNSAFE = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f​-‏‪-‮⁠-⁩﻿]/g;

/** Text shown in a card: unsafe characters made visible, cut to `max`. */
export function cardText(text: string, max: number): string {
  const visible = text.replace(UNSAFE, (c) => `\\u{${(c.codePointAt(0) ?? 0).toString(16)}}`);
  return visible.length > max ? `${visible.slice(0, max)}\n… (${visible.length - max} more characters)` : visible;
}
