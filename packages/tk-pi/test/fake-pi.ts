// A fake Pi host for tests: records what an extension registers, dispatches
// events the way Pi 1.4.0's runner does for the handlers tk-pi uses, and
// offers a fake RPC client for `confirm`.
//
// The RPC dialog mirrors Pi 1.4.0 dist/modes/rpc/rpc-mode.js:47-85
// (`createDialogPromise(opts, false, { method: "confirm", ... })`): the
// request goes to the client; `{confirmed}` resolves it, `{cancelled: true}`
// resolves false, the timeout or an abort resolves false.

import type {
  DialogOptions,
  EventBus,
  PiApi,
  PiContext,
  ToolInfoLike,
} from "../src/pi-api.ts";

export type Answer = { confirmed: boolean } | { cancelled: true } | "never";

export interface DialogRequest {
  type: "extension_ui_request";
  id: string;
  method: "confirm";
  title: string;
  message: string;
  timeout: number | undefined;
}

export class FakeRpcClient {
  readonly requests: DialogRequest[] = [];
  constructor(private readonly answer: (req: DialogRequest) => Answer) {}

  confirm(title: string, message: string, opts?: DialogOptions): Promise<boolean> {
    if (opts?.signal?.aborted) return Promise.resolve(false);
    const id = `req-${this.requests.length + 1}`;
    return new Promise<boolean>((resolve) => {
      let timeoutId: ReturnType<typeof setTimeout> | undefined;
      const cleanup = () => {
        if (timeoutId) clearTimeout(timeoutId);
        opts?.signal?.removeEventListener("abort", onAbort);
      };
      const onAbort = () => {
        cleanup();
        resolve(false);
      };
      opts?.signal?.addEventListener("abort", onAbort, { once: true });
      if (opts?.timeout) {
        timeoutId = setTimeout(() => {
          cleanup();
          resolve(false);
        }, opts.timeout);
      }
      const req: DialogRequest = { type: "extension_ui_request", id, method: "confirm", title, message, timeout: opts?.timeout };
      this.requests.push(req);
      const a = this.answer(req);
      if (a === "never") return;
      queueMicrotask(() => {
        cleanup();
        resolve("cancelled" in a ? false : a.confirmed);
      });
    });
  }
}

type AnyHandler = (event: unknown, ctx: PiContext) => unknown;

export interface FakePi {
  api: PiApi;
  handlers: Map<string, AnyHandler[]>;
  bus: { channel: string; data: unknown }[];
  touched: string[];
  emit(name: string, event: unknown, ctx?: PiContext): Promise<unknown>;
  ctx(over?: Partial<PiContext>): PiContext;
}

export function fakePi(opts: { tools?: ToolInfoLike[]; client?: FakeRpcClient; hasUI?: boolean; sessionId?: string } = {}): FakePi {
  const handlers = new Map<string, AnyHandler[]>();
  const bus: { channel: string; data: unknown }[] = [];
  const listeners = new Map<string, ((d: unknown) => void)[]>();
  const touched: string[] = [];
  const events: EventBus = {
    emit(channel, data) {
      bus.push({ channel, data });
      for (const l of listeners.get(channel) ?? []) l(data);
    },
    on(channel, handler) {
      const list = listeners.get(channel) ?? [];
      list.push(handler);
      listeners.set(channel, list);
      return () => listeners.set(channel, (listeners.get(channel) ?? []).filter((h) => h !== handler));
    },
  };
  const api = {
    on(name: string, handler: AnyHandler) {
      touched.push(`on:${name}`);
      const list = handlers.get(name) ?? [];
      list.push(handler);
      handlers.set(name, list);
      return () => undefined;
    },
    getAllTools() {
      touched.push("getAllTools");
      return opts.tools ?? [];
    },
    get events() {
      touched.push("events");
      return events;
    },
  } as unknown as PiApi;
  const client = opts.client ?? new FakeRpcClient(() => ({ confirmed: false }));
  const ctx = (over: Partial<PiContext> = {}): PiContext => ({
    ui: {
      confirm: (t, m, o) => client.confirm(t, m, o),
      notify: () => undefined,
    },
    mode: "rpc",
    hasUI: opts.hasUI ?? true,
    cwd: "/work/project",
    sessionManager: { getSessionId: () => opts.sessionId ?? "session-1" },
    signal: undefined,
    ...over,
  });
  return {
    api,
    handlers,
    bus,
    touched,
    ctx,
    // Pi's runner (runner.js:957-972 for tool_call): handlers in order; for
    // tool_call the first block wins. Here each event has at most one handler
    // per extension, so the last return value is the answer.
    async emit(name, event, c) {
      let result: unknown;
      for (const h of handlers.get(name) ?? []) {
        const r = await h(event, c ?? ctx());
        if (r !== undefined) result = r;
      }
      return result;
    },
  };
}

export interface MemFile {
  text: string;
  mode?: number;
  uid?: number;
  mtimeMs?: number;
  isDir?: boolean;
  /** A symbolic link at the path: open with O_NOFOLLOW fails. */
  isLink?: boolean;
}

/** An in-memory FsLike that counts reads. */
export function memFs(files: Record<string, MemFile>) {
  const calls: string[] = [];
  return {
    calls,
    files,
    fs: {
      async open(path: string) {
        calls.push(`open:${path}`);
        const f = files[path];
        if (!f) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" });
        if (f.isLink) throw Object.assign(new Error("ELOOP"), { code: "ELOOP" });
        // The handle holds the file it opened, as a real one does: a later
        // change to `files` does not reach it.
        const held = { ...f };
        return {
          async stat() {
            calls.push(`stat:${path}`);
            return {
              mtimeMs: held.mtimeMs ?? 1,
              size: Buffer.byteLength(held.text),
              mode: held.mode ?? 0o100600,
              uid: held.uid ?? 1000,
              ino: 7,
              isFile: () => !held.isDir,
            };
          },
          async readText() {
            calls.push(`read:${path}`);
            return held.text;
          },
          async close() {},
        };
      },
    },
  };
}
