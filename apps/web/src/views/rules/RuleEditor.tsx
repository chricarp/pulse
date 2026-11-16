import { useEffect, useRef, useState } from "react";

import { useApp, useDaemon } from "../../app/context";
import { useBindings } from "../../app/keys";
import { navigate, useLocation } from "../../app/router";
import { toast } from "../../app/toasts";
import type { ConfigError, Expected, RuleInfo } from "../../contracts";
import { expectedFor, staleOf } from "../../lib/edits";
import {
  LIST_OPS,
  NO_VALUE_OPS,
  WHERE_OPS,
  WHERE_OP_LABEL,
  emptyRuleForm,
  formToRule,
  fullName,
  infoToForm,
  scalarKind,
  type FieldProblem,
  type RuleForm,
  type WhereRow,
} from "../../lib/ruleForm";
import { typeGlobMatches, typeGlobProblem } from "../../lib/syntax";
import { ErrorCodes, RpcCallError } from "../../rpc/client";
import { Icon } from "../../ui/icons";
import { Button, Field, IconButton, Input, ListInput, Notice, Select, TextArea, Toggle, cx } from "../../ui/primitives";
import { ConflictNotice, hasConflict } from "../common/Conflict";
import { RuleTester } from "./RuleTester";

const TEMPLATE_VARS = ["subject", "type", "source", "time", "id", "rule", "agent"];

/**
 * `opened`: the rule the editor was opened on (null: a new rule). `current`: that rule as the
 * daemon has it now (null once removed), which may differ: the file is edited by hand, the CLI
 * and other windows while this is open.
 */
