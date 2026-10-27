import { describe, expect, it } from "vitest";
import { cardText } from "../src/ask.ts";
import { inputTextFor } from "../src/input-text.ts";
import { REDACTED, SECRET_LIKE, redactDeep, redactText } from "../src/redact.ts";

// Shapes from gitleaks v8.30.1 config/gitleaks.toml (cited in src/redact.ts).
// Every sample below is made up for the test.
const SAMPLES: [string, string][] = [
  ["github", `ghp_${"a1B2c3D4e5".repeat(3)}abcdef`],
  ["github", `gho_${"a1B2c3D4e5".repeat(3)}abcdef`],
  ["github", `ghs_${"a1B2c3D4e5".repeat(3)}abcdef`],
  ["github", `github_pat_${"A1b2_".repeat(16)}xy`],
  ["aws-access-key-id", "AKIAABCDEFGHIJKLMNOP"],
  ["aws-access-key-id", "ASIAQRSTUVWXYZ234567"],
  ["anthropic", `sk-ant-api03-${"x".repeat(40)}`],
  ["openai", `sk-proj-${"Y".repeat(40)}`],
  ["slack", "xoxb-1234567890-1234567890-abcdefABCDEF"],
  ["google-api-key", `AIza${"B".repeat(35)}`],
  ["jwt", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlLXZhbHVl"],
];

describe("redactText", () => {
  it.each(SAMPLES)("redacts a %s token standing alone", (kind, secret) => {
    const r = redactText(`before ${secret} after`);
    expect(r.text).toBe(`before ${REDACTED} after`);
    expect(r.kinds).toContain(kind);
  });

  it("redacts a private key block, and one with no end line", () => {
    const block = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----";
    expect(redactText(`x\n${block}\ny`).text).toBe(`x\n${REDACTED}\ny`);
    expect(redactText("x\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow").text).toBe(`x\n${REDACTED}`);
  });

  it.each([
    ["GITHUB_TOKEN=abc123", `GITHUB_TOKEN=${REDACTED}`],
    ["export API_KEY='s3cr3t value'", `export API_KEY='${REDACTED}'`],
    ['{"password": "hunter2"}', `{"password": "${REDACTED}"}`],
    ["db_password: hunter2", `db_password: ${REDACTED}`],
    ["x-tk-token: abcdef0123", `x-tk-token: ${REDACTED}`],
    ["client_secret=abc&next=1", `client_secret=${REDACTED}&next=1`],
  ])("redacts the value of a secret-like key: %s", (input, out) => {
    expect(redactText(input).text).toBe(out);
  });

  it("redacts a bearer token and keeps the scheme", () => {
    expect(redactText("Authorization: Bearer abcdefghijklmnop.qrs").text).toBe(`Authorization: Bearer ${REDACTED}`);
  });

  it("leaves ordinary text alone", () => {
    for (const t of ["tokens used: 120", "the password policy", "sk-short", "ghp_short", "keyboard: us"]) {
      expect(redactText(t).text, t).toBe(t);
    }
  });

  it("is idempotent", () => {
    const once = redactText("TOKEN=abc ghp_" + "a".repeat(36)).text;
    expect(redactText(once).text).toBe(once);
  });

  it("is fast on a large result (no catastrophic backtracking)", () => {
    const big = "-----BEGIN PRIVATE KEY-----".repeat(2000) + "token=".repeat(20000) + "a".repeat(200000);
    const t0 = performance.now();
    redactText(big);
    expect(performance.now() - t0).toBeLessThan(2000);
  });

  it("SECRET_LIKE is the fork's pattern (dockerArgs.ts:38)", () => {
    expect(SECRET_LIKE.source).toBe("token|secret|password|passwd|apikey|api_key|private_key");
    expect(SECRET_LIKE.flags).toBe("i");
  });
});

describe("redactDeep", () => {
  it("redacts values under secret-like keys and strings everywhere", () => {
    const kinds = new Set<string>();
    expect(redactDeep({ apiKey: "abc", nested: [{ note: "AKIAABCDEFGHIJKLMNOP" }], n: 3, ok: true }, kinds)).toEqual({
      apiKey: REDACTED,
      nested: [{ note: REDACTED }],
      n: 3,
      ok: true,
    });
    expect([...kinds].sort()).toEqual(["aws-access-key-id", "pair"]);
  });

  it("an empty secret value is left as it is", () => {
    expect(redactDeep({ token: "" }, new Set())).toEqual({ token: "" });
  });
});

describe("cardText", () => {
  it("makes escape, bidi and zero-width characters visible and keeps newlines", () => {
    expect(cardText("a\u001b[31mb‮c​d\ne\tf", 100)).toBe("a\\u{1b}[31mb\\u{202e}c\\u{200b}d\ne\tf");
  });

  it("cuts long text and says how much is left", () => {
    expect(cardText("x".repeat(10), 4)).toBe("xxxx\n… (6 more characters)");
  });
});

describe("inputTextFor", () => {
  it("a shell tool gives its command as it is", () => {
    expect(inputTextFor("bash", { command: 'echo "a\\b"', timeout: 5 })).toBe('echo "a\\b"');
    expect(inputTextFor("powershell", { command: "Get-Item $x" })).toBe("Get-Item $x");
  });

  it("another tool gives keys and values, unescaped, one per line", () => {
    expect(inputTextFor("write", { path: "a.txt", content: 'say "hi"\nnext', n: 2, f: false, nested: { k: ["v"] } })).toBe(
      'path\na.txt\ncontent\nsay "hi"\nnext\nn\n2\nf\nfalse\nnested\nk\nv',
    );
  });

  it("a bash call without a command string falls back to keys and values", () => {
    expect(inputTextFor("bash", { cmd: "ls" })).toBe("cmd\nls");
  });
});
