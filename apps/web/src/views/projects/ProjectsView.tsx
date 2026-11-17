import { useEffect, useState, type ReactNode } from "react";

import { useApp, useDaemon } from "../../app/context";
import { useTildify, useTildifyText } from "../../app/home";
import { href, navigate } from "../../app/router";
import { attempt, toast } from "../../app/toasts";
import type { AgentConfig, Project, ProjectReview, SourceConfig } from "../../contracts";
import { staleOf } from "../../lib/edits";
import { TRUST_LABEL, trustTone } from "../../lib/explain";
import { dayAndTime, plural, visible } from "../../lib/format";
import { Button, Confirm, Dl, Empty, Field, Input, Modal, Notice, Path, Pill, Section, Trunc } from "../../ui/primitives";

export function ProjectsView() {
  const projects = useDaemon((s) => s.projects);
  const [adding, setAdding] = useState(false);
  const list = projects ?? [];
  return (
    <div className="split">
      <div className="split-main">
        <header className="view-head">
          <h1>Projects</h1>
          <span className="sub num">{plural(list.length, "project file")}</span>
          <div className="view-actions">
            <Button variant="primary" icon="plus" onClick={() => setAdding(true)}>
              Trust a project
            </Button>
          </div>
        </header>
        <div className="table-scroll view-pad stack">
          <p className="muted intro">
            A repository can check in a <code>pulse.toml</code> with its own rules, agents and sources. Pulse loads it only after you trust it, and only for the exact content you trusted: when the file changes, it is inactive until you look again. Cloning a repository is never enough to run its commands.
          </p>
          {projects && list.length === 0 ? <Empty title="No project files">Trust a directory that contains a pulse.toml to load its definitions.</Empty> : list.map((p) => <ProjectCard key={p.dir} p={p} />)}
        </div>
      </div>
      <TrustDialog open={adding} onClose={() => setAdding(false)} />
    </div>
  );
}

