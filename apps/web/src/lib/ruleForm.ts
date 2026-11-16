// The rule editor's form state and its mapping to and from `RuleConfig`.
//
// The form is flat text so every field can be typed freely; `toRule` turns it back into a
// `RuleConfig` with a list of field problems. The `where` table becomes rows of
// (key, operator, value); several rows with the same key make one operator table.

import type {
  ConfigOrigin,
  DebounceBy,
  RuleConfig,
  RuleInfo,
  WhereCondition,
  WhereOps,
  WhereScalar,
} from "../contracts";
import { nameProblem, parseDuration, parseRate, templateProblems, typeGlobProblem } from "./syntax";

export const WHERE_OPS = [
  "match",
  "any",
  "eq",
  "ne",
  "gt",
  "gte",
  "lt",
  "lte",
  "in",
  "contains",
  "glob",
  "exists",
  "missing",
] as const;
export type WhereOp = (typeof WHERE_OPS)[number];

export const WHERE_OP_LABEL: Record<WhereOp, string> = {
  match: "matches",
  any: "is any of",
  eq: "equals",
  ne: "is not",
  gt: ">",
  gte: "≥",
  lt: "<",
  lte: "≤",
  in: "is in",
  contains: "contains",
  glob: "glob",
  exists: "exists",
  missing: "is missing",
};

/** Operators whose value is a list of scalars. */
export const LIST_OPS: ReadonlySet<WhereOp> = new Set(["any", "in"]);
/** Operators that take no value. */
export const NO_VALUE_OPS: ReadonlySet<WhereOp> = new Set(["exists", "missing"]);
const NUMERIC_OPS: ReadonlySet<WhereOp> = new Set(["gt", "gte", "lt", "lte"]);

export interface WhereRow {
  key: string;
  op: WhereOp;
  /** A scalar as typed: `"1"` (quoted) is a string, `1` a number, `true` a boolean. */
  value: string;
  /** For list operators. */
  values: string[];
}

export interface RuleForm {
  /** Bare name, without the project prefix. */
  name: string;
  /** The trusted project whose file owns the rule; `null` is the user's `pulse.toml`. */
  project: string | null;
  description: string;
  on: string[];
  where: WhereRow[];
  debounce: string;
  debounceBy: DebounceBy;
  cooldown: string;
  rateLimit: string;
  supervised: boolean;
  enabled: boolean;
  archived: boolean;
  agent: string;
  prompt: string;
}

export interface FieldProblem {
  /** `name`, `on`, `where[2].value`, `run.prompt`… */
  field: string;
  message: string;
}

export function emptyRuleForm(): RuleForm {
  return {
    name: "",
    project: null,
    description: "",
    on: [],
    where: [],
    debounce: "",
    debounceBy: "rule",
    cooldown: "",
    rateLimit: "",
    supervised: false,
    enabled: true,
    archived: false,
    agent: "",
    prompt: "",
  };
}

// ---- scalars ----

export function formatScalar(v: WhereScalar): string {
  if (typeof v !== "string") return String(v);
  // Quote strings that would otherwise read back as a number or a boolean.
  return parseScalar(v) === v ? v : JSON.stringify(v);
}

export function parseScalar(text: string): WhereScalar {
  const t = text.trim();
  if (t.length >= 2 && t.startsWith('"') && t.endsWith('"')) {
    try {
      const s: unknown = JSON.parse(t);
      if (typeof s === "string") return s;
    } catch {
      return t.slice(1, -1);
    }
  }
  if (t === "true") return true;
  if (t === "false") return false;
  if (/^-?\d+(\.\d+)?([eE][-+]?\d+)?$/.test(t)) return Number(t);
  return text;
}

export function scalarKind(text: string): "string" | "number" | "boolean" {
  const v = parseScalar(text);
  return typeof v === "number" ? "number" : typeof v === "boolean" ? "boolean" : "string";
}

// ---- where ----

const OP_ORDER: (keyof WhereOps)[] = ["eq", "ne", "gt", "gte", "lt", "lte", "in", "contains", "glob", "exists"];

function isOps(c: WhereCondition): c is WhereOps {
  return typeof c === "object" && c !== null && !Array.isArray(c);
}

