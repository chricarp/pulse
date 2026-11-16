import { useVirtualizer } from "@tanstack/react-virtual";
import { useCallback, useEffect, useRef, useState } from "react";

import { useApp, useDaemon } from "../../app/context";
import { useBindings } from "../../app/keys";
import { navigate, setSearch, useLocation } from "../../app/router";
import type { RunFilter, RunStatus, RunSummary } from "../../contracts";
import { applyRunChanges } from "../../app/daemon";
import { RUN_STATUS_LABEL, runTone } from "../../lib/explain";
import { count, dayAndTime, shortId, span } from "../../lib/format";
import { Empty, Notice, Pill, Select, cx, useNow } from "../../ui/primitives";
import { RunDetail } from "./RunDetail";

const TABS: { key: string; label: string; status: RunStatus[] }[] = [
  { key: "all", label: "All", status: [] },
  { key: "approval", label: "Needs approval", status: ["pending_approval"] },
  { key: "active", label: "Active", status: ["queued", "running"] },
  { key: "failed", label: "Failed", status: ["failed", "timed_out", "interrupted"] },
  { key: "done", label: "Finished", status: ["succeeded", "cancelled", "rejected"] },
];

/** Runs matching a filter: paged from the daemon, kept live by `runs.changed`. */
export function useRunList(filter: RunFilter) {
  const { client, daemon } = useApp();
  const key = JSON.stringify(filter);
  const [items, setItems] = useState<RunSummary[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const connected = useDaemon((s) => s.connection.kind === "open");

  useEffect(() => {
    if (!connected) return;
    let live = true;
    // Changes pushed before the page arrives are applied on top of it: the page may have been
    // read before them.
    let early: RunSummary[] | null = [];
    setLoading(true);
    client
      .call("runs.list", { filter, limit: 100 })
      .then((page) => {
        if (!live) return;
        setItems(applyRunChanges(page.runs, early ?? [], filter));
        early = null;
        setCursor(page.next_cursor ?? null);
        setError(null);
        setLoading(false);
      })
      .catch((e: unknown) => {
        if (!live) return;
        early = null;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      });
    const off = daemon.onRunsChanged((runs) => {
      if (early) early.push(...runs);
      else setItems((prev) => applyRunChanges(prev, runs, filter));
    });
    return () => {
      live = false;
      off();
    };
    // `key` stands for `filter`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, daemon, key, connected]);

  const loadMore = async () => {
    if (!cursor || loading) return;
    setLoading(true);
    try {
      const page = await client.call("runs.list", { filter, limit: 100, cursor });
      setItems((prev) => [...prev, ...page.runs.filter((r) => !prev.some((p) => p.id === r.id))]);
      setCursor(page.next_cursor ?? null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
    setLoading(false);
  };
  return { items, loading, error, more: cursor !== null, loadMore };
}

export function RunsView({ runId }: { runId: string | null }) {
  const { search } = useLocation();
  const params = new URLSearchParams(search);
  const tabKey = params.get("show") ?? "all";
  const tab = TABS.find((t) => t.key === tabKey) ?? TABS[0];
  const rule = params.get("rule") ?? "";
  const agent = params.get("agent") ?? "";
  const filter: RunFilter = { status: tab?.status ?? [], ...(rule ? { rule } : {}), ...(agent ? { agent } : {}) };
  const { items, loading, error, more, loadMore } = useRunList(filter);
  const runs = useDaemon((s) => s.runs);
  const rules = useDaemon((s) => s.rules);
  const agents = useDaemon((s) => s.agents);
  let pending = 0;
  let active = 0;
  for (const r of runs.values()) {
    if (r.status === "pending_approval") pending++;
    if (r.status === "queued" || r.status === "running") active++;
  }
  const setParam = (k: string, v: string) => {
    const p = new URLSearchParams(search);
    if (v) p.set(k, v);
    else p.delete(k);
    setSearch(p);
  };
  const select = (id: string | null) => navigate({ view: "runs", runId: id }, { keepSearch: true, replace: runId !== null && id !== null });

  return (
    <div className={cx("split", runId && "has-detail")}>
      <div className="split-main">
        <header className="view-head">
          <h1>Runs</h1>
          <div className="seg" role="tablist" aria-label="Show">
            {TABS.map((t) => (
              <button key={t.key} type="button" role="tab" aria-selected={t.key === tab?.key} className={cx("seg-item", t.key === tab?.key && "is-active")} onClick={() => setParam("show", t.key === "all" ? "" : t.key)}>
                {t.label}
                {t.key === "approval" && pending > 0 && <span className="badge tone-attention">{pending}</span>}
                {t.key === "active" && active > 0 && <span className="badge tone-running">{active}</span>}
              </button>
            ))}
          </div>
          <div className="view-actions">
            <Select aria-label="Rule" value={rule} onChange={(e) => setParam("rule", e.target.value)}>
              <option value="">All rules</option>
              {(rules ?? []).map((r) => (
                <option key={r.config.name} value={r.config.name}>
                  {r.config.name}
                </option>
              ))}
            </Select>
            <Select aria-label="Agent" value={agent} onChange={(e) => setParam("agent", e.target.value)}>
              <option value="">All agents</option>
              {(agents ?? []).map((a) => (
                <option key={a.config.name} value={a.config.name}>
                  {a.config.name}
                </option>
              ))}
            </Select>
          </div>
        </header>
        {error && <Notice tone="error" title="Could not load runs">{error}</Notice>}
        <RunList items={items} selected={runId} onSelect={select} loading={loading} more={more} loadMore={loadMore} emptyTitle={tab?.key === "approval" ? "Nothing is waiting for approval" : "No runs"} />
      </div>
      {runId && <RunDetail key={runId} id={runId} onClose={() => select(null)} />}
    </div>
  );
}

const runRowHeight = () => 30;

export function RunList({
  items,
  selected,
  onSelect,
  loading,
  more,
  loadMore,
  emptyTitle,
}: {
  items: RunSummary[];
  selected: string | null;
  onSelect: (id: string | null) => void;
  loading: boolean;
  more: boolean;
  loadMore: () => void;
  emptyTitle: string;
}) {
  const parent = useRef<HTMLDivElement>(null);
  const now = useNow();
  const getItemKey = useCallback((i: number) => items[i]?.id ?? "more", [items]);
  const getScrollElement = useCallback(() => parent.current, []);
  const virtualizer = useVirtualizer({ count: items.length + (more ? 1 : 0), getScrollElement, estimateSize: runRowHeight, overscan: 10, getItemKey });
  const last = virtualizer.getVirtualItems().at(-1)?.index ?? 0;
  useEffect(() => {
    if (more && !loading && last >= items.length - 10) loadMore();
  }, [last, items.length, more, loading, loadMore]);
  const move = (d: number) => {
    const i = items.findIndex((r) => r.id === selected);
    const j = Math.max(0, Math.min(items.length - 1, i < 0 ? 0 : i + d));
    const r = items[j];
    if (r) {
      onSelect(r.id);
      virtualizer.scrollToIndex(j, { align: "auto" });
    }
  };
  useBindings([
    { keys: "j", label: "Next run", group: "Lists", run: () => move(1) },
    { keys: "k", label: "Previous run", group: "Lists", run: () => move(-1) },
    { keys: "arrowdown", label: "Next run", group: "Lists", run: () => move(1), hidden: true },
    { keys: "arrowup", label: "Previous run", group: "Lists", run: () => move(-1), hidden: true },
    { keys: "escape", label: "Close the detail", group: "Lists", run: () => onSelect(null) },
  ]);
  if (!loading && items.length === 0) {
    return <Empty title={emptyTitle}>Runs appear here when a rule fires and wakes an agent.</Empty>;
  }
  return (
    <div ref={parent} className="feed-list" role="grid" aria-label="Runs, newest first" tabIndex={0}>
      <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
        {virtualizer.getVirtualItems().map((v) => {
          const r = items[v.index];
          return (
            <div key={v.key} className="vrow" style={{ transform: `translateY(${v.start}px)`, height: 30 }}>
              {r ? (
                <div className={cx("row", "run-row", r.id === selected && "is-selected")} role="row" aria-selected={r.id === selected} onClick={() => onSelect(r.id)}>
                  <span role="cell">
                    <Pill tone={runTone(r.status)}>{RUN_STATUS_LABEL[r.status]}</Pill>
                  </span>
                  <span className="c-rule mono" role="cell">
                    {r.rule ?? <span className="faint">(deleted rule)</span>}
                  </span>
                  <span className="c-agent mono" role="cell">
                    {r.agent}
                  </span>
                  <span className="c-when num faint" role="cell" title={r.created_at}>
                    {dayAndTime(r.created_at, now)}
                  </span>
                  <span className="c-dur num" role="cell">
                    {r.started_at ? span(Math.max(0, (r.ended_at ? Date.parse(r.ended_at) : Math.max(now, Date.now())) - Date.parse(r.started_at))) : ""}
                  </span>
                  <span className="c-id mono faint" role="cell">
                    {shortId(r.id)}
                  </span>
                </div>
              ) : (
                <div className="row footer-row faint">{loading ? "Loading…" : `${count(items.length)} runs`}</div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