export function RuleEditor({ opened, current }: { opened: RuleInfo | null; current: RuleInfo | null }) {
  const existing = opened;
  const { client } = useApp();
  const agents = useDaemon((s) => s.agents);
  const projects = useDaemon((s) => s.projects);
  const types = useDaemon((s) => s.eventTypes);
  const status = useDaemon((s) => s.status);
  const { search } = useLocation();
  const params = new URLSearchParams(search);
  const [form, setForm] = useState<RuleForm>(() => {
    if (existing) return infoToForm(existing);
    const f = emptyRuleForm();
    const on = params.get("on");
    if (on) f.on = [on];
    return f;
  });
  const [touched, setTouched] = useState(false);
  const [serverErrors, setServerErrors] = useState<ConfigError[]>([]);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  // What the form's edits are based on; a save names its revision.
  const [base, setBase] = useState<RuleInfo | null>(opened);
  // The rule as it was when a save was refused as stale (the list may not have caught up yet).
  const [refused, setRefused] = useState<{ current: RuleInfo | null } | null>(null);
  const first = useRef<HTMLInputElement>(null);

  useEffect(() => {
    first.current?.focus();
  }, []);

  const set = <K extends keyof RuleForm>(k: K, v: RuleForm[K]) => {
    setForm((f) => ({ ...f, [k]: v }));
    setDirty(true);
  };
  const agent = (agents ?? []).find((a) => a.config.name === resolveAgent(form, agents?.map((x) => x.config.name) ?? []));
  const isCommand = agent?.config.adapter === "command";
  const { rule, problems } = formToRule(form, { agentIsCommand: isCommand });
  const show = (field: string): string | undefined => {
    const server = serverErrors.find((e) => e.field === field || (field === "where" && e.field?.startsWith("where")));
    if (server) return server.message;
    if (!touched) return undefined;
    return problems.find((p) => p.field === field)?.message;
  };
  const whereProblem = (i: number, part: string) => (touched ? problems.find((p) => p.field === `where[${i}].${part}`)?.message : undefined);
  const unmatchedServer = serverErrors.filter((e) => !["name", "on", "debounce", "cooldown", "rate_limit", "run.agent", "run.prompt"].includes(e.field ?? "") && !e.field?.startsWith("where"));

  const trusted = (projects ?? []).filter((p) => p.state === "trusted");
  const runnable = (agents ?? []).filter((a) => a.config.adapter !== "mcp");
  const typeSuggestions = [...new Set(["fs.*", "fs.file.*", "git.*", "process.*", "schedule.*", "calendar.*", "ext.*", ...(types ?? []).map((t) => t.type)])];
  const exampleKeys = new Set<string>(["subject", "source", "type"]);
  for (const t of types ?? []) {
    if (form.on.some((g) => !typeGlobProblem(g) && typeGlobMatches(g, t.type)) && t.example && typeof t.example === "object") {
      for (const k of Object.keys(t.example as object)) exampleKeys.add(k);
    }
  }

  const live = opened ? (refused && current?.revision === base?.revision ? refused.current : current) : (refused?.current ?? null);
  const conflict = hasConflict(base, live);

  const save = async (expected: Expected = expectedFor(base)) => {
    setTouched(true);
    if (problems.length > 0) {
      toast("Fix the highlighted fields first", "error");
      return;
    }
    setSaving(true);
    setServerErrors([]);
    try {
      const info = await client.call("rules.upsert", { rule, ...(form.project ? { project: form.project } : {}), expected });
      setDirty(false);
      toast(existing ? `Saved ${info.config.name}` : `Created ${info.config.name}`, "ok", `Written to ${info.origin.path}`);
      navigate({ view: "rules", name: info.config.name, mode: "show" }, { replace: true });
    } catch (err) {
      if (staleOf(err)) {
        // Changed (or created, or removed) elsewhere: show what is there now; the form stays.
        const now = await client.call("rules.get", { name: fullName(form) }).catch(() => null);
        setRefused({ current: now });
      } else if (err instanceof RpcCallError && err.code === ErrorCodes.ConfigInvalid) {
        setServerErrors((err.data as { errors?: ConfigError[] } | undefined)?.errors ?? [{ message: err.message }]);
      } else {
        toast("Save failed", "error", err instanceof Error ? err.message : String(err));
      }
    }
    setSaving(false);
  };
  const cancel = () => {
    if (dirty && !window.confirm("Discard your changes to this rule?")) return;
    if (existing) navigate({ view: "rules", name: existing.config.name, mode: "show" });
    else navigate({ view: "rules", name: null, mode: "show" });
  };

  useBindings([
    { keys: "mod+s", label: "Save the rule", group: "Editor", run: () => void save() },
    { keys: "escape", label: "Cancel editing", group: "Editor", run: cancel },
  ]);

  const insertVar = (v: string) => {
    const el = document.getElementById("rule-prompt") as HTMLTextAreaElement | null;
    const token = `{{${v}}}`;
    if (!el) {
      set("prompt", form.prompt + token);
      return;
    }
    const start = el.selectionStart ?? form.prompt.length;
    const end = el.selectionEnd ?? start;
    const next = form.prompt.slice(0, start) + token + form.prompt.slice(end);
    set("prompt", next);
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(start + token.length, start + token.length);
    });
  };

  const ownerPath = form.project ? trusted.find((p) => p.name === form.project)?.file : status ? `${status.pulse_home}/pulse.toml` : "pulse.toml";

  return (
    <div className="editor-layout">
      <form
        className="editor"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
        aria-label={existing ? `Edit rule ${existing.config.name}` : "New rule"}
      >
        <header className="view-head editor-head">
          <IconButton icon="back" label="Back" variant="ghost" size="sm" onClick={cancel} />
          <h1>{existing ? `Edit ${existing.config.name}` : "New rule"}</h1>
          <div className="view-actions">
            <Button onClick={cancel} shortcut="escape">
              Cancel
            </Button>
            <Button type="submit" variant="primary" disabled={saving} shortcut="mod+s">
              {saving ? "Saving…" : existing ? "Save" : "Create rule"}
            </Button>
          </div>
        </header>
        <div className="editor-body">
          {conflict && (
            <ConflictNotice
              what="rule"
              name={fullName(form)}
              base={base}
              current={live}
              onReload={() => {
                if (!live) return;
                setForm(infoToForm(live));
                setBase(live);
                setRefused(null);
                setDirty(false);
              }}
              onOverwrite={(expected) => void save(expected)}
            />
          )}
          {(serverErrors.length > 0 || unmatchedServer.length > 0) && (
            <Notice tone="error" title="The daemon did not accept this rule">
              <ul className="plain">
                {(unmatchedServer.length ? unmatchedServer : serverErrors).map((e, i) => (
                  <li key={i}>
                    {e.field && <code>{e.field}</code>} {e.message}
                  </li>
                ))}
              </ul>
            </Notice>
          )}
          <fieldset className="group">
            <legend>Rule</legend>
            <Field label="Saved in" hint={<span className="mono">{ownerPath}</span>}>
              {existing ? (
                <Input value={form.project ? `project ${form.project}` : "your pulse.toml"} disabled />
              ) : (
                <Select value={form.project ?? ""} onChange={(e) => set("project", e.target.value || null)}>
                  <option value="">Your pulse.toml</option>
                  {trusted.map((p) => (
                    <option key={p.name} value={p.name}>
                      Project {p.name} ({p.file})
                    </option>
                  ))}
                </Select>
              )}
            </Field>
            <Field label="Name" htmlFor="rule-name" error={show("name")} hint={form.project ? `Full name: ${fullName(form)}` : "Lowercase letters, digits, - and _."}>
              <Input id="rule-name" ref={first} mono value={form.name} onChange={(e) => set("name", e.target.value)} disabled={existing !== null} placeholder="new-lecture" />
            </Field>
            <Field label="Description" htmlFor="rule-desc">
              <Input id="rule-desc" value={form.description} onChange={(e) => set("description", e.target.value)} placeholder="What this rule is for (optional)" />
            </Field>
          </fieldset>

          <fieldset className="group">
            <legend>When</legend>
            <Field label="Event types" htmlFor="rule-on" error={show("on")} hint="Type globs: fs.* covers fs.file.created and fs.burst. Enter to add.">
              <ListInput id="rule-on" values={form.on} onChange={(v) => set("on", v)} suggestions={typeSuggestions} problem={typeGlobProblem} placeholder="fs.file.created" />
            </Field>
            <Field
              label="Where"
              error={show("where")}
              hint={
                <>
                  Every condition must hold. Keys are <code>subject</code>, <code>source</code>, <code>type</code> or a data field like <code>path</code>. A string starting with / or ~ is a path glob.
                </>
              }
            >
              <WhereEditor rows={form.where} onChange={(w) => set("where", w)} keys={[...exampleKeys]} problem={whereProblem} />
            </Field>
          </fieldset>

          <fieldset className="group">
            <legend>Limits</legend>
            <Field label="Debounce" htmlFor="rule-debounce" error={show("debounce")} hint="Wait until matching events stop for this long, then fire once with the latest.">
              <div className="inline">
                <Input id="rule-debounce" mono value={form.debounce} onChange={(e) => set("debounce", e.target.value)} placeholder="10s" className="w-short" />
                <Select value={form.debounceBy} onChange={(e) => set("debounceBy", e.target.value as RuleForm["debounceBy"])} aria-label="Debounce per" disabled={!form.debounce}>
                  <option value="rule">one timer for the rule</option>
                  <option value="subject">one timer per subject</option>
                </Select>
              </div>
            </Field>
            <Field label="Cooldown" htmlFor="rule-cooldown" error={show("cooldown")} hint="Minimum time between fires.">
              <Input id="rule-cooldown" mono value={form.cooldown} onChange={(e) => set("cooldown", e.target.value)} placeholder="30s" className="w-short" />
            </Field>
            <Field label="Rate limit" htmlFor="rule-rate" error={show("rate_limit")} hint="At most this many fires per period: 10/hour, 3/minute, 5/10m.">
              <Input id="rule-rate" mono value={form.rateLimit} onChange={(e) => set("rateLimit", e.target.value)} placeholder="10/hour" className="w-short" />
            </Field>
          </fieldset>

          <fieldset className="group">
            <legend>Then</legend>
            <Field label="Wake agent" htmlFor="rule-agent" error={show("run.agent")} hint={agent ? <AgentHint scopes={agent.config.allow ?? []} adapter={agent.config.adapter} /> : "MCP agents are identities for MCP clients and cannot be woken by rules."}>
              <Select id="rule-agent" value={form.agent} onChange={(e) => set("agent", e.target.value)}>
                <option value="">Choose an agent…</option>
                {runnable.map((a) => (
                  <option key={a.config.name} value={a.origin.kind === "project" && form.project === a.origin.name ? a.config.name.slice(a.origin.name.length + 1) : a.config.name}>
                    {a.config.name} ({a.config.adapter})
                  </option>
                ))}
              </Select>
            </Field>
            <Field
              label="Prompt"
              htmlFor="rule-prompt"
              wide
              error={show("run.prompt")}
              hint={isCommand ? "Optional for command agents; they get it on stdin if configured." : "The instruction. The event itself is appended as fenced, untrusted data."}
            >
              <TextArea id="rule-prompt" mono rows={6} value={form.prompt} onChange={(e) => set("prompt", e.target.value)} placeholder="Summarize {{subject}} and write flashcards next to it." />
              <div className="vars">
                <span className="faint">Insert</span>
                {[...TEMPLATE_VARS, ...[...exampleKeys].filter((k) => !TEMPLATE_VARS.includes(k))].map((v) => (
                  <button key={v} type="button" className="var-chip mono" onClick={() => insertVar(v)}>
                    {`{{${v}}}`}
                  </button>
                ))}
              </div>
            </Field>
            <Field label="Approval">
              <Toggle checked={form.supervised} onChange={(v) => set("supervised", v)} label="Supervised" hint="Each run waits for you to approve it before anything starts." />
            </Field>
            <Field label="State">
              <Toggle checked={form.enabled} onChange={(v) => set("enabled", v)} label="Enabled" hint="A disabled rule still records why it did not fire." />
            </Field>
          </fieldset>
        </div>
      </form>
      <aside className="editor-side">
        <RuleTester
          makeRef={() => (problems.length ? `Fix the rule first: ${problems.map((p: FieldProblem) => p.message).join(" ")}` : { kind: "draft", rule, ...(form.project ? { project: form.project } : {}) })}
          on={form.on}
          initialEvent={params.get("event") ?? params.get("test")}
          defaultIgnoreState
          onConfigErrors={setServerErrors}
        />
      </aside>
    </div>
  );
}

