// Reads the policy file lazily and keeps it until it changes
// (contracts/policy.md, section 1; rules section 4, case 46: no file read at
// load or at session start, only on the first tool call).
//
// Refusals, each one leaving the default policy (every call `ask`) and a
// reason: a relative TK_POLICY, or one inside the session folder (it would be
// the project's file, which an agent can write: rules section 5, item 4; the
// hook core refuses the same, integration w1); a symbolic link; anything but
// a regular file; a file larger than 1 MiB; on POSIX, a file another user owns
// or that group or others can write.
//
// The file is opened once per check (O_NOFOLLOW, O_NONBLOCK so a FIFO cannot
// hang the turn) and every check and the read use that one handle, so the
// file that was checked is the file that is read (integration w1 security
// finding S11: a stat by path, then a read by path, left a window in which
// the path could be swapped).

import { constants, promises as fsp, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { defaultPolicy, parsePolicy, type LoadedPolicy } from "./policy.ts";

export interface StatLike {
  mtimeMs: number;
  size: number;
  mode: number;
  uid: number;
  ino: number;
  isFile(): boolean;
}

/** One open file: every check and the read go through it. */
export interface HandleLike {
  stat(): Promise<StatLike>;
  readText(): Promise<string>;
  close(): Promise<void>;
}

export interface FsLike {
  /** Opens without following a link at the last component; rejects with code ELOOP (or EMLINK) on a link. */
  open(path: string): Promise<HandleLike>;
}

const OPEN_FLAGS = constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0);

export const realFs: FsLike = {
  async open(p) {
    const h = await fsp.open(p, OPEN_FLAGS);
    return {
      stat: () => h.stat(),
      readText: () => h.readFile({ encoding: "utf8" }),
      close: () => h.close(),
    };
  },
};

/** The real path of `p`, following links on every part that exists; a missing tail is kept as written. */
function realish(p: string): string {
  let head = resolve(p);
  const tail: string[] = [];
  for (;;) {
    try {
      return join(realpathSync(head), ...tail.reverse());
    } catch {
      const parent = dirname(head);
      if (parent === head) return resolve(p);
      tail.push(head.slice(parent.length).replace(/^[\\/]/, ""));
      head = parent;
    }
  }
}

/** True when `file` is `folder` or lies under it, after links are followed. */
export function insideFolder(file: string, folder: string): boolean {
  const rel = relative(realish(folder), realish(file));
  return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

export const MAX_POLICY_BYTES = 1024 * 1024;

export function policyPath(
  env: Record<string, string | undefined>,
  home: () => string = homedir,
  cwd: () => string = () => process.cwd(),
): string | { problem: string } {
  const fromEnv = env.TK_POLICY;
  if (fromEnv !== undefined && fromEnv !== "") {
    if (!isAbsolute(fromEnv)) return { problem: "TK_POLICY must be an absolute path" };
    if (insideFolder(fromEnv, cwd())) return { problem: "TK_POLICY is inside the session folder, which a project can set; not used" };
    return fromEnv;
  }
  return join(home(), ".tk", "policy.json");
}

export class PolicyStore {
  private cached: { key: string; loaded: LoadedPolicy } | undefined;

  private readonly path: string | { problem: string };
  private readonly fs: FsLike;
  private readonly uid: number | undefined;
  private readonly posix: boolean;

  constructor(
    path: string | { problem: string },
    fs: FsLike = realFs,
    uid: number | undefined = typeof process.getuid === "function" ? process.getuid() : undefined,
    posix: boolean = process.platform !== "win32",
  ) {
    this.path = path;
    this.fs = fs;
    this.uid = uid;
    this.posix = posix;
  }

  async get(): Promise<LoadedPolicy> {
    if (typeof this.path !== "string") return { policy: defaultPolicy(), problem: this.path.problem };
    let handle: HandleLike;
    try {
      handle = await this.fs.open(this.path);
    } catch (err) {
      const code = (err as { code?: string }).code;
      this.cached = undefined;
      if (code === "ENOENT") return { policy: defaultPolicy(), problem: "no policy file" };
      if (code === "ELOOP" || code === "EMLINK") return { policy: defaultPolicy(), problem: "policy file is a symbolic link" };
      return { policy: defaultPolicy(), problem: "policy file unreadable" };
    }
    try {
      const st = await handle.stat();
      if (!st.isFile()) return { policy: defaultPolicy(), problem: "policy path is not a file" };
      if (st.size > MAX_POLICY_BYTES) return { policy: defaultPolicy(), problem: "policy file larger than 1 MiB" };
      if (this.posix) {
        if ((st.mode & 0o022) !== 0) return { policy: defaultPolicy(), problem: "policy file is writable by group or others" };
        if (this.uid !== undefined && st.uid !== this.uid) return { policy: defaultPolicy(), problem: "policy file is owned by another user" };
      }
      const key = `${st.ino}:${st.mtimeMs}:${st.size}`;
      if (this.cached?.key === key) return this.cached.loaded;
      let text: string;
      try {
        text = await handle.readText();
      } catch {
        return { policy: defaultPolicy(), problem: "policy file unreadable" };
      }
      // The size was checked on this handle; a file that grew since is read
      // up to its new end, so check the text too.
      if (Buffer.byteLength(text, "utf8") > MAX_POLICY_BYTES) return { policy: defaultPolicy(), problem: "policy file larger than 1 MiB" };
      const loaded = parsePolicy(text);
      this.cached = { key, loaded };
      return loaded;
    } catch {
      return { policy: defaultPolicy(), problem: "policy file unreadable" };
    } finally {
      await handle.close().catch(() => undefined);
    }
  }
}
