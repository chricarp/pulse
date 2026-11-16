import { useEffect, useState, type ReactNode } from "react";

import { useApp, useDaemon } from "../../app/context";
import { isActive } from "../../app/daemon";
import { useBindings } from "../../app/keys";
import { href, navigate } from "../../app/router";
import { attempt } from "../../app/toasts";
import type { Event, Run } from "../../contracts";
import { RUN_STATUS_LABEL, runTone } from "../../lib/explain";
import { dayAndTime, shortId, sourceName, span, visible } from "../../lib/format";
import { RpcCallError } from "../../rpc/client";
import { Icon } from "../../ui/icons";
import { Button, Dl, IconButton, Modal, Notice, Pill, TextArea, Trunc, cx, useTicker } from "../../ui/primitives";
import { PromptView } from "../common/PromptView";
import { RunOutput } from "./RunOutput";

type Tab = "output" | "prompt" | "invocation" | "snapshot";

export function RunDetail({ id, onClose }: { id: string; onClose: () => void }) {
  const { client } = useApp();
  const summary = useDaemon((s) => s.runs.get(id));
  const [run, setRun] = useState<Run | null>(null);
  const [event, setEvent] = useState<Event | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("output");
  const [rejecting, setRejecting] = useState(false);

  // Refetch the full run whenever its summary changes (status, end time).
  const stamp = summary ? `${summary.status}|${summary.ended_at ?? ""}|${summary.started_at ?? ""}` : "";
  useEffect(() => {
    let live = true;
    client
      .call("runs.get", { id })
      .then((r) => {
        if (!live) return;
        setRun(r);
        setError(null);
      })
      .catch((err: unknown) => {
        if (live) setError(err instanceof RpcCallError && err.code === -32002 ? "This run is not in the log (pruned by retention, or never existed)." : String(err instanceof Error ? err.message : err));
      });
    return () => {
      live = false;
    };
  }, [client, id, stamp]);

  useEffect(() => {
    if (!run) return;
    let live = true;
    client.call("events.get", { id: run.event }).then(
      (d) => live && setEvent(d.event),
      () => live && setEvent(null),
    );
    return () => {
      live = false;
    };
  }, [client, run?.event]);

  const status = summary?.status ?? run?.status;
  const pending = status === "pending_approval";
  const cancellable = status === "queued" || status === "running" || pending;
  const approve = () => void attempt("Approve", () => client.call("runs.approve", { id }), "Approved: the run is queued");
  const cancel = () => void attempt("Cancel", () => client.call("runs.cancel", { id }), "Cancelled");
  const rerun = async () => {
    const r = await attempt("Rerun", () => client.call("runs.rerun", { id }), "Rerun queued");
    if (r) navigate({ view: "runs", runId: r.id });
  };

  useBindings([
    { keys: "shift+a", label: "Approve the run", group: "Runs", run: () => pending && approve() },
    { keys: "shift+x", label: "Reject the run", group: "Runs", run: () => pending && setRejecting(true) },
    { keys: "shift+c", label: "Cancel the run", group: "Runs", run: () => cancellable && cancel() },
    { keys: "shift+r", label: "Rerun", group: "Runs", run: () => status && !isActive(status) && void rerun() },
  ]);

  if (error) {
    return (
      <aside className="detail">
        <header className="detail-head">
          <h2 className="detail-title">Run</h2>
          <div className="detail-head-actions">
            <IconButton icon="x" label="Close" size="sm" variant="ghost" onClick={onClose} />
          </div>
        </header>
        <div className="detail-body">
          <Notice tone="error" title="Could not load the run">
            {error}
          </Notice>
        </div>
      </aside>
    );
  }
  if (!run || !status) {
    return (
      <aside className="detail" aria-busy="true">
        <div className="detail-body">
          <p className="faint">Loading…</p>
        </div>
      </aside>
    );
  }

  const agent = run.snapshot.agent;
  return (
    <aside className="detail run-detail" aria-label={`Run ${shortId(run.id)}`}>
      <header className="detail-head">
        <IconButton icon="back" label="Back to runs" className="only-narrow" onClick={onClose} size="sm" variant="ghost" />
        <Pill tone={runTone(status)}>{RUN_STATUS_LABEL[status]}</Pill>
        <h2 className="detail-title">
          <span className="mono">{run.rule ?? "(deleted rule)"}</span> <span className="faint">→</span> <span className="mono">{run.agent}</span>
        </h2>
        <div className="detail-head-actions">
          {pending && (
            <>
              <Button size="sm" variant="attention" icon="check" onClick={approve} shortcut="shift+a">
                Approve
              </Button>
              <Button size="sm" icon="x" onClick={() => setRejecting(true)} shortcut="shift+x">
                Reject
              </Button>
            </>
          )}
          {cancellable && !pending && (
            <Button size="sm" icon="stop" onClick={cancel} shortcut="shift+c">
              Cancel
            </Button>
          )}
          {!isActive(status) && (
            <Button size="sm" icon="rerun" onClick={() => void rerun()} shortcut="shift+r" title="Start a new run with the same snapshot and prompt">
              Rerun
            </Button>
          )}
          <IconButton icon="x" label="Close (Esc)" onClick={onClose} size="sm" variant="ghost" className="hide-narrow" />
        </div>
      </header>

      {pending && <ApprovalCard run={run} />}

      <div className="run-meta">
        <Dl
          items={[
            [
              "Triggered by",
              <span key="e" className="run-event">
                <a href={href({ view: "feed", eventId: run.event })} onClick={(e) => { e.preventDefault(); navigate({ view: "feed", eventId: run.event }); }} className="mono">
                  {event?.type ?? shortId(run.event)}
                </a>{" "}
                {event?.subject && <Trunc text={event.subject} />}
                {event && <span className="faint"> from {sourceName(event.source)}</span>}
              </span>,
            ],
            ["Timing", <Timing key="t" run={run} />],
            [
              "Process",
              <span key="p" className="num">
                {run.exit_code !== null && run.exit_code !== undefined
                  ? `exit ${run.exit_code}`
                  : run.signal
                    ? `signal ${run.signal}`
                    : status === "pending_approval" || status === "queued"
                      ? "not started"
                      : status === "running"
                        ? "running"
                        : "no exit code"}
                {run.pid ? <span className="faint"> · pid {run.pid}</span> : null}
                <span className="faint"> · chain depth {run.chain_depth}</span>
              </span>,
            ],
            ...(run.rerun_of
              ? ([["Rerun of", <a key="r" className="mono" href={href({ view: "runs", runId: run.rerun_of })} onClick={(e) => { e.preventDefault(); navigate({ view: "runs", runId: run.rerun_of ?? "" }); }}>{shortId(run.rerun_of)}</a>]] as [string, ReactNode][])
              : []),
            ["Id", <span key="id" className="mono faint">{run.id}</span>],
          ]}
        />
        {run.error && (
          <Notice tone={status === "rejected" || status === "cancelled" ? "neutral" : "error"} title={status === "rejected" ? "Rejected" : status === "cancelled" ? "Cancelled" : "Error"}>
            {run.error}
          </Notice>
        )}
      </div>

      <div className="tabs" role="tablist" aria-label="Run detail">
        {(["output", "prompt", "invocation", "snapshot"] as const).map((t) => (
          <button key={t} type="button" role="tab" aria-selected={tab === t} className={cx("tab", tab === t && "is-active")} onClick={() => setTab(t)}>
            {t === "output" ? "Output" : t === "prompt" ? "Prompt" : t === "invocation" ? "Invocation" : "Snapshot"}
          </button>
        ))}
      </div>
      <div className="tab-panel" role="tabpanel">
        {tab === "output" &&
          (pending ? (
            <p className="faint tab-note">Nothing has run yet. Approve the run to start it; its output streams here.</p>
          ) : (
            <RunOutput runId={run.id} live={status === "running" || status === "queued"} droppedBytes={run.output.dropped_bytes} totalBytes={run.output.bytes} />
          ))}
        {tab === "prompt" && (
          <div className="tab-scroll">
            {run.prompt ? <PromptView text={run.prompt} /> : <p className="faint">No prompt: a command agent whose rule has none. It gets the event on stdin or in its environment.</p>}
          </div>
        )}
        {tab === "invocation" && (
          <div className="tab-scroll">
            {run.invocation || isActive(status) ? <InvocationView run={run} pending={!run.started_at} /> : <p className="faint">This run never started a process.</p>}
            <p className="faint">Adapter: <span className="mono">{agent.adapter}</span></p>
          </div>
        )}
        {tab === "snapshot" && (
          <div className="tab-scroll">
            <p className="muted">The rule and agent exactly as configured when this run was created. Later edits do not change it; a rerun uses it again.</p>
            <h4 className="sub-head">Rule</h4>
            <pre className="code">{run.snapshot.rule ? JSON.stringify(run.snapshot.rule, null, 2) : "(none)"}</pre>
            <h4 className="sub-head">Agent</h4>
            <pre className="code">{JSON.stringify(agent, null, 2)}</pre>
          </div>
        )}
      </div>
      <RejectDialog open={rejecting} id={run.id} onClose={() => setRejecting(false)} />
    </aside>
  );
}