function resolveAgent(form: RuleForm, names: string[]): string {
  if (form.project && names.includes(`${form.project}/${form.agent}`)) return `${form.project}/${form.agent}`;
  return form.agent;
}

function AgentHint({ scopes, adapter }: { scopes: string[]; adapter: string }) {
  return (
    <>
      {adapter} agent. It only receives events its scopes cover:{" "}
      {scopes.length ? scopes.map((s) => <code key={s}>{s}</code>) : <strong>none (it can see nothing)</strong>}
    </>
  );
}

function WhereEditor({ rows, onChange, keys, problem }: { rows: WhereRow[]; onChange: (r: WhereRow[]) => void; keys: string[]; problem: (i: number, part: string) => string | undefined }) {
  const update = (i: number, patch: Partial<WhereRow>) => onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div className="where">
      {rows.length === 0 && <p className="faint">No conditions: every event of these types matches.</p>}
      {rows.map((r, i) => (
        <div key={i} className="where-row">
          <Input mono value={r.key} onChange={(e) => update(i, { key: e.target.value })} placeholder="path" list="where-keys" aria-label="Field" className={cx(problem(i, "key") && "is-invalid")} title={problem(i, "key")} />
          <Select value={r.op} onChange={(e) => update(i, { op: e.target.value as WhereRow["op"] })} aria-label="Operator">
            {WHERE_OPS.map((op) => (
              <option key={op} value={op}>
                {WHERE_OP_LABEL[op]}
              </option>
            ))}
          </Select>
          {NO_VALUE_OPS.has(r.op) ? (
            <span className="faint where-novalue">no value</span>
          ) : LIST_OPS.has(r.op) ? (
            <ListInput values={r.values} onChange={(v) => update(i, { values: v })} placeholder="value, Enter" />
          ) : (
            <div className="where-value">
              <Input mono value={r.value} onChange={(e) => update(i, { value: e.target.value })} placeholder={r.op === "match" ? "~/University/**/*.pdf" : "0"} aria-label="Value" title={problem(i, "value")} className={cx(problem(i, "value") && "is-invalid")} />
              {r.value && <span className="where-kind faint">{scalarKind(r.value)}</span>}
            </div>
          )}
          <IconButton icon="x" label="Remove condition" size="sm" variant="ghost" onClick={() => onChange(rows.filter((_, j) => j !== i))} />
          {(problem(i, "key") || problem(i, "value") || problem(i, "op")) && <p className="field-error where-error">{problem(i, "key") ?? problem(i, "value") ?? problem(i, "op")}</p>}
        </div>
      ))}
      <datalist id="where-keys">
        {keys.map((k) => (
          <option key={k} value={k} />
        ))}
      </datalist>
      <Button size="sm" icon="plus" onClick={() => onChange([...rows, { key: "", op: "match", value: "", values: [] }])}>
        Add condition
      </Button>
      <p className="faint where-note">
        <Icon name="alert" size={11} /> Quote a value to force text: <code>"1"</code> is the text 1, <code>1</code> the number.
      </p>
    </div>
  );
}
