import { useState, type ReactNode } from "react";

import { useApp, useDaemon } from "../../app/context";
import { useBindings } from "../../app/keys";
import { href, navigate } from "../../app/router";
import { attempt, toast } from "../../app/toasts";
import type { AgentConfig, AgentInfo, ConfigError, EventTypeInfo, Expected, McpCredentials } from "../../contracts";
import { expectedFor, refusedCurrent, staleOf } from "../../lib/edits";
import { ADAPTERS, agentToForm, emptyAgentForm, formToAgent, type AgentForm } from "../../lib/agentForm";
import { plural } from "../../lib/format";
import { parseScope, typeGlobMatches } from "../../lib/syntax";
import { ErrorCodes, RpcCallError } from "../../rpc/client";
import { Icon } from "../../ui/icons";
import { ConflictNotice, hasConflict, useFirst } from "../common/Conflict";
import { Button, Confirm, CopyButton, Dl, Empty, Field, IconButton, Input, ListInput, Notice, Pill, Section, Select, Trunc, cx } from "../../ui/primitives";

const ADAPTER_NOTE: Record<AgentConfig["adapter"], string> = {
  "claude-code": "Runs Claude Code headless (claude -p) and reads its stream-json output.",
  codex: "Runs Codex (codex exec) and reads its JSON events.",
  command: "Runs a program with arguments. Never through a shell.",
  mcp: "Not run by Pulse. An identity with scopes for an MCP client.",
};

export function AgentsView({ name, mode }: { name: string | null; mode: "show" | "edit" | "new" }) {
  const agents = useDaemon((s) => s.agents);
  const info = name ? (agents ?? []).find((a) => a.config.name === name) : undefined;
  if (mode === "new") return <AgentEditor opened={null} current={null} />;
  if (mode === "edit") return <EditAgent key={name} name={name ?? ""} agents={agents} info={info} />;
  return (
    <div className={cx("split", name && "has-detail", "rules-split")}>
      <div className="split-main">
        <AgentList selected={name} agents={agents} />
      </div>
      {name &&
        (info ? (
          <AgentDetail key={name} info={info} />
        ) : (
          <aside className="detail">
            <div className="detail-body">
              <Notice tone="neutral" title={`No agent named ${name}`}>It may have been deleted.</Notice>
            </div>
          </aside>
        ))}
    </div>
  );
}

/** The editor stays open (with what the user typed) if the agent is removed meanwhile. */
function EditAgent({ name, agents, info }: { name: string; agents: AgentInfo[] | null; info: AgentInfo | undefined }) {
  const opened = useFirst(info);
  if (!agents && !opened) return <Empty title="Loading…" />;
  if (!opened) return <Empty title={`No agent named ${name}`} />;
  return <AgentEditor opened={opened} current={info ?? null} />;
}

function AgentList({ selected, agents }: { selected: string | null; agents: AgentInfo[] | null }) {
  const list = agents ?? [];
  const move = (d: number) => {
    const i = list.findIndex((a) => a.config.name === selected);
    const next = list[Math.max(0, Math.min(list.length - 1, i < 0 ? 0 : i + d))];
    if (next) navigate({ view: "agents", name: next.config.name, mode: "show" }, { replace: selected !== null });
  };
  useBindings([
    { keys: "j", label: "Next agent", group: "Lists", run: () => move(1) },
    { keys: "k", label: "Previous agent", group: "Lists", run: () => move(-1) },
    { keys: "escape", label: "Close the detail", group: "Lists", run: () => navigate({ view: "agents", name: null, mode: "show" }) },
  ]);
  return (
    <>
      <header className="view-head">
        <h1>Agents</h1>
        <span className="sub num">{plural(list.length, "agent")}</span>
        <div className="view-actions">
          <Button variant="primary" icon="plus" onClick={() => navigate({ view: "agents", name: null, mode: "new" })}>
            New agent
          </Button>
        </div>
      </header>
      <div className="table-scroll">
        {agents && list.length === 0 ? (
          <Empty title="No agents yet">An agent is a named identity with an adapter (how Pulse runs it) and scopes (what it may see).</Empty>
        ) : (
          <ul className="rule-list" role="listbox" aria-label="Agents">
            {list.map((a) => (
              <li key={a.config.name} role="option" aria-selected={a.config.name === selected}>
                <a
                  className={cx("rule-row", "agent-row", a.config.name === selected && "is-selected")}
                  href={href({ view: "agents", name: a.config.name, mode: "show" })}
                  onClick={(e) => {
                    e.preventDefault();
                    navigate({ view: "agents", name: a.config.name, mode: "show" }, { replace: selected !== null });
                  }}
                >
                  <span className={cx("dot", a.running ? "tone-running" : a.queued ? "tone-waiting" : "tone-neutral")} title={a.running ? "running" : "idle"} />
                  <span className="rule-name mono">{a.config.name}</span>
                  <span className="rule-on">
                    <Pill tone="neutral">{a.config.adapter}</Pill>
                  </span>
                  <span className="rule-agent mono" title={(a.config.allow ?? []).join("\n")}>
                    {(a.config.allow ?? []).length ? (a.config.allow ?? []).join("  ") : "sees nothing"}
                  </span>
                  <span className="rule-tags num">
                    {a.running > 0 && <Pill tone="running">{a.running} running</Pill>}
                    {a.queued > 0 && <Pill tone="waiting">{a.queued} queued</Pill>}
                  </span>
                  <span className="rule-last faint">{a.origin.kind === "project" ? `project ${a.origin.name}` : ""}</span>
                </a>
              </li>
            ))}
          </ul>
        )}
      </div>
    </>
  );
}