export function whereToRows(where: { [key: string]: WhereCondition } | undefined): WhereRow[] {
  const rows: WhereRow[] = [];
  for (const [key, cond] of Object.entries(where ?? {})) {
    if (Array.isArray(cond)) {
      rows.push({ key, op: "any", value: "", values: cond.map(formatScalar) });
    } else if (isOps(cond)) {
      for (const op of OP_ORDER) {
        const v = cond[op];
        if (v === undefined || v === null) continue;
        if (op === "in") {
          rows.push({ key, op: "in", value: "", values: (v as WhereScalar[]).map(formatScalar) });
        } else if (op === "exists") {
          rows.push({ key, op: v ? "exists" : "missing", value: "", values: [] });
        } else if (op === "glob") {
          rows.push({ key, op: "glob", value: String(v), values: [] });
        } else {
          rows.push({ key, op, value: formatScalar(v as WhereScalar), values: [] });
        }
      }
    } else {
      rows.push({ key, op: "match", value: formatScalar(cond), values: [] });
    }
  }
  return rows;
}

export function rowsToWhere(rows: readonly WhereRow[]): {
  where: { [key: string]: WhereCondition };
  problems: FieldProblem[];
} {
  const problems: FieldProblem[] = [];
  const byKey = new Map<string, { row: WhereRow; index: number }[]>();
  rows.forEach((row, index) => {
    const key = row.key.trim();
    if (!key) {
      problems.push({ field: `where[${index}].key`, message: "Name the field (path, subject, data.repo…)." });
      return;
    }
    if (!/^[A-Za-z0-9_-]+(\.[A-Za-z0-9_-]+)*$/.test(key)) {
      problems.push({ field: `where[${index}].key`, message: `"${key}" is not a field path.` });
      return;
    }
    const list = byKey.get(key) ?? [];
    list.push({ row, index });
    byKey.set(key, list);
  });

  const where: { [key: string]: WhereCondition } = {};
  for (const [key, entries] of byKey) {
    const first = entries[0];
    if (!first) continue;
    if (entries.length === 1 && (first.row.op === "match" || first.row.op === "any")) {
      const { row, index } = first;
      if (row.op === "match") {
        if (!row.value.trim()) {
          problems.push({ field: `where[${index}].value`, message: "Give a value or a glob." });
          continue;
        }
        where[key] = parseScalar(row.value);
      } else {
        const values = row.values.filter((v) => v.trim() !== "");
        if (values.length === 0) {
          problems.push({ field: `where[${index}].value`, message: "An empty list never matches." });
          continue;
        }
        where[key] = values.map(parseScalar);
      }
      continue;
    }
    const ops: WhereOps = {};
    for (const { row, index } of entries) {
      let op: keyof WhereOps;
      switch (row.op) {
        case "match":
          // In an operator table a plain match is a glob (strings) or equality (others).
          op = typeof parseScalar(row.value) === "string" ? "glob" : "eq";
          break;
        case "any":
          op = "in";
          break;
        case "missing":
          op = "exists";
          break;
        default:
          op = row.op;
      }
      if (ops[op] !== undefined) {
        problems.push({ field: `where[${index}].op`, message: `"${key}" already has a ${WHERE_OP_LABEL[row.op]} condition.` });
        continue;
      }
      if (row.op === "exists" || row.op === "missing") {
        ops.exists = row.op === "exists";
      } else if (LIST_OPS.has(row.op)) {
        const values = row.values.filter((v) => v.trim() !== "");
        if (values.length === 0) {
          problems.push({ field: `where[${index}].value`, message: "Add at least one value." });
          continue;
        }
        ops.in = values.map(parseScalar);
      } else if (NUMERIC_OPS.has(row.op)) {
        const v = parseScalar(row.value);
        if (typeof v !== "number") {
          problems.push({ field: `where[${index}].value`, message: "Needs a number." });
          continue;
        }
        (ops as Record<string, unknown>)[op] = v;
      } else if (op === "glob") {
        if (!row.value.trim()) {
          problems.push({ field: `where[${index}].value`, message: "Give a glob." });
          continue;
        }
        ops.glob = row.op === "glob" ? row.value : String(parseScalar(row.value));
      } else {
        if (!row.value.trim()) {
          problems.push({ field: `where[${index}].value`, message: "Give a value." });
          continue;
        }
        (ops as Record<string, unknown>)[op] = parseScalar(row.value);
      }
    }
    if (Object.keys(ops).length > 0) where[key] = ops;
  }
  return { where, problems };
}

