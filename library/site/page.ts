// The library browser as one static HTML file (lane J, T5). Every value is
// escaped here (`esc`); the page's only script filters cards by search,
// kind and set, and touches no entry text: it reads the cards' data-*
// attributes and sets `hidden`. A content security policy pins the script
// and the stylesheet by hash and allows nothing else: no network, no
// frames, no images, no forms (rules section 5, item 6).
import { createHash } from "node:crypto";
import type { Card, Site } from "./model.ts";

/** HTML text and attribute escaping. */
export function esc(s: string): string {
  return s.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}

export const SCRIPT = `
(() => {
  const q = document.getElementById("q");
  const kind = document.getElementById("kind");
  const set = document.getElementById("set");
  const shown = document.getElementById("shown");
  const cards = Array.from(document.querySelectorAll("article.card"));
  function apply() {
    const t = q.value.trim().toLowerCase();
    let n = 0;
    for (const c of cards) {
      const ok = (!t || c.dataset.search.includes(t))
        && (!kind.value || c.dataset.kind === kind.value)
        && (!set.value || (" " + c.dataset.sets + " ").includes(" " + set.value + " "));
      c.hidden = !ok;
      if (ok) n++;
    }
    shown.textContent = n + " of " + cards.length;
  }
  q.addEventListener("input", apply);
  kind.addEventListener("change", apply);
  set.addEventListener("change", apply);
  apply();
})();
`;

export const STYLE = `
:root { color-scheme: light dark; --bg: #fbfaf7; --fg: #1d1d1b; --muted: #66645e; --line: #e3e0d8; --card: #ffffff; --accent: #2f5d50; --warn: #8a4b00; --code: #f2f0ea; }
@media (prefers-color-scheme: dark) { :root { --bg: #141413; --fg: #ecebe6; --muted: #a3a097; --line: #2c2b28; --card: #1c1c1a; --accent: #8fc5b3; --warn: #e3a35a; --code: #23231f; } }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 15px/1.5 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; }
header, main { max-width: 980px; margin: 0 auto; padding: 0 16px; }
header { padding-top: 28px; padding-bottom: 8px; }
h1 { font-size: 22px; margin: 0 0 4px; letter-spacing: -0.01em; }
.sub { color: var(--muted); margin: 0 0 16px; }
.bar { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin-bottom: 18px; }
.bar input, .bar select { font: inherit; padding: 7px 10px; border: 1px solid var(--line); border-radius: 6px; background: var(--card); color: var(--fg); }
.bar input { flex: 1 1 240px; min-width: 0; }
#shown { color: var(--muted); font-size: 13px; }
article.card { background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 16px 18px; margin-bottom: 14px; }
article.card h2 { font-size: 17px; margin: 0; }
.meta { color: var(--muted); font-size: 13px; margin: 2px 0 8px; }
.kind { display: inline-block; font-size: 12px; padding: 1px 7px; border: 1px solid var(--line); border-radius: 999px; margin-left: 6px; color: var(--accent); vertical-align: 2px; }
dl { display: grid; grid-template-columns: max-content 1fr; gap: 3px 14px; margin: 10px 0; font-size: 14px; }
dt { color: var(--muted); }
dd { margin: 0; overflow-wrap: anywhere; }
.trust { border-top: 1px dashed var(--line); margin-top: 10px; padding-top: 8px; }
.trust h3, details summary { font-size: 13px; font-weight: 600; color: var(--muted); margin: 0 0 4px; }
.warn { color: var(--warn); font-size: 13px; margin: 6px 0 0; padding-left: 18px; }
code, pre { font: 13px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; background: var(--code); border-radius: 4px; }
code { padding: 1px 5px; overflow-wrap: anywhere; }
details { margin-top: 10px; }
details table { border-collapse: collapse; width: 100%; font-size: 13px; margin-top: 6px; }
details td { padding: 4px 6px; border-top: 1px solid var(--line); vertical-align: top; }
details td:first-child { white-space: nowrap; color: var(--muted); }
.line { user-select: all; }
a { color: var(--accent); }
footer { max-width: 980px; margin: 24px auto 40px; padding: 0 16px; color: var(--muted); font-size: 13px; }
`;

function sha256b64(s: string): string {
  return createHash("sha256").update(s, "utf8").digest("base64");
}

