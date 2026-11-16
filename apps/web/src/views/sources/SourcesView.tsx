import { useState, type ReactNode } from "react";

import { useApp, useDaemon } from "../../app/context";
import { useTildifyText } from "../../app/home";
import { useBindings } from "../../app/keys";
import { href, navigate } from "../../app/router";
import { attempt, toast } from "../../app/toasts";
import type { ConfigError, Expected, SourceConfig, SourceInfo } from "../../contracts";
import { expectedFor, refusedCurrent, staleOf } from "../../lib/edits";
import { sourceTone } from "../../lib/explain";
import { toSearchParams, EMPTY_FILTER } from "../../lib/filter";
import { count, dayAndTime, plural } from "../../lib/format";
import { parseDuration } from "../../lib/syntax";
import { ErrorCodes, RpcCallError } from "../../rpc/client";
import { ConflictNotice, hasConflict, useFirst } from "../common/Conflict";
import { Ago, Button, Confirm, Dl, Empty, Field, IconButton, Input, ListInput, Notice, Pill, Section, Select, Toggle, Trunc, cx } from "../../ui/primitives";

type Kind = SourceConfig["kind"];

function target(c: SourceConfig): string {
  switch (c.kind) {
    case "fs":
    case "git":
      return c.path;
    case "schedule":
      return c.cron ? `cron ${c.cron}${c.timezone ? ` (${c.timezone})` : ""}` : `every ${c.every ?? "?"}`;
    case "calendar":
      return c.url ?? c.path ?? "";
  }
}

export function SourcesView({ name, mode }: { name: string | null; mode: "show" | "edit" | "new" }) {
  const sources = useDaemon((s) => s.sources);
  const info = name ? (sources ?? []).find((s) => s.config.name === name) : undefined;
  if (mode === "new") return <SourceEditor opened={null} current={null} />;
  if (mode === "edit") return <EditSource key={name} name={name ?? ""} sources={sources} info={info} />;
  const list = sources ?? [];
  return (
    <div className={cx("split", name && "has-detail")}>
      <div className="split-main">
        <SourceList list={list} selected={name} loaded={sources !== null} />
      </div>
      {name && (info ? <SourceDetail key={name} info={info} /> : <aside className="detail"><div className="detail-body"><Notice tone="neutral" title={`No source named ${name}`}>It may have been removed.</Notice></div></aside>)}
    </div>
  );
}

/** The editor stays open (with what the user typed) if the source is removed meanwhile. */
function EditSource({ name, sources, info }: { name: string; sources: SourceInfo[] | null; info: SourceInfo | undefined }) {
  const opened = useFirst(info);
  if (!sources && !opened) return <Empty title="Loading…" />;
  if (!opened) return <Empty title={`No source named ${name}`} />;
  return <SourceEditor opened={opened} current={info ?? null} />;
}