function ProjectCard({ p }: { p: Project }) {
  const { client } = useApp();
  const rules = useDaemon((s) => s.rules) ?? [];
  const agents = useDaemon((s) => s.agents) ?? [];
  const sources = useDaemon((s) => s.sources) ?? [];
  const errors = (useDaemon((s) => s.configErrors) ?? []).filter((e) => e.file === p.file);
  const shortText = useTildifyText();
  const [confirm, setConfirm] = useState<"trust" | "untrust" | null>(null);
  const mine = <T extends { origin: { kind: string; name?: string } }>(xs: T[]) => xs.filter((x) => x.origin.kind === "project" && x.origin.name === p.name);
  const defs = { rules: mine(rules), agents: mine(agents), sources: mine(sources) };
  const tone = trustTone(p.state);
  return (
    <article className="project">
      <header className="project-head">
        <h2 className="mono">{p.name}</h2>
        <Pill tone={tone}>{TRUST_LABEL[p.state]}</Pill>
        <span className="spacer" />
        {p.state === "trusted" ? (
          <Button size="sm" onClick={() => setConfirm("untrust")}>
            Untrust
          </Button>
        ) : p.state !== "missing" ? (
          <Button size="sm" variant={p.state === "changed" ? "attention" : "primary"} icon="shield" onClick={() => setConfirm("trust")}>
            {p.state === "changed" ? "Review and trust again" : "Trust"}
          </Button>
        ) : null}
      </header>
      <div className="project-file">
        <Trunc text={p.file} copy />
      </div>
      {p.state === "changed" && (
        <Notice tone="attention" title="Changed since you trusted it">
          Its definitions are inactive. Read the file (a <code>git pull</code> can change it), then trust it again if you are happy with it.
        </Notice>
      )}
      {p.state === "untrusted" && <p className="faint">Found, not loaded. Nothing in it runs until you trust it.</p>}
      {p.state === "missing" && <p className="faint">The file is gone. Its definitions are inactive.</p>}
      {errors.length > 0 && (
        <Notice tone="error" title={`${plural(errors.length, "error")} in this file; nothing from it is loaded`}>
          <ul className="config-errors">
            {errors.map((e, i) => (
              <li key={i}>
                <span className="mono">line {e.line ?? "?"}</span> {e.item && <span className="mono">{e.item}</span>} {e.field && <code>{e.field}</code>} {shortText(e.message)}
              </li>
            ))}
          </ul>
        </Notice>
      )}
      <dl className="project-meta">
        {p.trusted_at && (
          <div>
            <dt>Trusted</dt>
            <dd className="num">{dayAndTime(p.trusted_at, Date.now())}</dd>
          </div>
        )}
        {p.trusted_hash && (
          <div>
            <dt>Trusted content</dt>
            <dd className="mono" title={p.trusted_hash}>{p.trusted_hash.slice(0, 16)}…</dd>
          </div>
        )}
        {p.current_hash && p.current_hash !== p.trusted_hash && (
          <div>
            <dt>Current content</dt>
            <dd className="mono" title={p.current_hash}>{p.current_hash.slice(0, 16)}…</dd>
          </div>
        )}
      </dl>
      {p.state === "trusted" && errors.length === 0 && (
        <div className="project-defs">
          <span className="faint">Defines</span>
          {defs.rules.map((r) => (
            <a key={r.config.name} className="mono chip-link" href={href({ view: "rules", name: r.config.name, mode: "show" })} onClick={(e) => { e.preventDefault(); navigate({ view: "rules", name: r.config.name, mode: "show" }); }}>
              rule {r.config.name}
            </a>
          ))}
          {defs.agents.map((a) => (
            <a key={a.config.name} className="mono chip-link" href={href({ view: "agents", name: a.config.name, mode: "show" })} onClick={(e) => { e.preventDefault(); navigate({ view: "agents", name: a.config.name, mode: "show" }); }}>
              agent {a.config.name}
            </a>
          ))}
          {defs.sources.map((s) => (
            <a key={s.config.name} className="mono chip-link" href={href({ view: "sources", name: s.config.name, mode: "show" })} onClick={(e) => { e.preventDefault(); navigate({ view: "sources", name: s.config.name, mode: "show" }); }}>
              source {s.config.name}
            </a>
          ))}
          {defs.rules.length + defs.agents.length + defs.sources.length === 0 && <span className="faint">nothing</span>}
        </div>
      )}
      {confirm === "trust" && <TrustReview dir={p.dir} name={p.name} onClose={() => setConfirm(null)} />}
      <Confirm
        open={confirm === "untrust"}
        onClose={() => setConfirm(null)}
        title={`Untrust ${p.name}?`}
        confirm="Untrust"
        danger
        onConfirm={() => void attempt("Untrust", () => client.call("projects.untrust", { dir: p.dir }), `Untrusted ${p.name}; its definitions are inactive`)}
      >
        <p>Its rules, agents and sources stop. History stays in the log. You can trust it again at any time.</p>
      </Confirm>
    </article>
  );
}

/** Asks for a directory, then shows what trusting it would load. */
function TrustDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [dir, setDir] = useState("");
  const [name, setName] = useState("");
  const [reviewing, setReviewing] = useState(false);
  const close = () => {
    setReviewing(false);
    onClose();
  };
  if (open && reviewing) {
    return (
      <TrustReview
        dir={dir.trim()}
        {...(name.trim() ? { name: name.trim() } : {})}
        onClose={close}
        onTrusted={() => {
          setDir("");
          setName("");
        }}
      />
    );
  }
  return (
    <Modal
      open={open}
      onClose={close}
      title="Trust a project"
      actions={
        <>
          <Button onClick={close}>Cancel</Button>
          <Button variant="primary" disabled={!dir.trim()} onClick={() => setReviewing(true)}>
            Review
          </Button>
        </>
      }
    >
      <p>Point at a directory that contains a <code>pulse.toml</code>. Pulse shows you what it defines before anything is trusted.</p>
      <Field label="Directory">
        <Input mono autoFocus value={dir} onChange={(e) => setDir(e.target.value)} placeholder="~/code/my-repo" />
      </Field>
      <Field label="Name" hint="The prefix for its definitions (name/rule). Default: the directory's name.">
        <Input mono value={name} onChange={(e) => setName(e.target.value)} placeholder="my-repo" />
      </Field>
    </Modal>
  );
}

/**
 * What trusting a project file would load, read from the daemon, and a button that trusts
 * exactly that content (by its hash). If the file changes before the button is pressed, nothing
 * is trusted and the new content is shown instead. Everything shown comes from the repository.
 */
