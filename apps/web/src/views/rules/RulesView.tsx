import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef, useState, type ReactNode } from "react";

import { useApp, useDaemon } from "../../app/context";
import { useTildify } from "../../app/home";
import { useBindings } from "../../app/keys";
import { href, navigate, useLocation } from "../../app/router";
import { attempt } from "../../app/toasts";
import type { ConfigError, Delivery, DeliveryKind, RuleConfig, RuleInfo, WhereCondition } from "../../contracts";
import { RUN_STATUS_LABEL, deliveryLabel, deliveryTone, outcomeSentence, runTone } from "../../lib/explain";
import { dayAndTime, plural, relative, shortId } from "../../lib/format";
import { mergeDeliveries } from "../../lib/history";
import { createStore, useStore } from "../../store/store";
import { Icon } from "../../ui/icons";
import { Ago, Button, Confirm, Empty, IconButton, Input, Menu, Notice, Pill, Section, Toggle, Trunc, cx, useNow } from "../../ui/primitives";
import { useFirst } from "../common/Conflict";
import { useRunList } from "../runs/RunsView";
import { RuleEditor } from "./RuleEditor";
import { RuleTester } from "./RuleTester";

const showArchived = createStore(false);
const NO_ERRORS: ConfigError[] = [];

export function RulesView({ name, mode }: { name: string | null; mode: "show" | "edit" | "new" }) {
  const rules = useDaemon((s) => s.rules);
  if (mode === "new") return <RuleEditor opened={null} current={null} />;
  const info = name ? (rules ?? []).find((r) => r.config.name === name) : undefined;
  if (mode === "edit") return <EditRule key={name} name={name ?? ""} rules={rules} info={info} />;
  return (
    <div className={cx("split", name && "has-detail", "rules-split")}>
      <div className="split-main">
        <RulesList selected={name} />
      </div>
      {name && (info ? <RuleDetail key={name} info={info} /> : <MissingRule name={name} loaded={rules !== null} />)}
    </div>
  );
}

/** The editor stays open (with what the user typed) if the rule is removed meanwhile. */
function EditRule({ name, rules, info }: { name: string; rules: RuleInfo[] | null; info: RuleInfo | undefined }) {
  const opened = useFirst(info);
  if (!rules && !opened) return <Empty title="Loading…" />;
  if (!opened) return <Empty title={`No rule named ${name}`} action={<Button onClick={() => navigate({ view: "rules", name: null, mode: "show" })}>All rules</Button>} />;
  return <RuleEditor opened={opened} current={info ?? null} />;
}

function MissingRule({ name, loaded }: { name: string; loaded: boolean }) {
  return (
    <aside className="detail">
      <div className="detail-body">
        {loaded ? (
          <Notice tone="neutral" title={`No rule named ${name}`}>
            It may have been deleted, or its file has config errors and was not loaded. Its history is kept: <a href={`/runs?rule=${encodeURIComponent(name)}`}>runs of {name}</a>.
          </Notice>
        ) : (
          <p className="faint">Loading…</p>
        )}
      </div>
    </aside>
  );
}