/** For each catalog type, whether the scopes cover it and under which paths. */
export function scopeCoverage(allow: string[], types: EventTypeInfo[]): { type: EventTypeInfo; anywhere: boolean; paths: string[] }[] {
  return types.map((t) => {
    let anywhere = false;
    const paths: string[] = [];
    for (const s of allow) {
      const { types: glob, path, problem } = parseScope(s);
      if (problem || !typeGlobMatches(glob, t.type)) continue;
      if (path === null) anywhere = true;
      else paths.push(path);
    }
    return { type: t, anywhere, paths };
  });
}

export function ScopeExplainer({ allow }: { allow: string[] }) {
  const types = useDaemon((s) => s.eventTypes) ?? [];
  const rows = scopeCoverage(allow, types);
  const seen = rows.filter((r) => r.anywhere || r.paths.length > 0);
  const unseen = rows.filter((r) => !r.anywhere && r.paths.length === 0);
  return (
    <div className="scopes">
      {allow.length === 0 ? (
        <Notice tone="attention" title="This agent can see nothing">
          Scopes are default deny. Add one like <code>fs.*:~/University/**</code> or <code>calendar.*</code>.
        </Notice>
      ) : (
        <table className="scope-table">
          <tbody>
            {seen.map((r) => (
              <tr key={r.type.type}>
                <td className="mono">{r.type.type}</td>
                <td>
                  {r.anywhere ? (
                    <span>all of them</span>
                  ) : (
                    <span>
                      only when every path it carries is under{" "}
                      {r.paths.map((p, i) => (
                        <span key={p}>
                          {i > 0 && " or "}
                          <code>{p}</code>
                        </span>
                      ))}
                      {r.type.path_fields.length === 0 && <span className="tone-text-attention"> (this type carries no paths, so never)</span>}
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {unseen.length > 0 && (
        <p className="faint">
          Never sees: {unseen.map((r) => r.type.type).join(", ")}.
        </p>
      )}
      <p className="faint">
        Paths are compared as text: symlinks are not resolved, and paths with <code>..</code> are never in scope. Scopes decide what the agent is <em>told</em> (by rules and over MCP); they do not sandbox what it can do once running. That is the agent runtime's job (permission mode, sandbox).
      </p>
    </div>
  );
}

function AgentDetail({ info }: { info: AgentInfo }) {
  const { client } = useApp();
  const rules = useDaemon((s) => s.rules) ?? [];
  const [confirm, setConfirm] = useState(false);
  const a = info.config;
  const users = rules.filter((r) => {
    const proj = r.origin.kind === "project" ? r.origin.name : null;
    const resolved = proj && a.name === `${proj}/${r.config.run.agent}` ? a.name : r.config.run.agent;
    return resolved === a.name;
  });
  const settings: [string, ReactNode][] = [];
  switch (a.adapter) {
    case "claude-code":
      settings.push(["Program", <span key="p" className="mono">{a.program ?? "claude (from PATH)"}</span>]);
      if (a.model) settings.push(["Model", <span key="m" className="mono">{a.model}</span>]);
      settings.push(["Permission mode", <span key="pm" className="mono">{a.permission_mode ?? "default"}</span>]);
      if (a.allowed_tools?.length) settings.push(["Allowed tools", <span key="t" className="mono">{a.allowed_tools.join(", ")}</span>]);
      if (a.extra_args?.length) settings.push(["Extra arguments", <span key="x" className="mono">{a.extra_args.join(" ")}</span>]);
      break;
    case "codex":
      settings.push(["Program", <span key="p" className="mono">{a.program ?? "codex (from PATH)"}</span>]);
      if (a.model) settings.push(["Model", <span key="m" className="mono">{a.model}</span>]);
      settings.push(["Sandbox", <span key="s" className="mono">{a.sandbox ?? "default"}</span>]);
      if (a.extra_args?.length) settings.push(["Extra arguments", <span key="x" className="mono">{a.extra_args.join(" ")}</span>]);
      break;
    case "command":
      settings.push([
        "Command",
        <ol key="c" className="argv">
          {(a.command ?? []).map((x, i) => (
            <li key={i} className="mono">
              {x}
            </li>
          ))}
        </ol>,
      ]);
      settings.push(["Stdin", <span key="s" className="mono">{a.stdin ?? "prompt"}</span>]);
      break;
    case "mcp":
      break;
  }
  if (a.adapter !== "mcp") {
    settings.push(["Working directory", a.cwd ? <Trunc key="c" text={a.cwd} /> : <span key="c" className="faint">the Pulse home's runs/ directory</span>]);
    settings.push(["Timeout", <span key="t" className="num">{a.timeout}</span>]);
    settings.push(["Concurrency", <span key="m" className="num">{a.max_concurrent} at once, {a.max_queued} more may wait</span>]);
  }
  const envKeys = Object.keys(a.env ?? {});
  if (envKeys.length) settings.push(["Environment", <span key="e" className="mono">{envKeys.join(", ")} <span className="faint">(values hidden)</span></span>]);

  return (
    <aside className="detail" aria-label={`Agent ${a.name}`}>
      <header className="detail-head">
        <IconButton icon="back" label="Back to agents" className="only-narrow" onClick={() => navigate({ view: "agents", name: null, mode: "show" })} size="sm" variant="ghost" />
        <h2 className="detail-title mono">{a.name}</h2>
        <Pill tone="neutral">{a.adapter}</Pill>
        <div className="detail-head-actions">
          <Button size="sm" icon="edit" onClick={() => navigate({ view: "agents", name: a.name, mode: "edit" })}>
            Edit
          </Button>
          <Button size="sm" variant="danger" icon="trash" onClick={() => setConfirm(true)}>
            Delete
          </Button>
          <IconButton icon="x" label="Close (Esc)" onClick={() => navigate({ view: "agents", name: null, mode: "show" })} size="sm" variant="ghost" className="hide-narrow" />
        </div>
      </header>
      <div className="detail-body">
        <p className="muted">{ADAPTER_NOTE[a.adapter]}</p>
        <Section title="What it can see">
          <ScopeExplainer allow={a.allow ?? []} />
        </Section>
        {settings.length > 0 && (
          <Section title="How it runs">
            <Dl items={settings} />
          </Section>
        )}
        <Section title={`Woken by ${plural(users.length, "rule")}`}>
          {users.length === 0 ? (
            <p className="faint">{a.adapter === "mcp" ? "Rules cannot wake MCP agents." : "No rule wakes this agent yet."}</p>
          ) : (
            <ul className="link-list">
              {users.map((r) => (
                <li key={r.config.name}>
                  <a className="mono" href={href({ view: "rules", name: r.config.name, mode: "show" })} onClick={(e) => { e.preventDefault(); navigate({ view: "rules", name: r.config.name, mode: "show" }); }}>
                    {r.config.name}
                  </a>{" "}
                  <span className="faint mono">{r.config.on.join(", ")}</span>
                  {r.config.archived && <span className="faint"> (archived)</span>}
                </li>
              ))}
            </ul>
          )}
          {a.adapter !== "mcp" && (
            <p>
              <a href={`/runs?agent=${encodeURIComponent(a.name)}`} onClick={(e) => { e.preventDefault(); navigate(`/runs?agent=${encodeURIComponent(a.name)}`); }}>
                Runs of this agent
              </a>
              {info.running + info.queued > 0 && <span className="faint num"> · {info.running} running, {info.queued} queued</span>}
            </p>
          )}
        </Section>
        <McpSection name={a.name} isMcp={a.adapter === "mcp"} />
        <Section title="Defined in">
          <Trunc text={info.origin.path} copy />
        </Section>
      </div>
      <Confirm
        open={confirm}
        onClose={() => setConfirm(false)}
        title={`Delete ${a.name}?`}
        confirm="Delete"
        danger
        onConfirm={async () => {
          const ok = await attempt("Delete", () => client.call("agents.delete", { name: a.name, expected: { kind: "revision", revision: info.revision } }), `Deleted ${a.name}`);
          if (ok) navigate({ view: "agents", name: null, mode: "show" });
        }}
      >
        <p>
          The agent is removed from <span className="mono">{info.origin.path}</span>. Rules that wake it must be changed first; the daemon refuses otherwise. Past runs keep their snapshot.
        </p>
      </Confirm>
    </aside>
  );
}

function McpSection({ name, isMcp }: { name: string; isMcp: boolean }) {
  const { client } = useApp();
  const [creds, setCreds] = useState<McpCredentials | null>(null);
  const [reveal, setReveal] = useState(false);
  const load = async () => {
    const c = await attempt("Load MCP credentials", () => client.call("agents.mcp_credentials", { name }));
    if (c) setCreds(c);
  };
  const config = creds ? JSON.stringify({ mcpServers: { pulse: { type: "http", url: creds.url, headers: { Authorization: `Bearer ${creds.token}` } } } }, null, 2) : "";
  return (
    <Section title="MCP connection">
      <p className="muted">
        {isMcp ? "An MCP client uses these to act as this agent." : "This agent can also query and subscribe to events over MCP."} The token only works on <code>/mcp</code> and only as {name}: it cannot approve runs or change rules, and it only sees events the scopes above allow.
      </p>
      {!creds ? (
        <div>
          <Button size="sm" icon="key" onClick={() => void load()}>
            Show connection details
          </Button>
        </div>
      ) : (
        <>
          <Dl
            items={[
              ["URL", <span key="u" className="mono">{creds.url} <CopyButton value={creds.url} /></span>],
              [
                "Token",
                <span key="t" className="mono">
                  {reveal ? creds.token : "•".repeat(Math.min(24, creds.token.length))}{" "}
                  <button type="button" className="link-btn" onClick={() => setReveal((v) => !v)}>
                    {reveal ? "hide" : "reveal"}
                  </button>
                  <CopyButton value={creds.token} label="Copy token" />
                </span>,
              ],
              ["Over stdio", <span key="s" className="mono">pulse mcp --agent {name} <CopyButton value={`pulse mcp --agent ${name}`} /></span>],
            ]}
          />
          <div className="snippet">
            <div className="snippet-head">
              <span className="faint">Client config (streamable HTTP)</span>
              <CopyButton value={config} label="Copy config" />
            </div>
            <pre className="code">{reveal ? config : config.replace(creds.token, "•".repeat(12))}</pre>
          </div>
        </>
      )}
    </Section>
  );
}

function AgentEditor({ opened, current }: { opened: AgentInfo | null; current: AgentInfo | null }) {
  const existing = opened;
  const { client } = useApp();
  const projects = useDaemon((s) => s.projects) ?? [];
  const [form, setForm] = useState<AgentForm>(() => (existing ? agentToForm(existing) : emptyAgentForm()));
  const [touched, setTouched] = useState(false);
  const [serverErrors, setServerErrors] = useState<ConfigError[]>([]);
  const [saving, setSaving] = useState(false);
  const [base, setBase] = useState<AgentInfo | null>(opened);
  const [refused, setRefused] = useState<{ current: AgentInfo | null } | null>(null);
  const live = opened ? (refused && current?.revision === base?.revision ? refused.current : current) : (refused?.current ?? null);
  const conflict = hasConflict(base, live);
  const fullAgentName = form.project ? `${form.project}/${form.name}` : form.name;
  const set = <K extends keyof AgentForm>(k: K, v: AgentForm[K]) => setForm((f) => ({ ...f, [k]: v }));
  const { agent, problems } = formToAgent(form);
  const err = (field: string) => serverErrors.find((e) => e.field === field || e.field?.startsWith(`${field}[`))?.message ?? (touched ? problems.find((p) => p.field === field)?.message : undefined);
  const trusted = projects.filter((p) => p.state === "trusted");
  const back = () => navigate({ view: "agents", name: existing?.config.name ?? null, mode: "show" });
  const save = async (expected: Expected = expectedFor(base)) => {
    setTouched(true);
    if (problems.length) {
      toast("Fix the highlighted fields first", "error");
      return;
    }
    setSaving(true);
    setServerErrors([]);
    try {
      const info = await client.call("agents.upsert", { agent, ...(form.project ? { project: form.project } : {}), expected });
      toast(existing ? `Saved ${info.config.name}` : `Created ${info.config.name}`, "ok", `Written to ${info.origin.path}`);
      navigate({ view: "agents", name: info.config.name, mode: "show" }, { replace: true });
    } catch (e) {
      const stale = staleOf(e);
      if (stale) setRefused({ current: refusedCurrent(stale, await client.call("agents.get", { name: fullAgentName }).catch(() => null)) });
      else if (e instanceof RpcCallError && e.code === ErrorCodes.ConfigInvalid) setServerErrors((e.data as { errors?: ConfigError[] })?.errors ?? []);
      else toast("Save failed", "error", e instanceof Error ? e.message : String(e));
    }
    setSaving(false);
  };
  useBindings([
    { keys: "mod+s", label: "Save the agent", group: "Editor", run: () => void save() },
    { keys: "escape", label: "Cancel editing", group: "Editor", run: back },
  ]);
  return (
    <div className="editor-layout">
      <form className="editor" onSubmit={(e) => { e.preventDefault(); void save(); }} aria-label={existing ? `Edit agent ${existing.config.name}` : "New agent"}>
        <header className="view-head editor-head">
          <IconButton icon="back" label="Back" variant="ghost" size="sm" onClick={back} />
          <h1>{existing ? `Edit ${existing.config.name}` : "New agent"}</h1>
          <div className="view-actions">
            <Button onClick={back} shortcut="escape">Cancel</Button>
            <Button type="submit" variant="primary" disabled={saving} shortcut="mod+s">{saving ? "Saving…" : existing ? "Save" : "Create agent"}</Button>
          </div>
        </header>
        <div className="editor-body">
          {conflict && (
            <ConflictNotice
              what="agent"
              name={fullAgentName}
              base={base}
              current={live}
              onReload={() => {
                if (!live) return;
                setForm(agentToForm(live));
                setBase(live);
                setRefused(null);
              }}
              onOverwrite={(expected) => void save(expected)}
            />
          )}
          {serverErrors.length > 0 && (
            <Notice tone="error" title="The daemon did not accept this agent">
              <ul className="plain">{serverErrors.map((e, i) => <li key={i}>{e.field && <code>{e.field}</code>} {e.message}</li>)}</ul>
            </Notice>
          )}
          <fieldset className="group">
            <legend>Agent</legend>
            {!existing && (
              <Field label="Saved in">
                <Select value={form.project ?? ""} onChange={(e) => set("project", e.target.value || null)}>
                  <option value="">Your pulse.toml</option>
                  {trusted.map((p) => <option key={p.name} value={p.name}>Project {p.name}</option>)}
                </Select>
              </Field>
            )}
            <Field label="Name" error={err("name")}>
              <Input mono value={form.name} disabled={existing !== null} onChange={(e) => set("name", e.target.value)} placeholder="study" autoFocus />
            </Field>
            <Field label="Adapter" hint={ADAPTER_NOTE[form.adapter]}>
              <Select value={form.adapter} onChange={(e) => set("adapter", e.target.value as AgentForm["adapter"])}>
                {ADAPTERS.map((a) => <option key={a} value={a}>{a}</option>)}
              </Select>
            </Field>
          </fieldset>
          {form.adapter !== "mcp" && (
            <fieldset className="group">
              <legend>How it runs</legend>
              {(form.adapter === "claude-code" || form.adapter === "codex") && (
                <>
                  <Field label="Program" hint="Leave empty to find it on PATH.">
                    <Input mono value={form.program} onChange={(e) => set("program", e.target.value)} placeholder={form.adapter === "codex" ? "codex" : "claude"} />
                  </Field>
                  <Field label="Model">
                    <Input mono value={form.model} onChange={(e) => set("model", e.target.value)} placeholder="default" />
                  </Field>
                </>
              )}
              {form.adapter === "claude-code" && (
                <>
                  <Field label="Permission mode" hint="Passed through as --permission-mode. Pulse never widens it.">
                    <Input mono value={form.permissionMode} onChange={(e) => set("permissionMode", e.target.value)} list="perm-modes" placeholder="default" />
                    <datalist id="perm-modes"><option value="default" /><option value="acceptEdits" /><option value="plan" /></datalist>
                  </Field>
                  <Field label="Allowed tools"><ListInput values={form.allowedTools} onChange={(v) => set("allowedTools", v)} placeholder="Read" /></Field>
                </>
              )}
              {form.adapter === "codex" && (
                <Field label="Sandbox" hint="Passed through as --sandbox. Pulse never widens it.">
                  <Input mono value={form.sandbox} onChange={(e) => set("sandbox", e.target.value)} list="sandboxes" placeholder="read-only" />
                  <datalist id="sandboxes"><option value="read-only" /><option value="workspace-write" /></datalist>
                </Field>
              )}
              {(form.adapter === "claude-code" || form.adapter === "codex") && (
                <Field label="Extra arguments"><ListInput values={form.extraArgs} onChange={(v) => set("extraArgs", v)} placeholder="--flag" /></Field>
              )}
              {form.adapter === "command" && (
                <>
                  <Field label="Command" error={err("command")} hint={<>One entry per argument; nothing goes through a shell. <code>{"{{path}}"}</code> is replaced per argument. Put <code>--</code> before event-derived arguments, so a file named <code>--exec</code> is not read as an option.</>}>
                    <ListInput values={form.command} onChange={(v) => set("command", v)} placeholder="prettier" />
                  </Field>
                  <Field label="Stdin">
                    <Select value={form.stdin} onChange={(e) => set("stdin", e.target.value as AgentForm["stdin"])}>
                      <option value="prompt">the rendered prompt</option>
                      <option value="event">the event as JSON</option>
                      <option value="none">nothing</option>
                    </Select>
                  </Field>
                </>
              )}
              <Field label="Working directory" hint="Default: the Pulse home's runs/ directory.">
                <Input mono value={form.cwd} onChange={(e) => set("cwd", e.target.value)} placeholder="~/University" />
              </Field>
              <Field label="Timeout" error={err("timeout")}><Input mono className="w-short" value={form.timeout} onChange={(e) => set("timeout", e.target.value)} /></Field>
              <Field label="At once" error={err("max_concurrent")} hint="Runs executing at the same time."><Input mono className="w-short" inputMode="numeric" value={form.maxConcurrent} onChange={(e) => set("maxConcurrent", e.target.value)} /></Field>
              <Field label="Queue" error={err("max_queued")} hint="Runs that may wait for a slot. Past this, fires are blocked as agent busy."><Input mono className="w-short" inputMode="numeric" value={form.maxQueued} onChange={(e) => set("maxQueued", e.target.value)} /></Field>
              <Field label="Environment" error={err("env")} hint="Trusted config. Values are stored in the config file.">
                <div className="env">
                  {form.env.map((row, i) => (
                    <div key={i} className="env-row">
                      <Input mono value={row.key} placeholder="NAME" onChange={(e) => set("env", form.env.map((r, j) => (j === i ? { ...r, key: e.target.value } : r)))} />
                      <Input mono type="password" value={row.value} placeholder="value" onChange={(e) => set("env", form.env.map((r, j) => (j === i ? { ...r, value: e.target.value } : r)))} />
                      <IconButton icon="x" label="Remove" size="sm" variant="ghost" onClick={() => set("env", form.env.filter((_, j) => j !== i))} />
                    </div>
                  ))}
                  <Button size="sm" icon="plus" onClick={() => set("env", [...form.env, { key: "", value: "" }])}>Add variable</Button>
                </div>
              </Field>
            </fieldset>
          )}
          <fieldset className="group">
            <legend>What it can see</legend>
            <Field label="Scopes" error={err("allow")} hint={<>A type glob, optionally with a path glob: <code>fs.*:~/University/**</code>, <code>calendar.*</code>. Default deny.</>}>
              <ListInput values={form.allow} onChange={(v) => set("allow", v)} placeholder="fs.*:~/University/**" problem={(s) => parseScope(s).problem} />
            </Field>
          </fieldset>
        </div>
      </form>
      <aside className="editor-side">
        <div className="tester">
          <div className="tester-head">
            <h3><Icon name="shield" size={14} /> What this agent will see</h3>
          </div>
          <ScopeExplainer allow={form.allow.filter((s) => !parseScope(s).problem)} />
        </div>
      </aside>
    </div>
  );
}
