// Reads the policy file lazily and keeps it until it changes
// (contracts/policy.md, section 1; rules section 4, case 46: no file read at
// load or at session start, only on the first tool call).
//
// Refusals, each one leaving the default policy (every call `ask`) and a
// reason: a relative TK_POLICY (it would resolve inside the project folder,
// which an agent can write: rules section 5, item 4); anything but a regular
// file; a file larger than 1 MiB; on POSIX, a file another user owns or that
// group or others can write.

import { promises as fsp } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import { defaultPolicy, parsePolicy, type LoadedPolicy } from "./policy.ts";

export interface StatLike {
  mtimeMs: number;
  size: number;
  mode: number;
  uid: number;
  ino: number;
  isFile(): boolean;
}

export interface FsLike {
  stat(path: string): Promise<StatLike>;
  readFile(path: string): Promise<string>;
}

export const realFs: FsLike = {
  stat: (p) => fsp.stat(p),
  readFile: (p) => fsp.readFile(p, "utf8"),
};

export const MAX_POLICY_BYTES = 1024 * 1024;

export function policyPath(env: Record<string, string | undefined>, home: () => string = homedir): string | { problem: string } {
  const fromEnv = env.TK_POLICY;
  if (fromEnv !== undefined && fromEnv !== "") {
    if (!isAbsolute(fromEnv)) return { problem: "TK_POLICY must be an absolute path" };
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
    let st: StatLike;
    try {
      st = await this.fs.stat(this.path);
    } catch (err) {
      const code = (err as { code?: string }).code;
      this.cached = undefined;
      return { policy: defaultPolicy(), problem: code === "ENOENT" ? "no policy file" : "policy file unreadable" };
    }
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
      text = await this.fs.readFile(this.path);
    } catch {
      return { policy: defaultPolicy(), problem: "policy file unreadable" };
    }
    const loaded = parsePolicy(text);
    this.cached = { key, loaded };
    return loaded;
  }
}
