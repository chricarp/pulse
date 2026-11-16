import { useEffect, useState, type ReactNode } from "react";

import { useApp, useDaemon } from "../../app/context";
import { href, navigate } from "../../app/router";
import { attempt } from "../../app/toasts";
import type { Delivery, EventDetail as Detail, RunSummary } from "../../contracts";
import { RpcCallError } from "../../rpc/client";
import { deliveryLabel, deliveryTone, outcomeSentence, RUN_STATUS_LABEL, runTone } from "../../lib/explain";
import { EMPTY_FILTER, type FeedFilter } from "../../lib/filter";
import { count, dayAndTime, hasHiddenCharacters, shortId, sourceName, visible } from "../../lib/format";
import { typeGlobMatches } from "../../lib/syntax";
import { Icon } from "../../ui/icons";
import { Ago, Button, CopyButton, Dl, IconButton, Modal, Notice, Pill, Section, Toggle, Trunc, cx } from "../../ui/primitives";
import { typeFamily } from "./EventRow";

export function EventDetailPane({ id, onClose, onFilter }: { id: string; onClose: () => void; onFilter: (f: FeedFilter) => void }) {
  const { client, daemon, deliveries } = useApp();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [version, setVersion] = useState(0);
  const [replay, setReplay] = useState(false);

  useEffect(() => {
    let live = true;
    setError(null);
    client
      .call("events.get", { id })
      .then((d) => {
        if (!live) return;
        setDetail(d);
        deliveries.put(d);
      })
      .catch((err: unknown) => {
        if (!live) return;
        setError(err instanceof RpcCallError && err.code === -32002 ? "This event is no longer in the log (pruned by retention)." : err instanceof Error ? err.message : String(err));
      });
    return () => {
      live = false;
    };
  }, [client, deliveries, id, version]);

  // Runs this event caused change state: refresh.
  useEffect(() => daemon.onRunsChanged((runs) => {
    if (runs.some((r) => r.event === id)) setVersion((v) => v + 1);
  }), [daemon, id]);

  if (error) {
    return (
      <aside className="detail" aria-label="Event">
        <DetailHead title="Event" onClose={onClose} />
        <div className="detail-body">
          <Notice tone="error" title="Could not load the event">
            {error}
          </Notice>
        </div>
      </aside>
    );
  }
  if (!detail || detail.event.id !== id) {
    return (
      <aside className="detail" aria-label="Event" aria-busy="true">
        <DetailHead title="Event" onClose={onClose} />
        <div className="detail-body">
          <p className="faint">Loading…</p>
        </div>
      </aside>
    );
  }
  const e = detail.event;
  const runsById = new Map(detail.runs.map((r) => [r.id, r]));
  const loops = detail.deliveries.filter((d) => deliveryTone(d.outcome) === "loop");
  const data = JSON.stringify(e.data, null, 2) ?? "null";

  return (
    <aside className="detail" aria-label={`Event ${e.type}`}>
      <DetailHead
        title={<span className={cx("mono", `fam-${typeFamily(e.type)}`)}>{e.type}</span>}
        onClose={onClose}
        actions={
          <>
            <Button size="sm" icon="rerun" onClick={() => setReplay(true)} title="Emit this event again and route it">
              Replay
            </Button>
            <Button size="sm" icon="flask" onClick={() => navigate(`/rules/new?event=${encodeURIComponent(e.id)}&on=${encodeURIComponent(e.type)}`)}>
              New rule from this
            </Button>
          </>
        }
      />
      <div className="detail-body">
        {e.subject && (
          <div className="subject-line">
            <Trunc text={e.subject} copy tail={40} />
            {hasHiddenCharacters(e.subject) && (
              <Pill tone="attention" title="The subject contains invisible or direction-changing characters, shown as ‹U+…›">
                hidden characters
              </Pill>
            )}
          </div>
        )}
        <Dl
          items={[
            ["When", <span className="num" key="t">{dayAndTime(e.time, Date.now())} · <Ago iso={e.time} /></span>],
            [
              "Source",
              <span key="s">
                <button type="button" className="link-btn mono" onClick={() => onFilter({ ...EMPTY_FILTER, sources: [sourceName(e.source)] })} title="Show only this source">
                  {sourceName(e.source)}
                </button>{" "}
                <span className="faint mono">{e.source}</span>
              </span>,
            ],
            ["Id", <span key="id" className="mono">{e.id} <CopyButton value={e.id} /></span>],
            ...(e.pulsecoalesced && e.pulsecoalesced > 1
              ? ([["Coalesced", <span key="c">Folded from <span className="num">{count(e.pulsecoalesced)}</span> raw signals by the source.</span>]] as [string, ReactNode][])
              : []),
            ...(e.pulsereplayof
              ? ([["Replay of", <a key="r" className="mono" href={href({ view: "feed", eventId: e.pulsereplayof })} onClick={(ev) => { ev.preventDefault(); navigate({ view: "feed", eventId: e.pulsereplayof ?? "" }, { keepSearch: true }); }}>{shortId(e.pulsereplayof)}</a>]] as [string, ReactNode][])
              : []),
          ]}
        />

        <Causation event={e} />

        {loops.length > 0 && (
          <Notice tone="loop" title="Stopped by loop protection">
            {loops.map((d) => d.rule).join(", ")} did not fire on this event. Pulse blocks a rule from reacting to its own runs, chains deeper than the limit, and rules that fire too often.
          </Notice>
        )}

        <Section title={`Deliveries (${detail.deliveries.length})`} id="deliveries">
          {detail.deliveries.length === 0 ? (
            <p className="faint">No rule matched this event, so nothing was decided. Rules match on type and their <code>where</code> table.</p>
          ) : (
            <ul className="deliveries">
              {detail.deliveries.map((d) => (
                <DeliveryItem key={d.id} d={d} run={d.run ? runsById.get(d.run) : undefined} eventId={e.id} />
              ))}
            </ul>
          )}
        </Section>

        {detail.runs.length > 0 && (
          <Section title={`Runs (${detail.runs.length})`}>
            <ul className="mini-runs">
              {detail.runs.map((r) => (
                <li key={r.id}>
                  <RunLink run={r} />
                </li>
              ))}
            </ul>
          </Section>
        )}

        <Section title="Data" actions={<CopyButton value={data} label="Copy data" />}>
          <pre className="code">{visible(data)}</pre>
        </Section>
        <details className="envelope">
          <summary>Full envelope (CloudEvents JSON)</summary>
          <div className="envelope-copy">
            <CopyButton value={JSON.stringify(e, null, 2)} label="Copy envelope" />
          </div>
          <pre className="code">{visible(JSON.stringify(e, null, 2))}</pre>
        </details>
        <div className="detail-actions">
          <span className="faint">Filter by</span>
          <Button size="sm" variant="ghost" onClick={() => onFilter({ ...EMPTY_FILTER, types: [e.type] })}>
            this type
          </Button>
          <Button size="sm" variant="ghost" onClick={() => onFilter({ ...EMPTY_FILTER, sources: [sourceName(e.source)] })}>
            this source
          </Button>
          {e.subject?.startsWith("/") && (
            <Button size="sm" variant="ghost" onClick={() => onFilter({ ...EMPTY_FILTER, path: e.subject?.slice(0, e.subject.lastIndexOf("/")) || "/" })}>
              this directory
            </Button>
          )}
          {e.pulsecausationrun && (
            <Button size="sm" variant="ghost" onClick={() => onFilter({ ...EMPTY_FILTER, causedBy: e.pulsecausationrun ?? null })}>
              events from the same run
            </Button>
          )}
        </div>
      </div>
      <ReplayDialog open={replay} onClose={() => setReplay(false)} eventId={e.id} type={e.type} />
    </aside>
  );
}