function Timing({ run }: { run: Run }) {
  const running = run.status === "running";
  const now = useTicker(running);
  const start = run.started_at ? Date.parse(run.started_at) : null;
  const end = run.ended_at ? Date.parse(run.ended_at) : running ? now : null;
  return (
    <span className="num">
      created {dayAndTime(run.created_at, now)}
      {start !== null && end !== null && (
        <>
          {" "}
          · {running ? "running for" : "took"} <strong>{span(end - start)}</strong>
        </>
      )}
      {run.status === "queued" && <span className="faint"> · waiting for a free slot</span>}
    </span>
  );
}

function ApprovalCard({ run }: { run: Run }) {
  const agent = run.snapshot.agent;
  return (
    <div className="approval">
      <div className="approval-head">
        <Icon name="shield" />
        <div>
          <strong>This run is waiting for your approval.</strong>
          <p>
            Rule <span className="mono">{run.rule}</span> is supervised. Nothing has run yet. Approving starts <span className="mono">{agent.name}</span> ({agent.adapter}
            {agent.cwd ? (
              <>
                {" "}
                in <span className="mono">{agent.cwd}</span>
              </>
            ) : null}
            ) with the prompt below.
          </p>
          <p className="faint">
            It can see: {(agent.allow ?? []).length ? (agent.allow ?? []).map((s) => <code key={s}>{s}</code>) : "nothing (no scopes)"}
          </p>
        </div>
      </div>
      <InvocationView run={run} pending />
      {run.prompt ? <PromptView text={run.prompt} /> : <p className="faint">No prompt: the command above gets the event on stdin or in its environment, as configured.</p>}
    </div>
  );
}

