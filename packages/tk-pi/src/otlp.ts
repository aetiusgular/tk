// OTLP JSON log records for Pi events, and a batching sender. Contract:
// contracts/events.md section 2; the receiver that reads them is the fork's
// otel_receiver (crates/tk-daemon/src/otel_receiver/decode.rs:25-141,
// normalize.rs:20-60, 106-170), which lane B extends to strip `pi.`.
//
// Shape (OTLP/JSON, ExportLogsServiceRequest; the protobuf JSON mapping
// writes int64 as a decimal string, which decode.rs:113-121 and 175-182
// accept):
//   resourceLogs[0].resource.attributes: service.name=pi, host.arch, os.type
//   resourceLogs[0].scopeLogs[0].scope: { name, version }
//   logRecords[]: timeUnixNano, observedTimeUnixNano, body pi.<name>,
//                 attributes with the SHORT name in event.name (as Claude
//                 Code 2.1.302 does: tests/fixtures/otlp/claude-2.1.302-logs.json)
//
// The sender never blocks a turn: `enqueue` is synchronous; POSTs run in the
// background, one batch per second, and a failed POST is counted and dropped.
// Only a loopback endpoint is accepted; redirects are refused.

export type AttrValue = string | number | boolean;

export interface OtlpRecord {
  timeUnixNano: string;
  observedTimeUnixNano: string;
  body: { stringValue: string };
  attributes: { key: string; value: Record<string, unknown> }[];
}

export const SCOPE = { name: "io.github.aetiusgular.tk.pi", version: "0.0.0" };

function anyValue(v: AttrValue): Record<string, unknown> {
  if (typeof v === "string") return { stringValue: v };
  if (typeof v === "boolean") return { boolValue: v };
  if (Number.isInteger(v)) return { intValue: String(v) };
  return { doubleValue: v };
}

export function record(shortName: string, attrs: Record<string, AttrValue | undefined>, nowMs: number): OtlpRecord {
  const nanos = `${BigInt(Math.trunc(nowMs)) * 1_000_000n}`;
  const attributes: OtlpRecord["attributes"] = [{ key: "event.name", value: { stringValue: shortName } }];
  for (const [key, v] of Object.entries(attrs)) {
    if (v === undefined) continue;
    if (typeof v === "number" && !Number.isFinite(v)) continue;
    attributes.push({ key, value: anyValue(v) });
  }
  return { timeUnixNano: nanos, observedTimeUnixNano: nanos, body: { stringValue: `pi.${shortName}` }, attributes };
}

export function payload(records: OtlpRecord[], resource: { arch: string; os: string }): unknown {
  return {
    resourceLogs: [
      {
        resource: {
          attributes: [
            { key: "service.name", value: { stringValue: "pi" } },
            { key: "host.arch", value: { stringValue: resource.arch } },
            { key: "os.type", value: { stringValue: resource.os } },
          ],
        },
        scopeLogs: [{ scope: SCOPE, logRecords: records }],
      },
    ],
  };
}

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "[::1]", "::1"]);

/**
 * The URL to POST to, or a reason it is refused. Loopback only
 * (contracts/events.md section 2); no credentials, query or fragment.
 */
export function logsUrl(endpoint: string | undefined): { url: string } | { refused: string } {
  if (endpoint === undefined || endpoint === "") return { refused: "no endpoint" };
  let u: URL;
  try {
    u = new URL(endpoint);
  } catch {
    return { refused: "endpoint is not a URL" };
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return { refused: "endpoint is not http" };
  if (!LOOPBACK_HOSTS.has(u.hostname)) return { refused: "endpoint is not loopback" };
  if (u.username !== "" || u.password !== "") return { refused: "endpoint carries credentials" };
  if (u.search !== "" || u.hash !== "") return { refused: "endpoint carries a query" };
  const path = u.pathname.replace(/\/+$/, "");
  u.pathname = path.endsWith("/v1/logs") ? path : `${path}/v1/logs`;
  return { url: u.toString() };
}

/** A token that is safe to put in a header. The daemon's are 64 hex characters. */
export const TOKEN_PATTERN = /^[A-Za-z0-9._~+/=-]{16,512}$/;

export type FetchLike = (url: string, init: { method: string; headers: Record<string, string>; body: string; redirect: "error"; signal: AbortSignal }) => Promise<{ ok: boolean; status: number }>;

export interface SenderOptions {
  url: string;
  token: string;
  fetch: FetchLike;
  resource: { arch: string; os: string };
  /** Milliseconds between batches. */
  intervalMs?: number;
  maxQueue?: number;
  maxBatch?: number;
  postTimeoutMs?: number;
}

export interface SenderCounts {
  queued: number;
  sent: number;
  failed: number;
  dropped: number;
}

export class Sender {
  private queue: OtlpRecord[] = [];
  private timer: ReturnType<typeof setTimeout> | undefined;
  private inflight: Promise<void> | undefined;
  private closed = false;
  readonly counts: SenderCounts = { queued: 0, sent: 0, failed: 0, dropped: 0 };

  private readonly o: SenderOptions;

  constructor(o: SenderOptions) {
    this.o = o;
  }

  enqueue(r: OtlpRecord): void {
    if (this.closed) {
      this.counts.dropped++;
      return;
    }
    if (this.queue.length >= (this.o.maxQueue ?? 5000)) {
      this.counts.dropped++;
      return;
    }
    this.queue.push(r);
    this.counts.queued++;
    if (!this.timer) {
      this.timer = setTimeout(() => {
        this.timer = undefined;
        void this.flush();
      }, this.o.intervalMs ?? 1000);
      this.timer.unref?.();
    }
  }

  /** Sends what is queued. Never throws. */
  async flush(): Promise<void> {
    if (this.inflight) await this.inflight;
    const batchSize = this.o.maxBatch ?? 500;
    while (this.queue.length > 0) {
      const batch = this.queue.splice(0, batchSize);
      this.inflight = this.post(batch);
      await this.inflight;
      this.inflight = undefined;
    }
  }

  /** Flushes once, waiting at most `ms`, then stops taking records. */
  async close(ms: number): Promise<void> {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
    let t: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([
      this.flush(),
      new Promise<void>((resolve) => {
        t = setTimeout(resolve, ms);
        t.unref?.();
      }),
    ]);
    if (t) clearTimeout(t);
    this.closed = true;
    this.counts.dropped += this.queue.length;
    this.queue = [];
  }

  private async post(batch: OtlpRecord[]): Promise<void> {
    try {
      const res = await this.o.fetch(this.o.url, {
        method: "POST",
        headers: { "content-type": "application/json", "x-tk-token": this.o.token },
        body: JSON.stringify(payload(batch, this.o.resource)),
        redirect: "error",
        signal: AbortSignal.timeout(this.o.postTimeoutMs ?? 5000),
      });
      if (res.ok) this.counts.sent += batch.length;
      else this.counts.failed += batch.length;
    } catch {
      this.counts.failed += batch.length;
    }
  }
}