function DetailHead({ title, onClose, actions }: { title: ReactNode; onClose: () => void; actions?: ReactNode }) {
  return (
    <header className="detail-head">
      <IconButton icon="back" label="Back to the list" className="only-narrow" onClick={onClose} size="sm" variant="ghost" />
      <h2 className="detail-title">{title}</h2>
      <div className="detail-head-actions">
        {actions}
        <IconButton icon="x" label="Close (Esc)" onClick={onClose} size="sm" variant="ghost" className="hide-narrow" />
      </div>
    </header>
  );
}

function Causation({ event }: { event: Detail["event"] }) {
  if (!event.pulsecausationrun) {
    return event.pulsechaindepth > 0 ? <p className="faint">Chain depth {event.pulsechaindepth}.</p> : null;
  }
  const exact = event.pulsecausation === "exact";
  return (
    <Notice
      tone="neutral"
      title={
        <>
          Caused by run{" "}
          <a className="mono" href={href({ view: "runs", runId: event.pulsecausationrun })} onClick={(e) => { e.preventDefault(); navigate({ view: "runs", runId: event.pulsecausationrun ?? "" }); }}>
            {shortId(event.pulsecausationrun)}
          </a>{" "}
          <Pill tone={exact ? "ok" : "waiting"}>{exact ? "exact" : "inferred"}</Pill>
        </>
      }
    >
      {exact
        ? "The run reported this event itself (it carried the run's PULSE_RUN_ID)."
        : "The change happened inside that run's working directory while it ran (or just after), so Pulse attributes it to the run. Filesystems do not say which process wrote a file, so this is a guess."}{" "}
      Chain depth <span className="num">{event.pulsechaindepth}</span>.
    </Notice>
  );
}

