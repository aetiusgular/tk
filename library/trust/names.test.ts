import { readFileSync } from "node:fs";
import { join } from "node:path";
import { domainToASCII } from "node:url";
import { describe, expect, it } from "vitest";
import { fold as coreFold } from "../../packages/tk-claude-plugin/src/core/fold.ts";
import { fold as laneEFold } from "../validate/rejects.ts";
import { loadCatalog } from "../validate/catalog.ts";
import { fold, skeleton } from "./fold.ts";
import { CATALOG_DIR, identityOf, loadBrands, loadReserved, mergeBrands, TRUST_DIR } from "./identity.ts";
import type { Brand, Identity } from "./names.ts";
import { checkName, likeness, mixedWords, ownedBy, punycodeLabels } from "./names.ts";

const brands = loadBrands();
const reserved = loadReserved();
const stranger: Identity = { namespace: "io.github.someone", hosts: ["evil.example"], images: [], packages: [] };
const rules = (fs: { rule: string; severity: string }[]) => fs.map((f) => `${f.rule}:${f.severity}`).sort();

describe("one fold with the gate", () => {
  it("is the hook core's function, not a copy", () => {
    expect(fold).toBe(coreFold);
  });

  // Lane E's hand-picked table (library/validate/rejects.ts). Every entry must
  // still read as its letter in a name skeleton after the move (lane E's open
  // item; plan T4.3). The five where the gate's table differs are pinned.
  const laneESource = readFileSync(join(TRUST_DIR, "..", "validate", "rejects.ts"), "utf8");
  const laneETable = [...laneESource.matchAll(/"\\u([0-9A-Fa-f]{4})": "([a-z])"/g)].map((m) => ({ hex: m[1]!.toUpperCase(), ch: String.fromCharCode(Number.parseInt(m[1]!, 16)), to: m[2]! }));

  it("reads lane E's 56 table entries", () => {
    expect(laneETable).toHaveLength(56);
  });

  it("agrees with lane E's fold on 51 of them and records the 5 it does not", () => {
    const differ = laneETable.filter((e) => coreFold(e.ch) !== e.to).map((e) => `${e.hex}:${e.to}->${coreFold(e.ch)}`);
    // confusables.txt 17.0.0 maps capital iota and Cyrillic dotted I to `l`;
    // kappa, dotless j and eth have only non-ASCII targets there.
    expect(differ).toEqual(["0399:i->l", "03BA:k->κ", "0406:i->l", "0237:j->ȷ", "00F0:d->ð"]);
  });

  it("loses none of lane E's entries in a name skeleton", () => {
    for (const e of laneETable) {
      // `l` and `i` are themselves look-alikes, so a name with either is caught
      // whichever the table picks; the skeleton must land on ASCII.
      expect(skeleton(e.ch), e.hex).toMatch(/^[a-z]$/);
      if (!["0399", "0406"].includes(e.hex)) expect(skeleton(e.ch), e.hex).toBe(e.to);
    }
  });

  it("catches what lane E's subset missed", () => {
    expect(laneEFold("ΕΧΑ")).not.toBe("exa"); // lane E lowercases first and has no ε or χ
    expect(skeleton("ΕΧΑ")).toBe("exa");
    expect(skeleton("éxa")).toBe("exa");
    expect(skeleton("ᎬXA")).toBe("exa"); // Cherokee
    expect(skeleton("𝐞𝐱𝐚")).toBe("exa");
    expect(skeleton("e​xa")).toBe("exa");
    expect(skeleton("Exa_MCP")).toBe("examcp");
    expect(skeleton("gith0b")).toBe("githob");
    expect(skeleton("rnotion")).toBe("motion");
  });
});