// ---- the whole rule ----

export function projectOf(origin: ConfigOrigin): string | null {
  return origin.kind === "project" ? origin.name : null;
}

/** `config.name` is `project/rule` for project rules; the form edits the bare name. */
export function bareName(fullName: string, project: string | null): string {
  return project && fullName.startsWith(`${project}/`) ? fullName.slice(project.length + 1) : fullName;
}

export function fullName(form: Pick<RuleForm, "name" | "project">): string {
  return form.project ? `${form.project}/${form.name}` : form.name;
}

export function ruleToForm(rule: RuleConfig, origin: ConfigOrigin | null): RuleForm {
  const project = origin ? projectOf(origin) : null;
  return {
    name: bareName(rule.name, project),
    project,
    description: rule.description ?? "",
    on: [...(rule.on ?? [])],
    // `where`, `debounce_by` and the flags are omitted on the wire when they hold defaults.
    where: whereToRows(rule.where),
    debounce: rule.debounce ?? "",
    debounceBy: rule.debounce_by ?? "rule",
    cooldown: rule.cooldown ?? "",
    rateLimit: rule.rate_limit ?? "",
    supervised: rule.supervised ?? false,
    enabled: rule.enabled ?? true,
    archived: rule.archived ?? false,
    agent: rule.run.agent,
    prompt: rule.run.prompt ?? "",
  };
}

export function infoToForm(info: RuleInfo): RuleForm {
  return ruleToForm(info.config, info.origin);
}

export interface ToRuleOptions {
  /** The named agent uses the `command` adapter, which may run without a prompt. */
  agentIsCommand: boolean;
}

/** The form as a `RuleConfig` (bare name) and what is wrong with it. */
export function formToRule(form: RuleForm, opts: ToRuleOptions): { rule: RuleConfig; problems: FieldProblem[] } {
  const problems: FieldProblem[] = [];
  const name = form.name.trim();
  const np = nameProblem(name);
  if (np) problems.push({ field: "name", message: np });

  const on = form.on.map((t) => t.trim()).filter(Boolean);
  if (on.length === 0) problems.push({ field: "on", message: "Pick at least one event type." });
  for (const t of on) {
    const p = typeGlobProblem(t);
    if (p) problems.push({ field: "on", message: p });
  }

  const { where, problems: whereProblems } = rowsToWhere(form.where);
  problems.push(...whereProblems);

  const duration = (field: string, text: string): string | undefined => {
    const t = text.trim();
    if (!t) return undefined;
    if (parseDuration(t) === null) {
      problems.push({ field, message: `"${t}" is not a duration. Use 500ms, 10s, 15m, 1h30m or 7d.` });
    }
    return t;
  };
  const debounce = duration("debounce", form.debounce);
  const cooldown = duration("cooldown", form.cooldown);

  const rate = form.rateLimit.trim();
  if (rate && parseRate(rate) === null) {
    problems.push({ field: "rate_limit", message: `"${rate}" is not a rate. Use 10/hour, 3/minute or 5/10m.` });
  }

  const agent = form.agent.trim();
  if (!agent) problems.push({ field: "run.agent", message: "Pick the agent to wake." });
  const prompt = form.prompt;
  if (!prompt.trim() && !opts.agentIsCommand) {
    problems.push({ field: "run.prompt", message: "Write the instruction the agent gets." });
  }
  for (const p of templateProblems(prompt).problems) problems.push({ field: "run.prompt", message: p });

  const rule: RuleConfig = {
    name,
    on,
    where,
    debounce_by: form.debounceBy,
    supervised: form.supervised,
    enabled: form.enabled,
    archived: form.archived,
    run: prompt.trim() ? { agent, prompt } : { agent },
  };
  if (form.description.trim()) rule.description = form.description.trim();
  if (debounce !== undefined) rule.debounce = debounce;
  if (cooldown !== undefined) rule.cooldown = cooldown;
  if (rate) rule.rate_limit = rate;
  return { rule, problems };
}