/** The content security policy for the page's own script and style. */
export function csp(): string {
  return [
    "default-src 'none'",
    `script-src 'sha256-${sha256b64(SCRIPT)}'`,
    `style-src 'sha256-${sha256b64(STYLE)}'`,
    "img-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
    "frame-ancestors 'none'",
  ].join("; ");
}

function card(c: Card): string {
  const row = (label: string, value: string) => (value ? `<dt>${esc(label)}</dt><dd>${esc(value)}</dd>` : "");
  const tools = c.tools.length
    ? `<dt>tools</dt><dd>${c.tools
        .map((t) => `<code>${esc(t.name)}</code>${t.readOnly === true ? " read-only" : t.readOnly === false ? " writes" : " (no hint)"}`)
        .join(", ")}${c.toolsNote ? ` <span class="meta">(${esc(c.toolsNote)})</span>` : ""}</dd>`
    : "";
  const allowed = c.allowedTools.length ? `<dt>uses</dt><dd>${c.allowedTools.map((t) => `<code>${esc(t)}</code>`).join(", ")}</dd>` : "";
  const entry = c.entrySkill ? `<dt>entry skill</dt><dd><a href="#${esc(encodeURIComponent(c.entrySkill))}">${esc(c.entrySkill)}</a></dd>` : "";
  const links = c.links.length
    ? `<dt>links</dt><dd>${c.links.map((l) => `<a href="${esc(l.href)}" rel="noopener noreferrer nofollow">${esc(l.label)}</a>`).join(" · ")}</dd>`
    : "";
  const trust = c.trust.map((t) => row(t.label, t.value)).join("");
  const warnings = c.warnings.length ? `<ul class="warn">${c.warnings.map((w) => `<li>${esc(w)}</li>`).join("")}</ul>` : "";
  const add = c.add.length
    ? `<details><summary>Add with tk connect</summary><p class="meta">${esc(c.addNote)}</p><table>${c.add
        .map((a) => `<tr><td>${esc(a.name)}</td><td><code class="line">${esc(a.line)}</code>${a.note ? ` <span class="meta">${esc(a.note)}</span>` : ""}</td></tr>`)
        .join("")}</table></details>`
    : `<p class="meta">${esc(c.addNote)}</p>`;
  return `<article class="card" id="${esc(encodeURIComponent(c.id))}" data-kind="${esc(c.kind)}" data-sets="${esc(c.sets.join(" "))}" data-search="${esc(c.search)}">
<h2>${esc(c.title)}<span class="kind">${esc(c.kind)}</span></h2>
<p class="meta"><code>${esc(c.id)}</code> · version ${esc(c.version || "not stated")} · tier ${esc(c.tier)} · ${esc(c.status)}${c.sets.length ? ` · in ${esc(c.sets.join(", "))}` : ""}</p>
<p>${esc(c.description)}</p>
<dl>${row("runs", c.runs)}${c.network.length ? row("reaches", c.network.join(", ")) : ""}${row("exposure", c.exposure)}${tools}${allowed}${entry}${links}</dl>
<div class="trust"><h3>Trust records: what was checked and when, not a verdict</h3><dl>${trust}</dl>${warnings}</div>
${add}
</article>`;
}

export function renderPage(site: Site): string {
  const kinds = [...new Set(site.cards.map((c) => c.kind))];
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="${esc(csp())}">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>tk library</title>
<style>${STYLE}</style>
</head>
<body>
<header>
<h1>tk library</h1>
<p class="sub">${site.cards.length} entries. Built ${esc(site.builtAt)} from commit <code>${esc(site.commit.slice(0, 12))}</code>. Records are shown with their dates; none of them says an entry is safe.</p>
<div class="bar">
<input id="q" type="search" placeholder="Search names, tools, descriptions" aria-label="Search">
<select id="kind" aria-label="Kind"><option value="">every kind</option>${kinds.map((k) => `<option value="${esc(k)}">${esc(k)}</option>`).join("")}</select>
<select id="set" aria-label="Set"><option value="">every set</option>${site.sets.map((s) => `<option value="${esc(s.id)}">${esc(s.title)}</option>`).join("")}</select>
<span id="shown"></span>
</div>
</header>
<main>
${site.cards.map(card).join("\n")}
</main>
<footer>Generated by <code>node library/site/build.ts</code> from <code>library/catalog/</code>. The page loads nothing from the network.</footer>
<script>${SCRIPT}</script>
</body>
</html>
`;
}