describe("the seed catalog passes its own name rules", () => {
  const catalog = loadCatalog(CATALOG_DIR);
  it.each(catalog.entries.map((e) => [e.id, e] as const))("%s", (_id, entry) => {
    const title = typeof entry.data["title"] === "string" ? entry.data["title"] : undefined;
    const description = typeof entry.data["description"] === "string" ? entry.data["description"] : undefined;
    const findings = checkName({ id: entry.id, ...(title !== undefined ? { title } : {}), ...(description !== undefined ? { description } : {}), identity: identityOf(entry) }, { brands, reserved });
    expect(findings.filter((f) => f.severity === "error")).toEqual([]);
  });

  it("warns that docker-mcp-gateway is not Docker's own entry", () => {
    const entry = catalog.entries.find((e) => e.id === "docker-mcp-gateway")!;
    const f = checkName({ id: entry.id, identity: identityOf(entry) }, { brands, reserved: [{ name: "docker", namespaces: ["io.github.docker"], reason: "Docker's name", prefix: true }] });
    expect(rules(f)).toEqual(["trust/reserved-prefix:warning"]);
  });
});

describe("lookalikes (plan T4.3; UpGuard, note AP section 5)", () => {
  it("refuses a brand's name from a stranger pointing elsewhere", () => {
    expect(rules(checkName({ id: "exa", identity: stranger }, { brands, reserved }))).toEqual(["trust/lookalike:error"]);
  });

  it("refuses squat affixes and one-letter edits of long brands", () => {
    for (const id of ["exa-mcp", "official-exa", "exa-official", "vercel2", "githuh", "stripe-server", "hubsp0t"]) {
      expect(rules(checkName({ id, identity: stranger }, { brands, reserved })), id).toContain("trust/lookalike:error");
    }
  });

  it("leaves names that only contain a brand", () => {
    for (const id of ["exam-prep", "context-window", "vercel-blog-reader-for-teams"]) {
      expect(checkName({ id, identity: stranger }, { brands, reserved }).filter((f) => f.rule === "trust/lookalike"), id).toEqual([]);
    }
  });

  it("passes a pointer at the brand's own endpoint, whoever lists it (case 59)", () => {
    const pointer: Identity = { namespace: "io.github.someone", hosts: ["mcp.exa.ai"], images: [], packages: [] };
    expect(checkName({ id: "exa", title: "Exa", identity: pointer }, { brands, reserved })).toEqual([]);
  });

  it("refuses a pointer that mixes the brand's host with another", () => {
    const mixed: Identity = { namespace: "io.github.someone", hosts: ["mcp.exa.ai", "collect.example"], images: [], packages: [] };
    expect(rules(checkName({ id: "exa", identity: mixed }, { brands, reserved }))).toEqual(["trust/lookalike:error"]);
  });

  it("refuses a look-alike title even under an unrelated id", () => {
    const f = checkName({ id: "web-search", title: "Еxa", identity: stranger }, { brands, reserved });
    expect(rules(f)).toEqual(["trust/lookalike:error", "trust/mixed-script:error"]);
    expect(f.find((x) => x.rule === "trust/lookalike")?.path).toBe("/title");
  });

  it("refuses a title written wholly in Greek capitals that reads as the brand", () => {
    expect(rules(checkName({ id: "web-search", title: "ΕΧΑ", identity: stranger }, { brands, reserved }))).toEqual(["trust/lookalike:error"]);
  });

  it("does not let a package prefix own a longer scope", () => {
    const shopify: Brand = { name: "shopify", namespaces: [], packages: ["@shopify/"] };
    expect(ownedBy({ namespace: "x", hosts: [], images: [], packages: ["@shopify/dev-mcp"] }, shopify)).toBe(true);
    expect(ownedBy({ namespace: "x", hosts: [], images: [], packages: ["@shopifyx/dev-mcp"] }, shopify)).toBe(false);
  });

  it("does not let a host suffix match without a dot boundary", () => {
    const exa: Brand = { name: "exa", namespaces: [], hosts: ["exa.ai"] };
    expect(ownedBy({ namespace: "x", hosts: ["mcp.exa.ai"], images: [], packages: [] }, exa)).toBe(true);
    expect(ownedBy({ namespace: "x", hosts: ["notexa.ai"], images: [], packages: [] }, exa)).toBe(false);
  });

  it("compares short brands by equality only", () => {
    expect(likeness("tq", "tk")).toBeUndefined();
    expect(likeness("tk", "tk")?.kind).toBe("equal");
  });
});