/**
 * The argv and working directory a run executes. Rendered by the daemon when the run was
 * created, with the event's values substituted; for a pending run, what approving it starts (the
 * daemon refuses to start it if the agent changed since). Arguments may carry event content.
 */
function InvocationView({ run, pending }: { run: Run; pending?: boolean }) {
  const inv = run.invocation;
  if (!inv) return <p className="faint">Pulse could not prepare this run's command{pending ? ", so approving it will fail to start it" : ""}.</p>;
  return (
    <div className="invocation">
      <p className="muted">{pending ? "Approving runs exactly this" : "The command"}, as an argument list (never through a shell). The environment is not shown; it may hold secrets from the agent's config.</p>
      <Dl
        items={[
          ["Program", <span key="p" className="mono">{visible(inv.program, { singleLine: true })}</span>],
          [
            "Arguments",
            <ol key="a" className="argv">
              {inv.args.map((a, i) => (
                <li key={i} className="mono">
                  {a === "" ? <span className="faint">(empty)</span> : a}
                </li>
              ))}
            </ol>,
          ],
          ["Working directory", <Trunc key="c" text={inv.cwd} copy />],
        ]}
      />
    </div>
  );
}

function RejectDialog({ open, id, onClose }: { open: boolean; id: string; onClose: () => void }) {
  const { client } = useApp();
  const [reason, setReason] = useState("");
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Reject this run"
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            variant="danger"
            onClick={async () => {
              await attempt("Reject", () => client.call("runs.reject", { id, ...(reason.trim() ? { reason: reason.trim() } : {}) }), "Rejected");
              setReason("");
              onClose();
            }}
          >
            Reject
          </Button>
        </>
      }
    >
      <p>The run will not start. The rule stays as it is; the next matching event can ask again.</p>
      <TextArea autoFocus value={reason} onChange={(e) => setReason(e.target.value)} placeholder="Reason (optional, kept with the run)" rows={3} />
    </Modal>
  );
}
