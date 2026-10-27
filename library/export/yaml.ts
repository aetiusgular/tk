// A small YAML emitter for the Docker profile export (lane E, T3.4).
//
// Docker's import reads the file with gopkg.in/yaml.v3 (pkg/workingset/
// import.go:26). This emitter writes only block mappings, block sequences and
// double-quoted scalars, which every YAML 1.1 and 1.2 reader agrees on, so no
// implicit-typing surprise (a `yes`, a `1.0`, a `0o17`) can change a value.
// Integers and booleans are written bare; everything else is quoted. The
// test round-trips the output through `yaml` 2.9.1.

export type YamlValue = string | number | boolean | null | YamlValue[] | { [k: string]: YamlValue };

function scalar(v: string | number | boolean | null): string {
  if (v === null) return "null";
  if (typeof v === "number") {
    if (!Number.isFinite(v)) throw new Error("non-finite number in YAML export");
    return String(v);
  }
  if (typeof v === "boolean") return v ? "true" : "false";
  return JSON.stringify(v);
}

function isScalar(v: YamlValue): v is string | number | boolean | null {
  return v === null || typeof v !== "object";
}

export function toYaml(value: YamlValue, indent = 0): string {
  const pad = " ".repeat(indent);
  if (isScalar(value)) return pad + scalar(value) + "\n";
  if (Array.isArray(value)) {
    if (value.length === 0) return pad + "[]\n";
    let out = "";
    for (const item of value) {
      if (isScalar(item)) out += `${pad}- ${scalar(item)}\n`;
      else if (Array.isArray(item)) out += `${pad}-\n` + toYaml(item, indent + 2);
      else {
        const body = toYaml(item, indent + 2);
        // Hoist the first key onto the dash line.
        out += `${pad}- ` + body.slice(indent + 2);
      }
    }
    return out;
  }
  const keys = Object.keys(value);
  if (keys.length === 0) return pad + "{}\n";
  let out = "";
  for (const k of keys) {
    const v = value[k] as YamlValue;
    const key = /^[A-Za-z_][A-Za-z0-9_-]*$/.test(k) ? k : JSON.stringify(k);
    if (isScalar(v)) out += `${pad}${key}: ${scalar(v)}\n`;
    else if ((Array.isArray(v) && v.length === 0) || (!Array.isArray(v) && Object.keys(v).length === 0)) out += `${pad}${key}: ${Array.isArray(v) ? "[]" : "{}"}\n`;
    else out += `${pad}${key}:\n` + toYaml(v, indent + 2);
  }
  return out;
}