export function ConfigErrors({ errors }: { errors: ConfigError[] }) {
  if (errors.length === 0) return null;
  return (
    <Notice tone="error" title={`${plural(errors.length, "problem")} in config files`}>
      <p>Until it is fixed, Pulse keeps using the last version of the file that worked (a project's file with errors is inactive instead). Edit the file; Pulse reloads it on save.</p>
      <ConfigProblemList problems={errors} />
    </Notice>
  );
}

/** Definitions that loaded but cannot work as written. Nothing is disabled; the user is told. */
export function ConfigWarnings({ warnings }: { warnings: ConfigError[] }) {
  if (warnings.length === 0) return null;
  return (
    <Notice tone="attention" title={`${plural(warnings.length, "definition")} that cannot work as written`}>
      <p>These are loaded and in effect, but will not do what they say until the cause is fixed.</p>
      <ConfigProblemList problems={warnings} />
    </Notice>
  );
}

function ConfigProblemList({ problems }: { problems: ConfigError[] }) {
  const tildify = useTildify();
  return (
    <ul className="config-errors">
      {problems.map((e, i) => (
        <li key={i}>
          <span className="mono" title={e.file ?? undefined}>
            {e.file ? tildify(e.file) : "config"}
            {e.line ? `:${e.line}` : ""}
            {e.column ? `:${e.column}` : ""}
          </span>{" "}
          {e.item && <span className="mono">{e.item}</span>} {e.field && <code>{e.field}</code>} <span>{e.message}</span>
        </li>
      ))}
    </ul>
  );
}

function ruleTone(r: RuleInfo, now: number): { tone: "ok" | "neutral" | "attention" | "loop" | "blocked"; label: string } {
  if (r.config.archived) return { tone: "blocked", label: "archived" };
  if (r.state.circuit_open_until && Date.parse(r.state.circuit_open_until) > now) return { tone: "loop", label: "circuit open" };
  if (r.state.snoozed_until && Date.parse(r.state.snoozed_until) > now) return { tone: "attention", label: "snoozed" };
  if (!r.config.enabled) return { tone: "neutral", label: "disabled" };
  return { tone: "ok", label: "enabled" };
}

function RulesList({ selected }: { selected: string | null }) {
  const rules = useDaemon((s) => s.rules);
  const errors = useDaemon((s) => s.configErrors) ?? NO_ERRORS;
  const warnings = useDaemon((s) => s.configWarnings) ?? NO_ERRORS;
  const archived = useStore(showArchived, (v) => v);
  const [q, setQ] = useState("");
  const now = useNow();
  const input = useRef<HTMLInputElement>(null);
  const list = (rules ?? [])
    .filter((r) => archived || !r.config.archived)
    .filter((r) => !q || `${r.config.name} ${r.config.on.join(" ")} ${r.config.run.agent} ${r.config.description ?? ""}`.toLowerCase().includes(q.toLowerCase()));
  const archivedCount = (rules ?? []).filter((r) => r.config.archived).length;
  const move = (d: number) => {
    const i = list.findIndex((r) => r.config.name === selected);
    const next = list[Math.max(0, Math.min(list.length - 1, i < 0 ? 0 : i + d))];
    if (next) navigate({ view: "rules", name: next.config.name, mode: "show" }, { replace: selected !== null });
  };
  useBindings([
    { keys: "j", label: "Next rule", group: "Lists", run: () => move(1) },
    { keys: "k", label: "Previous rule", group: "Lists", run: () => move(-1) },
    { keys: "arrowdown", label: "Next rule", group: "Lists", run: () => move(1), hidden: true },
    { keys: "arrowup", label: "Previous rule", group: "Lists", run: () => move(-1), hidden: true },
    { keys: "/", label: "Search rules", group: "Rules", run: () => input.current?.focus() },
    { keys: "escape", label: "Close the detail", group: "Lists", run: () => navigate({ view: "rules", name: null, mode: "show" }) },
  ]);
  return (
    <>
      <header className="view-head">
        <h1>Rules</h1>
        <span className="sub num">{plural(list.length, "rule")}</span>
        <div className="view-actions">
          <Input ref={input} value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search rules  /" aria-label="Search rules" className="w-search" />
          <Toggle checked={archived} onChange={(v) => showArchived.set(v)} label={`Archived (${archivedCount})`} />
          <Button variant="primary" icon="plus" onClick={() => navigate({ view: "rules", name: null, mode: "new" })} shortcut="n">
            New rule
          </Button>
        </div>
      </header>
      {errors.length > 0 && (
        <div className="list-notice">
          <ConfigErrors errors={errors} />
          <ConfigWarnings warnings={warnings} />
        </div>
      )}
      <div className="table-scroll">
        {rules && list.length === 0 ? (
          <Empty title={q ? "No rule matches" : "No rules yet"} action={<Button variant="primary" icon="plus" onClick={() => navigate({ view: "rules", name: null, mode: "new" })}>New rule</Button>}>
            A rule says: when an event matches, wake this agent with this prompt.
          </Empty>
        ) : (
          <ul className="rule-list" role="listbox" aria-label="Rules">
            {list.map((r) => {
              const st = ruleTone(r, now);
              return (
                <li key={r.config.name} role="option" aria-selected={r.config.name === selected}>
                  <a
                    className={cx("rule-row", r.config.name === selected && "is-selected", r.config.archived && "is-archived")}
                    href={href({ view: "rules", name: r.config.name, mode: "show" })}
                    onClick={(e) => {
                      e.preventDefault();
                      navigate({ view: "rules", name: r.config.name, mode: "show" }, { replace: selected !== null });
                    }}
                  >
                    <span className={cx("dot", `tone-${st.tone}`)} title={st.label} />
                    <span className="rule-name mono">
                      {r.origin.kind === "project" ? (
                        <>
                          <span className="faint">{r.origin.name}/</span>
                          {r.config.name.slice(r.origin.name.length + 1)}
                        </>
                      ) : (
                        r.config.name
                      )}
                    </span>
                    <span className="rule-on mono">{r.config.on.join(", ")}</span>
                    <span className="rule-agent mono">→ {r.config.run.agent}</span>
                    <span className="rule-tags">
                      {st.tone !== "ok" && <Pill tone={st.tone}>{st.label}{st.label === "snoozed" && r.state.snoozed_until ? ` · ${relative(r.state.snoozed_until, now).replace("in ", "")}` : ""}</Pill>}
                      {r.config.supervised && (
                        <Pill tone="neutral" icon="shield" title="Each run waits for approval">
                          supervised
                        </Pill>
                      )}
                    </span>
                    <span className="rule-last faint num">{r.state.last_fired_at ? `fired ${relative(r.state.last_fired_at, now)}` : "never fired"}</span>
                  </a>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </>
  );
}

function whereText(key: string, c: WhereCondition): string {
  if (Array.isArray(c)) return `${key} is any of ${c.map((v) => JSON.stringify(v)).join(", ")}`;
  if (typeof c === "object" && c !== null) {
    return Object.entries(c)
      .map(([op, v]) => (op === "exists" ? `${key} ${v ? "exists" : "is missing"}` : `${key} ${op} ${JSON.stringify(v)}`))
      .join(" and ");
  }
  return typeof c === "string" ? `${key} matches ${c}` : `${key} = ${String(c)}`;
}

export function RuleSummary({ rule }: { rule: RuleConfig }) {
  const where = Object.entries(rule.where ?? {});
  const limits: string[] = [];
  if (rule.debounce) limits.push(`debounce ${rule.debounce}${rule.debounce_by === "subject" ? " per subject" : ""}`);
  if (rule.cooldown) limits.push(`cooldown ${rule.cooldown}`);
  if (rule.rate_limit) limits.push(`at most ${rule.rate_limit}`);
  return (
    <div className="rule-summary">
      <p>
        <span className="kw">When</span> {rule.on.map((t, i) => <code key={i}>{t}</code>)}
        {where.length > 0 && (
          <>
            {" "}
            <span className="kw">where</span>{" "}
            {where.map(([k, c], i) => (
              <span key={k}>
                {i > 0 && <span className="kw"> and </span>}
                <code>{whereText(k, c)}</code>
              </span>
            ))}
          </>
        )}
      </p>
      <p>
        <span className="kw">wake</span> <code>{rule.run.agent}</code>
        {rule.supervised && <span className="kw"> after you approve each run</span>}
        {limits.length > 0 && <span className="faint"> · {limits.join(" · ")}</span>}
      </p>
    </div>
  );
}

function snoozeTargets(now: number): { label: string; until: string; hint: string }[] {
  const at = (ms: number) => new Date(now + ms).toISOString();
  const morning = new Date(now);
  morning.setDate(morning.getDate() + 1);
  morning.setHours(9, 0, 0, 0);
  return [
    { label: "1 hour", until: at(3_600_000), hint: dayAndTime(at(3_600_000), now) },
    { label: "4 hours", until: at(4 * 3_600_000), hint: dayAndTime(at(4 * 3_600_000), now) },
    { label: "Until tomorrow 9:00", until: morning.toISOString(), hint: dayAndTime(morning.toISOString(), now) },
    { label: "1 week", until: at(7 * 86_400_000), hint: "" },
  ];
}

function RuleDetail({ info }: { info: RuleInfo }) {
  const { client } = useApp();
  const { search } = useLocation();
  const testEvent = new URLSearchParams(search).get("test");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const now = useNow();
  const r = info.config;
  const name = r.name;
  const st = ruleTone(info, Date.now());
  const snoozed = info.state.snoozed_until && Date.parse(info.state.snoozed_until) > Date.now();
  const tripped = info.state.circuit_open_until && Date.parse(info.state.circuit_open_until) > Date.now();
  const call = <T,>(what: string, f: () => Promise<T>, ok: string) => void attempt(what, f, ok);
  const toggleEnabled = () => call(r.enabled ? "Disable" : "Enable", () => client.call("rules.set_enabled", { name, enabled: !r.enabled }), r.enabled ? `Disabled ${name}` : `Enabled ${name}`);
  const unsnooze = () => call("Unsnooze", () => client.call("rules.unsnooze", { name }), `${name} is active again`);
  const archive = () => call(r.archived ? "Reopen" : "Archive", () => client.call(r.archived ? "rules.reopen" : "rules.archive", { name }), r.archived ? `Reopened ${name}` : `Archived ${name}. Reopen it from the archived list.`);

  useBindings([
    { keys: "e", label: "Edit the rule", group: "Rules", run: () => navigate({ view: "rules", name, mode: "edit" }) },
    { keys: "shift+e", label: "Enable or disable the rule", group: "Rules", run: toggleEnabled },
  ]);

  return (
    <aside className="detail" aria-label={`Rule ${name}`}>
      <header className="detail-head">
        <IconButton icon="back" label="Back to rules" className="only-narrow" onClick={() => navigate({ view: "rules", name: null, mode: "show" })} size="sm" variant="ghost" />
        <span className={cx("dot", `tone-${st.tone}`)} />
        <h2 className="detail-title mono">{name}</h2>
        <div className="detail-head-actions">
          <Button size="sm" icon="edit" onClick={() => navigate({ view: "rules", name, mode: "edit" })} shortcut="e">
            Edit
          </Button>
          <IconButton icon="x" label="Close (Esc)" onClick={() => navigate({ view: "rules", name: null, mode: "show" })} size="sm" variant="ghost" className="hide-narrow" />
        </div>
      </header>
      <div className="detail-body">
        {r.description && <p>{r.description}</p>}
        <RuleSummary rule={r} />

        <div className="state-actions">
          <Button size="sm" onClick={toggleEnabled} title="Shift E">
            {r.enabled ? "Disable" : "Enable"}
          </Button>
          {snoozed || tripped ? (
            <Button size="sm" icon="play" onClick={unsnooze}>
              {tripped ? "Close the breaker" : "Unsnooze"}
            </Button>
          ) : (
            <Menu
              label="Snooze"
              icon="clock"
              items={snoozeTargets(now).map((t) => ({ label: t.label, hint: t.hint, onSelect: () => call("Snooze", () => client.call("rules.snooze", { name, until: t.until }), `Snoozed ${name} until ${dayAndTime(t.until, Date.now())}`) }))}
            />
          )}
          <Button size="sm" onClick={archive}>
            {r.archived ? "Reopen" : "Archive"}
          </Button>
          <Button size="sm" variant="danger" icon="trash" onClick={() => setConfirmDelete(true)}>
            Delete
          </Button>
        </div>

        {tripped && info.state.circuit_open_until && (
          <Notice tone="loop" title={<><Icon name="loop" size={14} /> Circuit breaker open until {dayAndTime(info.state.circuit_open_until, now)}</>}>
            The rule fired too often in a short time, which usually means a loop. It is held so it cannot keep waking its agent. Look at its recent deliveries below, then close the breaker when it is safe.
          </Notice>
        )}
        {snoozed && !tripped && info.state.snoozed_until && (
          <Notice tone="attention" title={`Snoozed until ${dayAndTime(info.state.snoozed_until, now)}`}>
            Matching events are recorded as blocked (snoozed) until then.
          </Notice>
        )}
        {!r.enabled && <Notice tone="neutral" title="Disabled">Matching events are recorded as blocked (rule disabled). Nothing fires.</Notice>}
        {r.archived && <Notice tone="neutral" title="Archived">Hidden from active lists and never routed. Its history is kept. Reopen to route it again.</Notice>}

        <Section title="Owned by">
          <div className="owner">
            <Trunc text={info.origin.path} copy />
            {info.origin.kind === "project" && <Pill tone="neutral">project {info.origin.name}</Pill>}
          </div>
          <p className="faint">Editing here rewrites this file in place; comments and ordering are kept.</p>
        </Section>

        {r.run.prompt && (
          <Section title="Prompt template">
            <pre className="code">{r.run.prompt}</pre>
          </Section>
        )}

        <Section title="Test">
          <RuleTester key={testEvent ?? ""} makeRef={() => ({ kind: "saved", name })} on={r.on} initialEvent={testEvent} defaultIgnoreState={false} />
        </Section>

        <RuleHistory name={name} on={r.on} />

        <RuleRuns name={name} />
      </div>
      <Confirm
        open={confirmDelete}
        onClose={() => setConfirmDelete(false)}
        title={`Delete ${name}?`}
        confirm="Delete"
        danger
        onConfirm={async () => {
          const ok = await attempt("Delete", () => client.call("rules.delete", { name, expected: { kind: "revision", revision: info.revision } }), `Deleted ${name}`);
          if (ok) navigate({ view: "rules", name: null, mode: "show" });
        }}
      >
        <p>
          The rule is removed from <span className="mono">{info.origin.path}</span>. Its deliveries and runs stay in the log. To stop it but keep it, archive or disable it instead.
        </p>
      </Confirm>
    </aside>
  );
}

const OUTCOME_TABS: { key: string; label: string; kinds: DeliveryKind[] }[] = [
  { key: "all", label: "All", kinds: [] },
  { key: "fired", label: "Fired", kinds: ["fired"] },
  { key: "approval", label: "Approval", kinds: ["awaiting_approval"] },
  { key: "debounced", label: "Debounced", kinds: ["debounced", "waiting"] },
  { key: "blocked", label: "Blocked", kinds: ["blocked"] },
];

/**
 * The rule's deliveries, newest first, loaded in pages as the list scrolls. It refreshes when
 * this rule records a delivery (blocked ones included, which create no run): the events of its
 * types arrive with their outcomes, and a refresh merges the newest page into what is loaded.
 */
function RuleHistory({ name, on }: { name: string; on: string[] }) {
  const { client } = useApp();
  const [tab, setTab] = useState("all");
  const [items, setItems] = useState<Delivery[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const parent = useRef<HTMLDivElement>(null);
  const kinds = OUTCOME_TABS.find((t) => t.key === tab)?.kinds ?? [];
  const onKey = on.join("\u0000");
  useEffect(() => {
    let live = true;
    setItems([]);
    setCursor(null);
    setLoading(true);
    client.call("rules.history", { name, outcomes: kinds, limit: 100 }).then(
      (p) => {
        if (!live) return;
        setItems((prev) => mergeDeliveries(prev, p.deliveries));
        setCursor(p.next_cursor ?? null);
        setLoading(false);
      },
      () => live && setLoading(false),
    );
    // One refresh at a time; changes during one ask for one more.
    let running = false;
    let again = false;
    const refresh = async () => {
      if (running) {
        again = true;
        return;
      }
      running = true;
      do {
        again = false;
        const p = await client.call("rules.history", { name, outcomes: kinds, limit: 100 }).catch(() => null);
        if (p && live) setItems((prev) => mergeDeliveries(prev, p.deliveries));
      } while (again && live);
      running = false;
    };
    const sub = client.subscribe(
      "events.subscribe",
      () => ({ filter: { types: on, sources: [] } }),
      // A dropped count means outcomes were left out of the live view: ask the log.
      (batch) => {
        if (batch.dropped > 0 || batch.outcomes.some((o) => o.rule === name)) void refresh();
      },
      { onSubscribed: ({ resumed }) => resumed && void refresh() },
    );
    return () => {
      live = false;
      sub.close();
    };
    // `tab` stands for `kinds`, `onKey` for `on`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, name, tab, onKey]);
  const more = async () => {
    if (!cursor || loading) return;
    setLoading(true);
    const p = await client.call("rules.history", { name, outcomes: kinds, limit: 100, cursor }).catch(() => null);
    if (p) {
      setItems((prev) => mergeDeliveries(prev, p.deliveries));
      setCursor(p.next_cursor ?? null);
    }
    setLoading(false);
  };
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => parent.current,
    estimateSize: () => 48,
    overscan: 6,
    getItemKey: (i) => items[i]?.id ?? i,
  });
  const last = virtualizer.getVirtualItems().at(-1)?.index ?? 0;
  useEffect(() => {
    if (cursor && !loading && last >= items.length - 5) void more();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [last, items.length, cursor, loading]);
  return (
    <Section
      title="History"
      actions={
        <div className="seg seg-sm" role="tablist">
          {OUTCOME_TABS.map((t) => (
            <button key={t.key} type="button" role="tab" aria-selected={t.key === tab} className={cx("seg-item", t.key === tab && "is-active")} onClick={() => setTab(t.key)}>
              {t.label}
            </button>
          ))}
        </div>
      }
    >
      <p className="faint">Every event this rule matched, and what was decided. Newest first.</p>
      {items.length === 0 ? (
        <p className="faint">{loading ? "Loading…" : "Nothing yet."}</p>
      ) : (
        <div ref={parent} className="history-scroll">
          <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((v) => {
              const d = items[v.index];
              if (!d) return null;
              return (
                <div key={v.key} data-index={v.index} ref={virtualizer.measureElement} className="vout">
                  <HistoryRow d={d} />
                </div>
              );
            })}
          </div>
        </div>
      )}
    </Section>
  );
}

function HistoryRow({ d }: { d: Delivery }): ReactNode {
  const tone = deliveryTone(d.outcome);
  return (
    <div className={cx("history-row", `tone-${tone}`)}>
      <span className="num faint">{dayAndTime(d.at, Date.now())}</span>
      <Pill tone={tone} {...(tone === "loop" ? { icon: "loop" as const } : {})}>
        {deliveryLabel(d.outcome)}
      </Pill>
      <a className="mono" href={href({ view: "feed", eventId: d.event })} onClick={(e) => { e.preventDefault(); navigate({ view: "feed", eventId: d.event }); }}>
        event {shortId(d.event)}
      </a>
      {d.run && (
        <a className="mono" href={href({ view: "runs", runId: d.run })} onClick={(e) => { e.preventDefault(); navigate({ view: "runs", runId: d.run ?? "" }); }}>
          run {shortId(d.run)}
        </a>
      )}
      {d.outcome.kind !== "fired" && <span className="history-why">{outcomeSentence(d.outcome, d.agent, (iso) => dayAndTime(iso, Date.now()))}</span>}
    </div>
  );
}

function RuleRuns({ name }: { name: string }) {
  const { items, loading } = useRunList({ status: [], rule: name });
  return (
    <Section
      title="Runs"
      actions={
        <a href={`/runs?rule=${encodeURIComponent(name)}`} onClick={(e) => { e.preventDefault(); navigate(`/runs?rule=${encodeURIComponent(name)}`); }}>
          All runs of this rule
        </a>
      }
    >
      {items.length === 0 ? (
        <p className="faint">{loading ? "Loading…" : "No runs yet."}</p>
      ) : (
        <ul className="mini-runs">
          {items.slice(0, 8).map((r) => (
            <li key={r.id} className="mini-run">
              <a href={href({ view: "runs", runId: r.id })} onClick={(e) => { e.preventDefault(); navigate({ view: "runs", runId: r.id }); }}>
                <Pill tone={runTone(r.status)}>{RUN_STATUS_LABEL[r.status]}</Pill>
                <span className="mono faint">{shortId(r.id)}</span>
                <Ago iso={r.created_at} className="faint" />
              </a>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