export function TrustReview({ dir, name, onClose, onTrusted }: { dir: string; name?: string; onClose: () => void; onTrusted?: () => void }) {
  const { client } = useApp();
  const [review, setReview] = useState<ProjectReview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [changedWhileOpen, setChangedWhileOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const load = async () => {
    setReview(null);
    try {
      setReview(await client.call("projects.review", { dir, ...(name ? { name } : {}) }));
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };
  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, dir, name]);
  const trust = async () => {
    if (!review) return;
    setBusy(true);
    try {
      await client.call("projects.trust", { dir, ...(name ? { name } : {}), hash: review.hash });
      toast(`Trusted ${review.name}; its definitions are active`, "ok");
      onTrusted?.();
      onClose();
    } catch (err) {
      if (staleOf(err)) {
        setChangedWhileOpen(true);
        await load();
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    }
    setBusy(false);
  };
  return (
    <Modal
      open
      wide
      onClose={onClose}
      title={review ? `Trust ${review.name}?` : "Review the project file"}
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" icon="shield" disabled={!review || review.errors.length > 0 || busy} onClick={() => void trust()}>
            Trust this content
          </Button>
        </>
      }
    >
      {error && (
        <Notice tone="error" title="Could not review it">
          {error}
        </Notice>
      )}
      {changedWhileOpen && (
        <Notice tone="attention" title="The file changed while you were reading it">
          Nothing was trusted. Below is what it says now; read it again before you trust it.
        </Notice>
      )}
      {!review && !error && <p className="faint">Reading the file…</p>}
      {review && <ReviewBody review={review} />}
    </Modal>
  );
}

