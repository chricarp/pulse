import { useState } from "react";

import { useApp, useDaemon } from "../../app/context";
import { href, navigate } from "../../app/router";
import { attempt, toast } from "../../app/toasts";
import type { Project } from "../../contracts";
import { TRUST_LABEL, trustTone } from "../../lib/explain";
import { dayAndTime, plural } from "../../lib/format";
import { Button, Confirm, Empty, Field, Input, Modal, Notice, Pill, Trunc } from "../../ui/primitives";

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
                <span className="mono">line {e.line ?? "?"}</span> {e.item && <span className="mono">{e.item}</span>} {e.field && <code>{e.field}</code>} {e.message}
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
      <Confirm
        open={confirm === "trust"}
        onClose={() => setConfirm(null)}
        title={`Trust ${p.name}?`}
        confirm="Trust this content"
        onConfirm={() => void attempt("Trust", () => client.call("projects.trust", { dir: p.dir, name: p.name }), `Trusted ${p.name}; its definitions are active`)}
      >
        <p>
          Pulse will load the rules, agents and sources in <span className="mono">{p.file}</span>. Its rules can wake agents and run the commands it defines. Trust covers this exact content only.
        </p>
      </Confirm>
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

function TrustDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { client } = useApp();
  const [dir, setDir] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Trust a project"
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            variant="primary"
            disabled={!dir.trim()}
            onClick={async () => {
              // A refusal (no pulse.toml there, a name in use) is shown in the dialog, not behind it.
              setError(null);
              try {
                await client.call("projects.trust", { dir: dir.trim(), ...(name.trim() ? { name: name.trim() } : {}) });
              } catch (err) {
                setError(err instanceof Error ? err.message : String(err));
                return;
              }
              toast("Trusted", "ok");
              setDir("");
              setName("");
              onClose();
            }}
          >
            Trust
          </Button>
        </>
      }
    >
      <p>Point at a directory that contains a <code>pulse.toml</code>. Read the file first: trusting it lets its rules wake agents and run its commands.</p>
      <Field label="Directory" error={error}>
        <Input mono autoFocus value={dir} onChange={(e) => setDir(e.target.value)} placeholder="~/code/my-repo" />
      </Field>
      <Field label="Name" hint="The prefix for its definitions (name/rule). Default: the directory's name.">
        <Input mono value={name} onChange={(e) => setName(e.target.value)} placeholder="my-repo" />
      </Field>
    </Modal>
  );
}