function SourceList({ list, selected, loaded }: { list: SourceInfo[]; selected: string | null; loaded: boolean }) {
  const shortText = useTildifyText();
  const move = (d: number) => {
    const i = list.findIndex((s) => s.config.name === selected);
    const next = list[Math.max(0, Math.min(list.length - 1, i < 0 ? 0 : i + d))];
    if (next) navigate({ view: "sources", name: next.config.name, mode: "show" }, { replace: selected !== null });
  };
  useBindings([
    { keys: "j", label: "Next source", group: "Lists", run: () => move(1) },
    { keys: "k", label: "Previous source", group: "Lists", run: () => move(-1) },
    { keys: "escape", label: "Close the detail", group: "Lists", run: () => navigate({ view: "sources", name: null, mode: "show" }) },
  ]);
  const failing = list.filter((s) => s.health.state === "failed" || s.health.state === "degraded");
  return (
    <>
      <header className="view-head">
        <h1>Sources</h1>
        <span className="sub num">
          {plural(list.length, "source")}
          {failing.length > 0 && <span className="tone-text-attention"> · {failing.length} need attention</span>}
        </span>
        <div className="view-actions">
          <Button variant="primary" icon="plus" onClick={() => navigate({ view: "sources", name: null, mode: "new" })}>
            New source
          </Button>
        </div>
      </header>
      <div className="table-scroll">
        {loaded && list.length === 0 ? (
          <Empty title="Nothing is being watched">Add a source: a folder, a git repository, a schedule or a calendar.</Empty>
        ) : (
          <ul className="source-list" role="listbox" aria-label="Sources">
            {list.map((s) => {
              const tone = sourceTone(s.health.state);
              return (
                <li key={s.config.name} role="option" aria-selected={s.config.name === selected}>
                  <a
                    className={cx("source-row", s.config.name === selected && "is-selected")}
                    href={href({ view: "sources", name: s.config.name, mode: "show" })}
                    onClick={(e) => {
                      e.preventDefault();
                      navigate({ view: "sources", name: s.config.name, mode: "show" }, { replace: selected !== null });
                    }}
                  >
                    <span className="source-line">
                      <span className={cx("dot", `tone-${tone}`)} />
                      <span className="mono source-name">{s.config.name}</span>
                      <Pill tone="neutral">{s.config.kind}</Pill>
                      <span className="source-target">
                        <Trunc text={target(s.config)} />
                      </span>
                      <Pill tone={tone}>{s.health.state}</Pill>
                      <span className="faint num source-last">{s.health.last_event_at ? <Ago iso={s.health.last_event_at} /> : "no events yet"}</span>
                    </span>
                    {s.health.message && (s.health.state === "failed" || s.health.state === "degraded") && <span className={cx("source-msg", `tone-${tone}`)}>{shortText(s.health.message)}</span>}
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

function SourceDetail({ info }: { info: SourceInfo }) {
  const { client } = useApp();
  const [confirm, setConfirm] = useState(false);
  const c = info.config;
  const h = info.health;
  const shortText = useTildifyText();
  const tone = sourceTone(h.state);
  // Changes `enabled` of the source as it is in its file, not this cached copy of it.
  const toggle = () => void attempt(c.enabled ? "Disable" : "Enable", () => client.call("sources.set_enabled", { name: c.name, enabled: !c.enabled }), c.enabled ? `Disabled ${c.name}` : `Enabled ${c.name}`);
  const settings: [string, ReactNode][] = [["Kind", <span key="k" className="mono">{c.kind}</span>]];
  switch (c.kind) {
    case "fs":
      settings.push(["Path", <Trunc key="p" text={c.path} copy />], ["Recursive", c.recursive ?? true ? "yes" : "no"], ["Quiet period", <span key="q" className="mono">{c.quiet ?? "2s (default)"}</span>], ["Burst threshold", <span key="b" className="num">{c.burst_threshold ?? "200 (default)"}</span>]);
      if (c.ignore?.length) settings.push(["Also ignores", <span key="i" className="mono">{c.ignore.join("  ")}</span>]);
      break;
    case "git":
      settings.push(["Repository", <Trunc key="p" text={c.path} copy />]);
      break;
    case "schedule":
      settings.push(c.cron ? ["Cron", <span key="c" className="mono">{c.cron}</span>] : ["Every", <span key="e" className="mono">{c.every}</span>]);
      if (c.timezone) settings.push(["Time zone", c.timezone]);
      break;
    case "calendar":
      settings.push([c.url ? "Feed URL" : "File", <Trunc key="u" text={c.url ?? c.path ?? ""} copy />], ["Refresh", <span key="r" className="mono">{c.refresh ?? "15m (default)"}</span>], ["Lead time", <span key="l" className="mono">{c.lead ?? "10m (default)"}</span>]);
      break;
  }
  const feedHref = `/feed?${toSearchParams({ ...EMPTY_FILTER, sources: [c.name] }).toString()}`;
  return (
    <aside className="detail" aria-label={`Source ${c.name}`}>
      <header className="detail-head">
        <IconButton icon="back" label="Back to sources" className="only-narrow" onClick={() => navigate({ view: "sources", name: null, mode: "show" })} size="sm" variant="ghost" />
        <span className={cx("dot", `tone-${tone}`)} />
        <h2 className="detail-title mono">{c.name}</h2>
        <div className="detail-head-actions">
          <Button size="sm" onClick={toggle}>{c.enabled ? "Disable" : "Enable"}</Button>
          <Button size="sm" icon="edit" onClick={() => navigate({ view: "sources", name: c.name, mode: "edit" })}>Edit</Button>
          <Button size="sm" variant="danger" icon="trash" onClick={() => setConfirm(true)}>Remove</Button>
          <IconButton icon="x" label="Close (Esc)" onClick={() => navigate({ view: "sources", name: null, mode: "show" })} size="sm" variant="ghost" className="hide-narrow" />
        </div>
      </header>
      <div className="detail-body">
        <Notice tone={tone} title={<>{h.state[0]?.toUpperCase()}{h.state.slice(1)} <span className="faint num">since {dayAndTime(h.since, Date.now())}</span></>}>
          {(h.message && shortText(h.message)) ?? (h.state === "healthy" ? "Watching. Changes become events after the source's quiet period." : h.state === "disabled" ? "Not watching. Enable it to start again." : "")}
        </Notice>
        {h.last_error && (
          <Section title="Last error">
            <p className="num faint">{dayAndTime(h.last_error.at, Date.now())}</p>
            <pre className="code">{shortText(h.last_error.message)}</pre>
          </Section>
        )}
        <Dl
          items={[
            ["Events", <span key="e" className="num">{count(h.events_emitted)} since the daemon started</span>],
            ["Last event", h.last_event_at ? <Ago key="l" iso={h.last_event_at} /> : <span key="l" className="faint">none yet</span>],
          ]}
        />
        <div>
          <a href={feedHref} onClick={(e) => { e.preventDefault(); navigate(feedHref); }}>Show its events in the feed</a>
        </div>
        <Section title="Settings">
          <Dl items={settings} />
        </Section>
        <Section title="Defined in">
          <Trunc text={info.origin.path} copy />
        </Section>
      </div>
      <Confirm
        open={confirm}
        onClose={() => setConfirm(false)}
        title={`Remove ${c.name}?`}
        confirm="Remove"
        danger
        onConfirm={async () => {
          const ok = await attempt("Remove", () => client.call("sources.delete", { name: c.name, expected: { kind: "revision", revision: info.revision } }), `Removed ${c.name}`);
          if (ok) navigate({ view: "sources", name: null, mode: "show" });
        }}
      >
        <p>Pulse stops watching and the source is removed from <span className="mono">{info.origin.path}</span>. Events it already recorded stay in the log. To stop it for a while, disable it instead.</p>
      </Confirm>
    </aside>
  );
}

interface SourceForm {
  name: string;
  project: string | null;
  kind: Kind;
  enabled: boolean;
  path: string;
  ignore: string[];
  recursive: boolean;
  quiet: string;
  burst: string;
  scheduleMode: "cron" | "every";
  cron: string;
  every: string;
  timezone: string;
  calMode: "url" | "path";
  url: string;
  refresh: string;
  lead: string;
}

function toForm(info: SourceInfo | null): SourceForm {
  const f: SourceForm = { name: "", project: null, kind: "fs", enabled: true, path: "", ignore: [], recursive: true, quiet: "", burst: "", scheduleMode: "cron", cron: "", every: "", timezone: "", calMode: "url", url: "", refresh: "", lead: "" };
  if (!info) return f;
  const c = info.config;
  f.project = info.origin.kind === "project" ? info.origin.name : null;
  f.name = f.project ? c.name.slice(f.project.length + 1) : c.name;
  f.kind = c.kind;
  f.enabled = c.enabled;
  if (c.kind === "fs") Object.assign(f, { path: c.path, ignore: [...(c.ignore ?? [])], recursive: c.recursive ?? true, quiet: c.quiet ?? "", burst: c.burst_threshold?.toString() ?? "" });
  if (c.kind === "git") f.path = c.path;
  if (c.kind === "schedule") Object.assign(f, { scheduleMode: c.every ? "every" : "cron", cron: c.cron ?? "", every: c.every ?? "", timezone: c.timezone ?? "" });
  if (c.kind === "calendar") Object.assign(f, { calMode: c.path ? "path" : "url", url: c.url ?? "", path: c.path ?? "", refresh: c.refresh ?? "", lead: c.lead ?? "" });
  return f;
}

function fromForm(f: SourceForm): { source: SourceConfig; problems: Record<string, string> } {
  const problems: Record<string, string> = {};
  if (!/^[a-z0-9][a-z0-9_-]{0,63}$/.test(f.name)) problems.name = "Lowercase letters, digits, - or _.";
  const dur = (k: string, v: string) => {
    if (v && parseDuration(v) === null) problems[k] = "Use a duration like 2s or 15m.";
  };
  const base = { name: f.name, enabled: f.enabled };
  let source: SourceConfig;
  switch (f.kind) {
    case "fs":
      if (!f.path.trim()) problems.path = "Which directory?";
      dur("quiet", f.quiet);
      if (f.burst && !/^\d+$/.test(f.burst)) problems.burst = "A whole number.";
      source = { ...base, kind: "fs", path: f.path.trim(), ignore: f.ignore, recursive: f.recursive, ...(f.quiet ? { quiet: f.quiet } : {}), ...(f.burst ? { burst_threshold: Number(f.burst) } : {}) };
      break;
    case "git":
      if (!f.path.trim()) problems.path = "Which repository?";
      source = { ...base, kind: "git", path: f.path.trim() };
      break;
    case "schedule":
      if (f.scheduleMode === "cron" && f.cron.trim().split(/\s+/).length !== 5) problems.cron = "Five fields: minute hour day-of-month month day-of-week.";
      if (f.scheduleMode === "every") dur("every", f.every);
      if (f.scheduleMode === "every" && !f.every) problems.every = "How often?";
      source = { ...base, kind: "schedule", ...(f.scheduleMode === "cron" ? { cron: f.cron.trim() } : { every: f.every.trim() }), ...(f.timezone ? { timezone: f.timezone } : {}) };
      break;
    case "calendar":
      if (f.calMode === "url" && !/^https?:\/\//.test(f.url)) problems.url = "An http:// or https:// ICS feed.";
      if (f.calMode === "path" && !f.path) problems.path = "Which .ics file?";
      dur("refresh", f.refresh);
      dur("lead", f.lead);
      source = { ...base, kind: "calendar", ...(f.calMode === "url" ? { url: f.url.trim() } : { path: f.path.trim() }), ...(f.refresh ? { refresh: f.refresh } : {}), ...(f.lead ? { lead: f.lead } : {}) };
      break;
  }
  return { source, problems };
}

function SourceEditor({ opened, current }: { opened: SourceInfo | null; current: SourceInfo | null }) {
  const existing = opened;
  const { client } = useApp();
  const projects = useDaemon((s) => s.projects) ?? [];
  const [form, setForm] = useState<SourceForm>(() => toForm(existing));
  const [touched, setTouched] = useState(false);
  const [serverErrors, setServerErrors] = useState<ConfigError[]>([]);
  const [base, setBase] = useState<SourceInfo | null>(opened);
  const [refused, setRefused] = useState<{ current: SourceInfo | null } | null>(null);
  const live = opened ? (refused && current?.revision === base?.revision ? refused.current : current) : (refused?.current ?? null);
  const conflict = hasConflict(base, live);
  const fullSourceName = form.project ? `${form.project}/${form.name}` : form.name;
  const set = <K extends keyof SourceForm>(k: K, v: SourceForm[K]) => setForm((f) => ({ ...f, [k]: v }));
  const { source, problems } = fromForm(form);
  const err = (k: string) => serverErrors.find((e) => e.field === k)?.message ?? (touched ? problems[k] : undefined);
  const back = () => navigate({ view: "sources", name: existing?.config.name ?? null, mode: "show" });
  const save = async (expected: Expected = expectedFor(base)) => {
    setTouched(true);
    if (Object.keys(problems).length) return;
    setServerErrors([]);
    try {
      const info = await client.call("sources.upsert", { source, ...(form.project ? { project: form.project } : {}), expected });
      toast(existing ? `Saved ${info.config.name}` : `Added ${info.config.name}`, "ok", `Written to ${info.origin.path}`);
      navigate({ view: "sources", name: info.config.name, mode: "show" }, { replace: true });
    } catch (e) {
      const stale = staleOf(e);
      if (stale) {
        const now = await client.call("sources.list", {}).catch(() => null);
        setRefused({ current: refusedCurrent(stale, now?.sources.find((x) => x.config.name === fullSourceName) ?? null) });
      } else if (e instanceof RpcCallError && e.code === ErrorCodes.ConfigInvalid) setServerErrors((e.data as { errors?: ConfigError[] })?.errors ?? []);
      else toast("Save failed", "error", e instanceof Error ? e.message : String(e));
    }
  };
  useBindings([
    { keys: "mod+s", label: "Save the source", group: "Editor", run: () => void save() },
    { keys: "escape", label: "Cancel editing", group: "Editor", run: back },
  ]);
  return (
    <form className="editor single" onSubmit={(e) => { e.preventDefault(); void save(); }} aria-label={existing ? `Edit source ${existing.config.name}` : "New source"}>
      <header className="view-head editor-head">
        <IconButton icon="back" label="Back" variant="ghost" size="sm" onClick={back} />
        <h1>{existing ? `Edit ${existing.config.name}` : "New source"}</h1>
        <div className="view-actions">
          <Button onClick={back} shortcut="escape">Cancel</Button>
          <Button type="submit" variant="primary" shortcut="mod+s">{existing ? "Save" : "Add source"}</Button>
        </div>
      </header>
      <div className="editor-body narrow-form">
        {conflict && (
          <ConflictNotice
            what="source"
            name={fullSourceName}
            base={base}
            current={live}
            onReload={() => {
              if (!live) return;
              setForm(toForm(live));
              setBase(live);
              setRefused(null);
            }}
            onOverwrite={(expected) => void save(expected)}
          />
        )}
        {serverErrors.length > 0 && (
          <Notice tone="error" title="The daemon did not accept this source">
            <ul className="plain">{serverErrors.map((e, i) => <li key={i}>{e.field && <code>{e.field}</code>} {e.message}</li>)}</ul>
          </Notice>
        )}
        {!existing && (
          <Field label="Saved in">
            <Select value={form.project ?? ""} onChange={(e) => set("project", e.target.value || null)}>
              <option value="">Your pulse.toml</option>
              {projects.filter((p) => p.state === "trusted").map((p) => <option key={p.name} value={p.name}>Project {p.name}</option>)}
            </Select>
          </Field>
        )}
        <Field label="Name" error={err("name")}>
          <Input mono value={form.name} disabled={existing !== null} onChange={(e) => set("name", e.target.value)} placeholder="university" autoFocus />
        </Field>
        <Field label="Kind">
          <Select value={form.kind} disabled={existing !== null} onChange={(e) => set("kind", e.target.value as Kind)}>
            <option value="fs">fs: a folder</option>
            <option value="git">git: a repository</option>
            <option value="schedule">schedule: a time</option>
            <option value="calendar">calendar: an ICS feed or file</option>
          </Select>
        </Field>
        {(form.kind === "fs" || form.kind === "git") && (
          <Field label={form.kind === "fs" ? "Directory" : "Repository"} error={err("path")} hint={form.project ? "Relative paths resolve against the project directory." : "Absolute, or starting with ~."}>
            <Input mono value={form.path} onChange={(e) => set("path", e.target.value)} placeholder={form.kind === "fs" ? "~/University" : "~/code/pulse"} />
          </Field>
        )}
        {form.kind === "fs" && (
          <>
            <Field label="Also ignore" hint="Globs relative to the directory, added to the built-in ignores (.git, node_modules…).">
              <ListInput values={form.ignore} onChange={(v) => set("ignore", v)} placeholder="**/build/**" />
            </Field>
            <Field label="Subfolders"><Toggle checked={form.recursive} onChange={(v) => set("recursive", v)} label="Watch subfolders too" /></Field>
            <Field label="Quiet period" error={err("quiet")} hint="A file must stop changing for this long before it becomes an event. Default 2s."><Input mono className="w-short" value={form.quiet} onChange={(e) => set("quiet", e.target.value)} placeholder="2s" /></Field>
            <Field label="Burst threshold" error={err("burst")} hint="Past this many pending changes in one directory, they fold into one fs.burst event. Default 200."><Input mono className="w-short" value={form.burst} onChange={(e) => set("burst", e.target.value)} placeholder="200" /></Field>
          </>
        )}
        {form.kind === "schedule" && (
          <>
            <Field label="When">
              <Select value={form.scheduleMode} onChange={(e) => set("scheduleMode", e.target.value as SourceForm["scheduleMode"])}>
                <option value="cron">at times (cron)</option>
                <option value="every">every interval</option>
              </Select>
            </Field>
            {form.scheduleMode === "cron" ? (
              <>
                <Field label="Cron" error={err("cron")} hint="minute hour day-of-month month day-of-week. 0 9 * * 1-5 is 9:00 on weekdays."><Input mono value={form.cron} onChange={(e) => set("cron", e.target.value)} placeholder="0 9 * * 1-5" /></Field>
                <Field label="Time zone" hint="IANA name; default is the system zone."><Input mono value={form.timezone} onChange={(e) => set("timezone", e.target.value)} placeholder="Europe/Rome" /></Field>
              </>
            ) : (
              <Field label="Every" error={err("every")}><Input mono className="w-short" value={form.every} onChange={(e) => set("every", e.target.value)} placeholder="1h" /></Field>
            )}
          </>
        )}
        {form.kind === "calendar" && (
          <>
            <Field label="From">
              <Select value={form.calMode} onChange={(e) => set("calMode", e.target.value as SourceForm["calMode"])}>
                <option value="url">an ICS feed URL</option>
                <option value="path">a local .ics file</option>
              </Select>
            </Field>
            {form.calMode === "url" ? (
              <Field label="URL" error={err("url")}><Input mono value={form.url} onChange={(e) => set("url", e.target.value)} placeholder="https://…/calendar.ics" /></Field>
            ) : (
              <Field label="File" error={err("path")}><Input mono value={form.path} onChange={(e) => set("path", e.target.value)} placeholder="~/calendar.ics" /></Field>
            )}
            <Field label="Refresh" error={err("refresh")} hint="How often to re-read it. Default 15m."><Input mono className="w-short" value={form.refresh} onChange={(e) => set("refresh", e.target.value)} placeholder="15m" /></Field>
            <Field label="Lead time" error={err("lead")} hint="How long before an event calendar.event.starting fires. Default 10m."><Input mono className="w-short" value={form.lead} onChange={(e) => set("lead", e.target.value)} placeholder="10m" /></Field>
          </>
        )}
        <Field label="State"><Toggle checked={form.enabled} onChange={(v) => set("enabled", v)} label="Enabled" /></Field>
      </div>
    </form>
  );
}
