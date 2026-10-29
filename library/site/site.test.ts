import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { CATALOG_DIR, loadCatalog } from "../validate/index.ts";
import { build } from "./build.ts";
import { addLines, buildSite, loadHarnesses, safeHref, shellQuote } from "./model.ts";
import { csp, esc, renderPage } from "./page.ts";

const dirs: string[] = [];
function tmp(): string {
  const d = mkdtempSync(join(tmpdir(), "tk-site-"));
  dirs.push(d);
  return d;
}
afterEach(() => {
  while (dirs.length) rmSync(dirs.pop()!, { recursive: true, force: true });
});

function hashOf(s: string): string {
  return createHash("sha256").update(s, "utf8").digest("base64");
}

describe("the library browser", () => {
  it("has a card for every catalog entry and a tk connect line per harness for a remote server", () => {
    const html = readFileSync(build(tmp(), CATALOG_DIR, "a".repeat(40)), "utf8");
    const catalog = loadCatalog();
    for (const e of catalog.entries) expect(html).toContain(`id="${encodeURIComponent(e.id)}"`);
    expect(html).toContain("tk connect claude-code --from library/catalog/exa/server.json");
    expect(html).toContain("tk connect pi --from library/catalog/exa/server.json");
    // Print-only harnesses say so.
    expect(html).toMatch(/tk connect hermes --from library\/catalog\/exa\/server\.json<\/code> <span class="meta">prints the entry/);
    // Skills install through an export; the gateway by profile.
    expect(html).toContain("installs through an export");
    expect(html).toContain("tk connect codex --gateway docker --profile &lt;profile&gt;");
  });

  it("states trust as dated records and placeholders, never as safe", () => {
    const html = readFileSync(build(tmp(), CATALOG_DIR, "b".repeat(40)), "utf8");
    expect(html).toContain("placeholder, not computed yet");
    expect(html).toContain("none recorded");
    expect(html).toContain("never checked");
    expect(html.toLowerCase()).not.toMatch(/>\s*(safe|verified|trusted)\s*</);
  });

  it("pins its one script and its stylesheet by hash and allows nothing else", () => {
    const html = readFileSync(build(tmp(), CATALOG_DIR, "c".repeat(40)), "utf8");
    const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);
    const styles = [...html.matchAll(/<style>([\s\S]*?)<\/style>/g)].map((m) => m[1]);
    expect(scripts).toHaveLength(1);
    expect(styles).toHaveLength(1);
    expect((html.match(/<script/g) ?? []).length).toBe(1);
    const policy = csp();
    expect(policy).toContain(`'sha256-${hashOf(scripts[0] ?? "")}'`);
    expect(policy).toContain(`'sha256-${hashOf(styles[0] ?? "")}'`);
    expect(policy).toMatch(/^default-src 'none'/);
    // The policy is a <meta>, where frame-ancestors is not supported (CSP3
    // section 3.3) and Chrome logs an error on every load (Mac test w2, MW7).
    expect(policy).not.toContain("frame-ancestors");
    expect(html).toContain(esc(policy));
    // No inline handler and no remote resource anywhere.
    expect(html).not.toMatch(/\son[a-z]+=/i);
    expect(html).not.toMatch(/<(img|iframe|link|object|embed)\b/i);
  });

  it("escapes a stranger's entry and drops links that are not https", () => {
    // The validator would refuse most of this; the page must hold even so,
    // so the entry goes to the renderer directly.
    const exa = loadCatalog().entries.find((e) => e.id === "exa")!;
    const data = structuredClone(exa.data) as Record<string, any>;
    data.description = 'Search <script>alert(1)</script><img src=x onerror="alert(2)"> "quoted" & more';
    data.title = "Exa</h2><a href='https://evil.example'>x</a>";
    data.websiteUrl = "javascript:alert(3)";
    data.repository = { url: "https://user:pw@github.com/x/y", source: "github" };
    const entry = { ...exa, id: 'ex"a<b>', data };
    const catalog = { dir: "x", entries: [entry], brands: [], sets: { version: 1 as const, sets: { s: { title: "S</option><script>x()</script>", description: "d", entries: [entry.id] } } } };
    const html = renderPage(buildSite(catalog, new Map([[entry.id, [{ rule: "lib/x", severity: "warning" as const, path: "", message: "<b>bold</b>" }]]]), loadHarnesses(), "e".repeat(40)));
    expect(html).not.toContain("<script>alert");
    expect(html).not.toContain("<img");
    expect(html).not.toContain("javascript:");
    expect(html).not.toContain("user:pw@");
    expect(html).not.toContain("<a href='https://evil.example'>");
    expect(html).not.toContain("<script>x()");
    expect(html).not.toContain("<b>");
    expect(html).toContain("&lt;script&gt;alert(1)&lt;/script&gt;");
    expect(html).toContain("&quot;quoted&quot; &amp; more");
    expect(html).toContain('id="ex%22a%3Cb%3E"');
    expect((html.match(/<script/g) ?? []).length).toBe(1);
  });

  it("keeps only plain https links", () => {
    expect(safeHref("https://docs.exa.ai")).toBe("https://docs.exa.ai/");
    for (const bad of ["http://x.example", "javascript:alert(1)", "data:text/html,x", "//x.example", "https://a:b@x.example", "file:///etc/passwd", "", 42, "https://" + "a".repeat(3000)]) {
      expect(safeHref(bad)).toBeUndefined();
    }
  });

  it("quotes a path with shell characters in a printed line", () => {
    expect(shellQuote("library/catalog/exa/server.json")).toBe("library/catalog/exa/server.json");
    expect(shellQuote("a b")).toBe("'a b'");
    expect(shellQuote("x';rm -rf ~;'")).toBe(`'x'\\'';rm -rf ~;'\\'''`);
    expect(shellQuote("$(id)")).toBe("'$(id)'");
  });

  it("lists every harness in the preset table, print-only ones marked", () => {
    const hs = loadHarnesses();
    expect(hs.map((h) => h.id)).toContain("claude-code");
    expect(hs.find((h) => h.id === "hermes")?.write).toBe("print");
    const exa = loadCatalog().entries.find((e) => e.id === "exa")!;
    const { lines } = addLines(exa, hs);
    expect(lines).toHaveLength(hs.length);
  });
});
