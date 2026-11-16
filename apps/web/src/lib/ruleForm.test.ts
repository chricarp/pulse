import { describe, expect, it } from "vitest";

import type { RuleConfig } from "../contracts";
import {
  emptyRuleForm,
  formToRule,
  parseScalar,
  formatScalar,
  ruleToForm,
  rowsToWhere,
  whereToRows,
} from "./ruleForm";

const fullRule: RuleConfig = {
  name: "new-lecture",
  description: "Summarize new lecture PDFs",
  on: ["fs.file.created", "fs.file.moved"],
  where: {
    path: "~/University/**/*.pdf",
    exit_code: { ne: 0 },
    size: { gt: 5, lte: 10.5 },
    branch: ["main", "release/{a,b}"],
    "data.tags": { contains: "urgent" },
    owner: { exists: false },
    label: "1",
  },
  debounce: "10s",
  debounce_by: "subject",
  cooldown: "30s",
  rate_limit: "10/hour",
  supervised: true,
  enabled: false,
  archived: false,
  run: { agent: "study", prompt: "Summarize {{subject}} and create flashcards next to it." },
};

describe("rule form mapping", () => {
  it("round-trips every RuleConfig field through the form", () => {
    const form = ruleToForm(fullRule, { kind: "user", path: "/home/me/.pulse/pulse.toml" });
    const { rule, problems } = formToRule(form, { agentIsCommand: false });
    expect(problems).toEqual([]);
    expect(rule).toEqual(fullRule);
  });

  it("edits project rules by their bare name", () => {
    const form = ruleToForm(
      { ...fullRule, name: "thesis/build-check" },
      { kind: "project", name: "thesis", dir: "/home/me/thesis", path: "/home/me/thesis/pulse.toml" },
    );
    expect(form.name).toBe("build-check");
    expect(form.project).toBe("thesis");
    expect(formToRule(form, { agentIsCommand: false }).rule.name).toBe("build-check");
  });

  it("fills defaults the daemon leaves off the wire", () => {
    // `where`, `debounce_by` and the flags are skipped by serde when they hold their defaults.
    const sparse = { name: "x", on: ["git.*"], run: { agent: "a", prompt: "p" } } as unknown as RuleConfig;
    const form = ruleToForm(sparse, null);
    expect(form).toMatchObject({ where: [], debounceBy: "rule", enabled: true, supervised: false, archived: false });
  });

  it("omits empty optional fields and keeps the prompt off command rules without one", () => {
    const form = { ...emptyRuleForm(), name: "lint", on: ["fs.file.modified"], agent: "linter" };
    const { rule, problems } = formToRule(form, { agentIsCommand: true });
    expect(problems).toEqual([]);
    expect(rule).toEqual({
      name: "lint",
      on: ["fs.file.modified"],
      where: {},
      debounce_by: "rule",
      supervised: false,
      enabled: true,
      archived: false,
      run: { agent: "linter" },
    });
  });

  it("reports each problem against its field", () => {
    const form = {
      ...emptyRuleForm(),
      name: "Bad Name",
      on: ["FS.*"],
      debounce: "10 seconds",
      cooldown: "30m1h",
      rateLimit: "10/fortnight",
      prompt: "Look at {{nope!}}",
      where: [{ key: "", op: "match" as const, value: "x", values: [] }],
    };
    const fields = formToRule(form, { agentIsCommand: false }).problems.map((p) => p.field);
    expect(fields).toEqual(["name", "on", "where[0].key", "debounce", "cooldown", "rate_limit", "run.agent", "run.prompt"]);
  });

  it("keeps string literals that look like numbers or booleans as strings", () => {
    expect(parseScalar('"1"')).toBe("1");
    expect(parseScalar("1")).toBe(1);
    expect(parseScalar("-2.5")).toBe(-2.5);
    expect(parseScalar("false")).toBe(false);
    expect(parseScalar("~/a/**")).toBe("~/a/**");
    for (const v of ["1", "true", "plain", 3, false]) expect(parseScalar(formatScalar(v))).toEqual(v);
  });

  it("groups several rows for one key into an operator table", () => {
    const { where, problems } = rowsToWhere([
      { key: "size", op: "gt", value: "5", values: [] },
      { key: "size", op: "lte", value: "10", values: [] },
      { key: "path", op: "match", value: "/a/**", values: [] },
      { key: "path", op: "missing", value: "", values: [] },
    ]);
    expect(problems).toEqual([]);
    expect(where).toEqual({ size: { gt: 5, lte: 10 }, path: { glob: "/a/**", exists: false } });
  });

  it("rejects conflicting and malformed operator rows", () => {
    const { problems } = rowsToWhere([
      { key: "size", op: "gt", value: "big", values: [] },
      { key: "n", op: "eq", value: "1", values: [] },
      { key: "n", op: "eq", value: "2", values: [] },
      { key: "tags", op: "any", value: "", values: [" "] },
    ]);
    expect(problems.map((p) => p.field)).toEqual(["where[0].value", "where[2].op", "where[3].value"]);
  });

  it("reads operator tables back as one row per operator", () => {
    expect(whereToRows({ size: { gt: 5, lte: 10 }, x: { exists: true } }).map((r) => `${r.key} ${r.op} ${r.value}`)).toEqual([
      "size gt 5",
      "size lte 10",
      "x exists ",
    ]);
  });
});
