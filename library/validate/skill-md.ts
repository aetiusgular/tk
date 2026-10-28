// SKILL.md frontmatter checks (lane E, T3.1), transcribed from the Agent
// Skills specification (agentskills/agentskills docs/specification.mdx at
// 69ef37e, 9 Aug 2026) and its reference validator
// (skills-ref/src/skills_ref/validator.py 0.1.0, same commit): the six
// allowed fields, name to 64 characters, lowercase letters, digits and
// hyphens, no leading, trailing or double hyphen, folder name equals name,
// description non-empty to 1024, compatibility to 500. The reference parser
// uses strictyaml; here the frontmatter is parsed with `yaml` 2.9.1 in its
// strict core schema, so implicit typing differences are reported as findings
// rather than hidden. `skills-ref validate` itself runs only when it is on
// PATH and TK_LIBRARY_SKILLS_REF=1 (upstream.ts).
import { parse as parseYaml } from "yaml";
import type { Finding } from "./types.ts";
import { finding, isRecord } from "./types.ts";

export const SKILL_FIELDS = new Set(["name", "description", "license", "allowed-tools", "metadata", "compatibility"]);
export const MAX_SKILL_NAME = 64;
export const MAX_SKILL_DESCRIPTION = 1024;
export const MAX_SKILL_COMPATIBILITY = 500;

export type SkillFrontmatter = { fields: Record<string, unknown>; body: string };

/** Split and parse the frontmatter. Returns findings instead of throwing. */
export function parseSkillMd(text: string): { parsed?: SkillFrontmatter; findings: Finding[] } {
  const findings: Finding[] = [];
  if (!text.startsWith("---")) {
    findings.push(finding("skill/frontmatter", "error", "", "SKILL.md must start with YAML frontmatter (---)"));
    return { findings };
  }
  const end = text.indexOf("\n---", 3);
  if (end < 0) {
    findings.push(finding("skill/frontmatter", "error", "", "SKILL.md frontmatter not properly closed with ---"));
    return { findings };
  }
  const yamlText = text.slice(3, end);
  const body = text.slice(end + 4).replace(/^\r?\n/, "");
  let fields: unknown;
  try {
    fields = parseYaml(yamlText, { schema: "core", strict: true, uniqueKeys: true });
  } catch (e) {
    findings.push(finding("skill/frontmatter", "error", "", `frontmatter is not valid YAML: ${(e as Error).message.split("\n")[0]}`));
    return { findings };
  }
  if (!isRecord(fields)) {
    findings.push(finding("skill/frontmatter", "error", "", "frontmatter must be a mapping"));
    return { findings };
  }
  return { parsed: { fields, body }, findings };
}

/** The reference validator's rules on parsed frontmatter. `dirName` is the
 *  folder that holds SKILL.md, when known. */
export function validateSkillFrontmatter(fields: Record<string, unknown>, dirName?: string): Finding[] {
  const out: Finding[] = [];
  const extra = Object.keys(fields).filter((k) => !SKILL_FIELDS.has(k)).sort();
  if (extra.length > 0) {
    out.push(finding("skill/unexpected-field", "error", "", `unexpected fields in frontmatter: ${extra.join(", ")}; only ${[...SKILL_FIELDS].sort().join(", ")} are allowed`));
  }
  const name = fields["name"];
  if (name === undefined) {
    out.push(finding("skill/name", "error", "/name", "missing required field in frontmatter: name"));
  } else if (typeof name !== "string" || name.trim() === "") {
    out.push(finding("skill/name", "error", "/name", "field 'name' must be a non-empty string"));
  } else {
    const n = name.trim().normalize("NFKC");
    if (n.length > MAX_SKILL_NAME) out.push(finding("skill/name", "error", "/name", `skill name exceeds ${MAX_SKILL_NAME} characters`));
    if (n !== n.toLowerCase()) out.push(finding("skill/name", "error", "/name", "skill name must be lowercase"));
    if (n.startsWith("-") || n.endsWith("-")) out.push(finding("skill/name", "error", "/name", "skill name cannot start or end with a hyphen"));
    if (n.includes("--")) out.push(finding("skill/name", "error", "/name", "skill name cannot contain consecutive hyphens"));
    if (!/^[\p{L}\p{N}-]+$/u.test(n)) out.push(finding("skill/name", "error", "/name", "skill name contains invalid characters; only letters, digits and hyphens are allowed"));
    if (dirName !== undefined && dirName.normalize("NFKC") !== n) {
      out.push(finding("skill/dir-name", "error", "/name", `directory name '${dirName}' must match skill name '${n}'`));
    }
  }
  const description = fields["description"];
  if (description === undefined) {
    out.push(finding("skill/description", "error", "/description", "missing required field in frontmatter: description"));
  } else if (typeof description !== "string" || description.trim() === "") {
    out.push(finding("skill/description", "error", "/description", "field 'description' must be a non-empty string"));
  } else if (description.length > MAX_SKILL_DESCRIPTION) {
    out.push(finding("skill/description", "error", "/description", `description exceeds ${MAX_SKILL_DESCRIPTION} characters`));
  }
  const compatibility = fields["compatibility"];
  if (compatibility !== undefined) {
    if (typeof compatibility !== "string") out.push(finding("skill/compatibility", "error", "/compatibility", "field 'compatibility' must be a string"));
    else if (compatibility.length > MAX_SKILL_COMPATIBILITY) out.push(finding("skill/compatibility", "error", "/compatibility", `compatibility exceeds ${MAX_SKILL_COMPATIBILITY} characters`));
  }
  const metadata = fields["metadata"];
  if (metadata !== undefined && !isRecord(metadata)) {
    out.push(finding("skill/metadata", "error", "/metadata", "field 'metadata' must be a mapping"));
  }
  const allowedTools = fields["allowed-tools"];
  if (allowedTools !== undefined && typeof allowedTools !== "string") {
    // The specification says a space-separated string; Pi and skills-ref
    // accept a list without complaint, so this is a warning for portability.
    out.push(finding("skill/allowed-tools", "warning", "/allowed-tools", "the specification defines allowed-tools as a space-separated string"));
  }
  return out;
}

/** allowed-tools as a list of names, whatever the spelling in the file. */
export function allowedToolsOf(fields: Record<string, unknown>): string[] {
  const v = fields["allowed-tools"];
  if (typeof v === "string") return v.split(/\s+/).filter(Boolean);
  if (Array.isArray(v)) return v.filter((x): x is string => typeof x === "string");
  return [];
}