describe("reserved names", () => {
  it("refuses a vendor's product name and its official forms", () => {
    for (const id of ["claude", "claude-official", "codex", "openai-mcp", "official", "gh"]) {
      expect(rules(checkName({ id, identity: stranger }, { brands: [], reserved })), id).toContain("trust/reserved:error");
    }
  });

  it("warns on a vendor mark at the start of an id, not on generic words", () => {
    expect(rules(checkName({ id: "claude-memory-index", identity: stranger }, { brands: [], reserved }))).toEqual(["trust/reserved-prefix:warning"]);
    expect(checkName({ id: "community-calendar", identity: stranger }, { brands: [], reserved })).toEqual([]);
  });

  it("lets the owner use its own name", () => {
    const anthropic: Identity = { namespace: "io.github.anthropics", hosts: [], images: [], packages: [] };
    expect(checkName({ id: "claude-code", identity: anthropic }, { brands: [], reserved })).toEqual([]);
    const tony: Identity = { namespace: "io.github.aetiusgular", hosts: [], images: [], packages: [] };
    expect(checkName({ id: "tk-computer-use", identity: tony }, { brands: [], reserved })).toEqual([]);
  });
});

describe("mixed scripts and punycode (UTS #39 section 5.1)", () => {
  it("flags a Latin word with a Cyrillic letter in it", () => {
    expect(mixedWords("Search the web with Еxa")).toEqual(["Еxa"]);
    expect(mixedWords("pаypal login")).toEqual(["pаypal"]);
  });

  it("leaves accented Latin and whole words in other scripts", () => {
    expect(mixedWords("Café menu, naïve résumé")).toEqual([]);
    expect(mixedWords("Αναζήτηση στο διαδίκτυο")).toEqual([]);
    expect(mixedWords("検索ツール")).toEqual([]);
  });

  it("warns in a description and refuses in a title", () => {
    const f = checkName({ id: "web-search", title: "Web search", description: "Search with Еxa", identity: stranger }, { brands: [], reserved: [] });
    expect(f).toEqual([expect.objectContaining({ rule: "trust/mixed-script", severity: "warning", path: "/description" })]);
  });

  it("refuses a punycode namespace that decodes to a brand", () => {
    const label = domainToASCII("еxa.com").split(".")[0]!; // Cyrillic е
    expect(label.startsWith("xn--")).toBe(true);
    const ns = `com.${label}`;
    expect(punycodeLabels(ns)).toEqual([{ label, unicode: "еxa" }]);
    const f = checkName({ id: "search", identity: { namespace: ns, hosts: [], images: [], packages: [] } }, { brands, reserved });
    expect(rules(f)).toEqual(["trust/punycode-namespace:error"]);
  });

  it("shows any other punycode namespace", () => {
    const label = domainToASCII("bücher.de").split(".")[0]!;
    const f = checkName({ id: "books", identity: { namespace: `de.${label}`, hosts: [], images: [], packages: [] } }, { brands, reserved });
    expect(rules(f)).toEqual(["trust/punycode-namespace:warning"]);
  });
});

describe("brand lists", () => {
  it("merges owners of one name from two sources", () => {
    const merged = mergeBrands([{ name: "Exa", namespaces: ["a"], hosts: ["mcp.exa.ai"], source: "one" }], [{ name: "exa", namespaces: ["b"], images: ["mcp/exa"], source: "two" }]);
    expect(merged).toEqual([{ name: "Exa", namespaces: ["a", "b"], hosts: ["mcp.exa.ai"], images: ["mcp/exa"], packages: [], source: "one; two" }]);
  });

  it("holds lane E's seed brands and the reserved list", () => {
    expect(brands.map((b) => b.name)).toEqual(expect.arrayContaining(["exa", "context7", "hubspot", "tk"]));
    for (const r of reserved) {
      expect(r.name).toMatch(/^[a-z0-9-]+$/);
      expect(Array.isArray(r.namespaces)).toBe(true);
      expect(r.reason.length).toBeGreaterThan(0);
    }
  });
});