// Repository text: DOM text goes through `visible()` in the JSX runtime; `line` also shows
// line breaks inside what should be one line.
function ReviewBody({ review }: { review: ProjectReview }) {
  const line = (t: string) => visible(t, { singleLine: true });
  const tildify = useTildify();
  const tildifyText = useTildifyText();
  // Paths in the file and in messages read as `~/…` under the home, as everywhere else.
  const text = (t: string) => line(tildifyText(t));
  const nothing = review.rules.length + review.agents.length + review.sources.length === 0;
  return (
    <div className="review stack">
      <p>
        Trusting loads what <Path path={review.file} /> defines, as <span className="mono">{line(review.name)}/…</span>: its rules can wake agents and run the commands below. Trust covers exactly this content (<span className="mono" title={review.hash}>{review.hash.slice(0, 12)}</span>); any change makes it inactive again.
      </p>
      {review.state === "changed" &&
        (review.changes ? (
          <Section title="What changed since you trusted it">
            {review.changes.length === 0 ? (
              <p className="faint">Nothing in its definitions: only comments or formatting.</p>
            ) : (
              <ul className="plain review-changes">
                {review.changes.map((c) => (
                  <li key={`${c.kind} ${c.name}`}>
                    {c.kind} <span className="mono">{line(c.name)}</span>: {c.change.kind === "changed" ? <>changed <span className="mono">{c.change.fields.map(line).join(", ")}</span></> : c.change.kind}
                  </li>
                ))}
              </ul>
            )}
          </Section>
        ) : (
          <Notice tone="attention" title="Changed since you trusted it">
            Pulse did not keep the version you trusted, so it cannot say what changed. Read all of it.
          </Notice>
        ))}
      {review.errors.length > 0 && (
        <Notice tone="error" title="It cannot be trusted as it is">
          <ul className="config-errors">
            {review.errors.map((e, i) => (
              <li key={i}>
                <span className="mono">line {e.line ?? "?"}</span> {e.item && <span className="mono">{line(e.item)}</span>} {e.field && <code>{line(e.field)}</code>} {text(e.message)}
              </li>
            ))}
          </ul>
        </Notice>
      )}
      {nothing && review.errors.length === 0 && <p className="faint">It defines nothing.</p>}
      {review.agents.length > 0 && (
        <Section title="Agents: what they run">
          {review.agents.map((a) => (
            <div key={a.name} className="review-def">
              <h4 className="mono">{line(a.name)} <Pill tone="neutral">{a.adapter}</Pill></h4>
              <Dl items={agentFacts(a, line, text, review.cwds[a.name])} />
            </div>
          ))}
        </Section>
      )}
      {review.rules.length > 0 && (
        <Section title="Rules: when they run">
          {review.rules.map((r) => (
            <div key={r.name} className="review-def">
              <h4 className="mono">{line(r.name)}</h4>
              <Dl
                items={[
                  ["On", <span key="o" className="mono">{r.on.map(line).join(", ")}</span>],
                  ...(Object.keys(r.where ?? {}).length ? [["Where", <span key="w" className="mono">{line(JSON.stringify(r.where))}</span>] as [string, ReactNode]] : []),
                  ["Wakes", <span key="a" className="mono">{line(r.run.agent)}</span>],
                  ["Approval", r.supervised ? "each run waits for you" : <strong key="s">runs without asking</strong>],
                  ...(r.enabled === false ? [["State", "disabled"] as [string, ReactNode]] : []),
                ]}
              />
              {r.run.prompt ? <pre className="code">{r.run.prompt}</pre> : <p className="faint">No prompt: the agent's command runs as defined above.</p>}
            </div>
          ))}
        </Section>
      )}
      {review.sources.length > 0 && (
        <Section title="Sources: what it watches">
          <ul className="plain">
            {review.sources.map((s) => (
              <li key={s.name}>
                <span className="mono">{line(s.name)}</span> <Pill tone="neutral">{s.kind}</Pill> <span className="mono">{line(tildify(sourceTarget(s)))}</span>
              </li>
            ))}
          </ul>
        </Section>
      )}
      <details className="review-text">
        <summary>The file as written</summary>
        <pre className="code">{review.text}</pre>
      </details>
    </div>
  );
}

function sourceTarget(s: SourceConfig): string {
  switch (s.kind) {
    case "fs":
    case "git":
      return s.path;
    case "schedule":
      return s.cron ? `cron ${s.cron}` : `every ${s.every ?? "?"}`;
    case "calendar":
      return s.url ?? s.path ?? "";
  }
}

/** `text` shows a value that may hold a path under the home as `~/…` (a program, a scope). */
function agentFacts(a: AgentConfig, line: (t: string) => string, text: (t: string) => string, cwd: string | undefined): [string, ReactNode][] {
  const mono = (k: string, t: string) => <span key={k} className="mono">{line(t)}</span>;
  const list = (k: string, xs: string[]) => (
    <ol key={k} className="argv">
      {xs.map((x, i) => (
        <li key={i} className="mono">{x === "" ? <span className="faint">(empty)</span> : line(x)}</li>
      ))}
    </ol>
  );
  const out: [string, ReactNode][] = [];
  switch (a.adapter) {
    case "command":
      out.push(["Command", list("c", a.command ?? [])], ["Stdin", mono("s", a.stdin ?? "prompt")]);
      break;
    case "claude-code":
      out.push(["Program", a.program ? <Path key="p" path={a.program} /> : mono("p", "claude (from PATH)")], ["Permission mode", mono("m", a.permission_mode ?? "dontAsk (default)")]);
      if (a.model) out.push(["Model", mono("mo", a.model)]);
      if (a.allowed_tools?.length) out.push(["Allowed tools", list("t", a.allowed_tools)]);
      if (a.disallowed_tools?.length) out.push(["Disallowed tools", list("d", a.disallowed_tools)]);
      if (a.extra_args?.length) out.push(["Extra arguments", list("x", a.extra_args)]);
      break;
    case "codex":
      out.push(["Program", a.program ? <Path key="p" path={a.program} /> : mono("p", "codex (from PATH)")], ["Sandbox", mono("s", a.sandbox ?? "read-only (default)")], ["Approval policy", mono("ap", a.approval_policy ?? "never (default)")]);
      if (a.model) out.push(["Model", mono("mo", a.model)]);
      if (a.extra_args?.length) out.push(["Extra arguments", list("x", a.extra_args)]);
      break;
    case "mcp":
      out.push(["Runs", "nothing: an identity for an MCP client"]);
      break;
  }
  out.push(["Sees (scopes)", (a.allow ?? []).length ? list("al", (a.allow ?? []).map(text)) : <span key="n" className="faint">nothing</span>]);
  // Where it runs, resolved by the daemon (`.` alone does not say where); as written in the title.
  if (a.adapter !== "mcp") {
    out.push([
      "Working directory",
      cwd ? (
        <span key="cwd" title={a.cwd ? `written as ${a.cwd}` : undefined}>
          <Path path={cwd} />
          {a.cwd && a.cwd !== cwd && !a.cwd.startsWith("~") && <span className="faint"> ({line(a.cwd)} in the file)</span>}
        </span>
      ) : (
        mono("cwd", a.cwd ?? "its own run directory")
      ),
    ]);
  }
  const env = Object.entries(a.env ?? {});
  if (env.length) out.push(["Environment", list("e", env.map(([k, x]) => `${k}=${x}`))]);
  return out;
}
