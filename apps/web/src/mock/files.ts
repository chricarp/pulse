// The mock's stand-ins for config files: revisions, project file text and what changed. The
// daemon hashes the typed definition (SHA-256); the mock only needs the same properties: equal
// meaning, equal revision; any change, a different one.

import type { AgentConfig, DefinitionChange, DefinitionKind, RuleConfig, SourceConfig } from "../contracts";

/** JSON with object keys sorted, so key order does not change a revision. */
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
      .filter(([, v]) => v !== undefined)
      .sort(([a], [b]) => (a < b ? -1 : 1));
    return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

/** 64-bit FNV-1a as 16 hex digits. */
export function digest(text: string): string {
  let h = 0xcbf29ce484222325n;
  for (let i = 0; i < text.length; i++) {
    h ^= BigInt(text.charCodeAt(i));
    h = (h * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return h.toString(16).padStart(16, "0");
}

export function revisionOf(def: unknown): string {
  return digest(canonical(def));
}

export function sameDefinition(a: unknown, b: unknown): boolean {
  return canonical(a) === canonical(b);
}

export interface ProjectFile {
  text: string;
  rules: RuleConfig[];
  agents: AgentConfig[];
  sources: SourceConfig[];
}

function tomlValue(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(tomlValue).join(", ")}]`;
  if (v && typeof v === "object") {
    return `{ ${Object.entries(v as Record<string, unknown>)
      .filter(([, x]) => x !== undefined)
      .map(([k, x]) => `${/^[A-Za-z0-9_-]+$/.test(k) ? k : JSON.stringify(k)} = ${tomlValue(x)}`)
      .join(", ")} }`;
  }
  return JSON.stringify(v);
}

/** A project file's text for its definitions (TOML-shaped enough to read). */
export function projectText(file: Omit<ProjectFile, "text">, header = ""): string {
  const tables: string[] = [];
  const add = (kind: string, defs: object[]) => {
    for (const d of defs) {
      const lines = Object.entries(d)
        .filter(([, v]) => v !== undefined && !(Array.isArray(v) && v.length === 0) && !(v && typeof v === "object" && !Array.isArray(v) && Object.keys(v).length === 0))
        .map(([k, v]) => `${k} = ${tomlValue(v)}`);
      tables.push(`[[${kind}]]\n${lines.join("\n")}\n`);
    }
  };
  add("source", file.sources);
  add("agent", file.agents);
  add("rule", file.rules);
  return `${header}${tables.join("\n")}`;
}

export function changes(old: Omit<ProjectFile, "text">, now: Omit<ProjectFile, "text">): DefinitionChange[] {
  const out: DefinitionChange[] = [];
  const compare = (kind: DefinitionKind, a: { name: string }[], b: { name: string }[]) => {
    for (const n of b) {
      const o = a.find((x) => x.name === n.name);
      if (!o) {
        out.push({ kind, name: n.name, change: { kind: "added" } });
        continue;
      }
      const keys = [...new Set([...Object.keys(o), ...Object.keys(n)])].sort();
      const fields = keys.filter((k) => canonical((o as Record<string, unknown>)[k]) !== canonical((n as Record<string, unknown>)[k]));
      if (fields.length) out.push({ kind, name: n.name, change: { kind: "changed", fields } });
    }
    for (const o of a) if (!b.some((x) => x.name === o.name)) out.push({ kind, name: o.name, change: { kind: "removed" } });
  };
  compare("rule", old.rules, now.rules);
  compare("agent", old.agents, now.agents);
  compare("source", old.sources, now.sources);
  return out;
}