function DeliveryItem({ d, run, eventId }: { d: Delivery; run: RunSummary | undefined; eventId: string }) {
  const tone = deliveryTone(d.outcome);
  const when = (iso: string) => dayAndTime(iso, Date.now());
  const testHref = `/rules/${encodeURIComponent(d.rule)}?test=${encodeURIComponent(eventId)}`;
  return (
    <li className={cx("delivery", `tone-${tone}`)}>
      <div className="delivery-head">
        <Pill tone={tone} {...(tone === "loop" ? { icon: "loop" as const } : {})}>
          {deliveryLabel(d.outcome)}
        </Pill>
        <a className="mono delivery-rule" href={href({ view: "rules", name: d.rule, mode: "show" })} onClick={(e) => { e.preventDefault(); navigate({ view: "rules", name: d.rule, mode: "show" }); }}>
          {d.rule}
        </a>
        <span className="faint">→</span>
        <span className="mono">{d.agent}</span>
        <span className="spacer" />
        <a className="link-btn" href={testHref} onClick={(e) => { e.preventDefault(); navigate(testHref); }} title="Dry-run this rule against this event">
          Test
        </a>
      </div>
      <p className="delivery-why">{outcomeSentence(d.outcome, d.agent, when)}</p>
      {d.outcome.kind === "blocked" && d.outcome.reason.reason === "loop_self_trigger" && (
        <p className="delivery-why">
          Its own run:{" "}
          <a className="mono" href={href({ view: "runs", runId: d.outcome.reason.run })} onClick={(e) => { e.preventDefault(); navigate({ view: "runs", runId: d.outcome.kind === "blocked" && d.outcome.reason.reason === "loop_self_trigger" ? d.outcome.reason.run : "" }); }}>
            {shortId(d.outcome.reason.run)}
          </a>
        </p>
      )}
      {run && (
        <p className="delivery-run">
          <RunLink run={run} />
        </p>
      )}
    </li>
  );
}

export function RunLink({ run }: { run: RunSummary }) {
  return (
    <a className="run-link" href={href({ view: "runs", runId: run.id })} onClick={(e) => { e.preventDefault(); navigate({ view: "runs", runId: run.id }); }}>
      <Pill tone={runTone(run.status)}>{RUN_STATUS_LABEL[run.status]}</Pill>
      <span className="mono">{run.rule ?? "(deleted rule)"}</span>
      <span className="faint">→ {run.agent}</span>
      <span className="faint num">{shortId(run.id)}</span>
      <Ago iso={run.created_at} className="faint" />
    </a>
  );
}

function ReplayDialog({ open, onClose, eventId, type }: { open: boolean; onClose: () => void; eventId: string; type: string }) {
  const { client } = useApp();
  const rules = useDaemon((s) => s.rules);
  const matching = (rules ?? []).filter((r) => !r.config.archived && r.config.on.some((g) => typeGlobMatches(g, type)));
  const [all, setAll] = useState(true);
  const [picked, setPicked] = useState<string[]>([]);
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Replay this event"
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            variant="primary"
            icon="rerun"
            disabled={!all && picked.length === 0}
            onClick={async () => {
              const r = await attempt("Replay", () => client.call("events.replay", { id: eventId, ...(all ? {} : { rules: picked }) }));
              onClose();
              if (r) navigate({ view: "feed", eventId: r.event.id }, { keepSearch: true });
            }}
          >
            Replay
          </Button>
        </>
      }
    >
      <p>
        A copy of the event is written to the log as a new event (marked as a replay, with no causation) and routed again. Rules can fire, so agents can run.
      </p>
      <Toggle checked={all} onChange={setAll} label="Route to every rule" hint="Otherwise only the rules you pick." />
      {!all && (
        <ul className="pick-list">
          {matching.length === 0 && <li className="faint">No rule listens to {type}.</li>}
          {matching.map((r) => (
            <li key={r.config.name}>
              <Toggle
                checked={picked.includes(r.config.name)}
                onChange={(v) => setPicked((p) => (v ? [...p, r.config.name] : p.filter((x) => x !== r.config.name)))}
                label={<span className="mono">{r.config.name}</span>}
                hint={r.config.enabled ? undefined : "disabled"}
              />
            </li>
          ))}
        </ul>
      )}
    </Modal>
  );
}

