import { useVirtualizer } from "@tanstack/react-virtual";
import { useCallback, useEffect, useRef } from "react";

import { useApp } from "../../app/context";
import { OLDER_MAX, type FeedState } from "../../app/feed";
import { useBindings } from "../../app/keys";
import { isEmptyFilter, type FeedFilter } from "../../lib/filter";
import { clockTime, count } from "../../lib/format";
import type { FeedRow, GapRow } from "../../store/feedBuffer";
import { Icon } from "../../ui/icons";
import { Button, Empty, cx } from "../../ui/primitives";
import { EventRow } from "./EventRow";

export const ROW_HEIGHT = 30;
const rowHeight = () => ROW_HEIGHT;

type Item = FeedRow | { kind: "footer" };

export function FeedList({
  state,
  selectedId,
  onSelect,
  onFilter,
}: {
  state: FeedState;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  onFilter: (f: FeedFilter) => void;
}) {
  const { feed } = useApp();
  const parent = useRef<HTMLDivElement>(null);
  const { rows, older } = state;
  const total = rows.length + older.length + 1;
  const itemAt = (i: number): Item => {
    if (i < rows.length) return rows[i] as FeedRow;
    const e = older[i - rows.length];
    return e ? { kind: "event", event: e } : { kind: "footer" };
  };
  // Stable callbacks: a new one makes the virtualizer recompute every row's position.
  const getItemKey = useCallback(
    (i: number) => {
      if (i < rows.length) {
        const r = rows[i];
        return r?.kind === "event" ? r.event.id : (r?.key ?? i);
      }
      return older[i - rows.length]?.id ?? "footer";
    },
    [rows, older],
  );
  const getScrollElement = useCallback(() => parent.current, []);
  const virtualizer = useVirtualizer({ count: total, getScrollElement, estimateSize: rowHeight, overscan: 10, getItemKey });
  const items = virtualizer.getVirtualItems();
  const lastIndex = items.at(-1)?.index ?? 0;

  // Reaching the bottom loads the next page from the log.
  useEffect(() => {
    if (lastIndex >= total - 25 && !state.olderDone && !state.olderLoading && older.length < OLDER_MAX && rows.length + older.length > 0) {
      void feed.loadOlder();
    }
  }, [lastIndex, total, state.olderDone, state.olderLoading, older.length, rows.length, feed]);

  const indexOf = (id: string | null): number => {
    if (!id) return -1;
    for (let i = 0; i < total; i++) {
      const it = itemAt(i);
      if (it.kind === "event" && it.event.id === id) return i;
    }
    return -1;
  };
  const move = (delta: number) => {
    let i = indexOf(selectedId);
    if (i < 0) i = delta > 0 ? -1 : total;
    for (let j = i + delta; j >= 0 && j < total; j += delta) {
      const it = itemAt(j);
      if (it.kind === "event") {
        onSelect(it.event.id);
        virtualizer.scrollToIndex(j, { align: "auto" });
        return;
      }
    }
  };
  const toNewest = () => {
    virtualizer.scrollToOffset(0);
    feed.setHeld(false);
  };

  useBindings([
    { keys: "j", label: "Next event", group: "Lists", run: () => move(1) },
    { keys: "k", label: "Previous event", group: "Lists", run: () => move(-1) },
    { keys: "arrowdown", label: "Next event", group: "Lists", run: () => move(1), hidden: true },
    { keys: "arrowup", label: "Previous event", group: "Lists", run: () => move(-1), hidden: true },
    { keys: "home", label: "Newest events", group: "Feed", run: toNewest },
    { keys: "escape", label: "Close the detail", group: "Lists", run: () => onSelect(null) },
  ]);

  const frozen = state.paused || state.held;
  const nothing = rows.length === 0 && older.length === 0;

  return (
    <div className="feed-list-wrap">
      {state.mode === "live" && frozen && (
        <button type="button" className="feed-frozen" onClick={() => (state.paused ? feed.setPaused(false) : toNewest())}>
          <Icon name={state.paused ? "pause" : "arrowDown"} size={12} className={state.paused ? undefined : "flip"} />
          {state.paused ? "Live tail paused" : "Scrolled away from the newest"}
          {state.arrivedWhileFrozen > 0 && <span className="num"> · {count(state.arrivedWhileFrozen)} new</span>}
          <span className="feed-frozen-action">{state.paused ? "Resume" : "Show newest"}</span>
        </button>
      )}
      <div
        ref={parent}
        className="feed-list"
        role="grid"
        aria-label="Events, newest first"
        aria-rowcount={total}
        tabIndex={0}
        onScroll={(e) => {
          if (state.mode === "live") feed.setHeld(e.currentTarget.scrollTop > ROW_HEIGHT / 2);
        }}
      >
        {nothing && !state.olderLoading && state.connection !== "subscribing" ? (
          <Empty title={isEmptyFilter(state.filter) ? "No events yet" : "No events match this filter"}>
            {isEmptyFilter(state.filter) ? (
              <p>Pulse shows what its sources see as it happens. Add a source to watch a folder, a repository, a schedule or a calendar.</p>
            ) : (
              <p>New matching events will appear here as they happen.</p>
            )}
          </Empty>
        ) : (
          <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {items.map((v) => {
              const it = itemAt(v.index);
              return (
                <div key={v.key} className="vrow" style={{ transform: `translateY(${v.start}px)`, height: ROW_HEIGHT }} aria-rowindex={v.index + 1}>
                  {it.kind === "event" ? (
                    <EventRow event={it.event} selected={it.event.id === selectedId} onSelect={onSelect} />
                  ) : it.kind === "gap" ? (
                    <Gap gap={it} onShow={(f) => onFilter({ ...state.filter, ...f })} />
                  ) : (
                    <Footer state={state} />
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function Gap({ gap, onShow }: { gap: GapRow; onShow: (f: Partial<FeedFilter>) => void }) {
  const range = { since: gap.olderTime, until: gap.newerTime ?? new Date().toISOString() };
  return (
    <div className={cx("row", "gap-row", gap.reason === "disconnected" && "is-lost")} role="row">
      <Icon name={gap.reason === "disconnected" ? "alert" : "filter"} size={12} />
      <span>
        {gap.reason === "disconnected" ? (
          <>The connection dropped here; events from this time may be missing below.</>
        ) : (
          <>
            <strong className="num">{count(gap.count ?? 0)}</strong> {gap.count === 1 ? "event" : "events"} not shown live
            {gap.newerTime && gap.olderTime && (
              <span className="faint num">
                {" "}
                ({clockTime(gap.olderTime)}–{clockTime(gap.newerTime)})
              </span>
            )}
            . The log has them all.
          </>
        )}
      </span>
      <Button size="sm" variant="ghost" onClick={() => onShow(range)}>
        Show from the log
      </Button>
    </div>
  );
}

function Footer({ state }: { state: FeedState }) {
  const { feed } = useApp();
  let text: string;
  if (state.olderLoading) text = "Loading older events…";
  else if (state.older.length >= OLDER_MAX) text = `Showing the newest ${count(OLDER_MAX + state.rows.length)} events. Narrow the filter or the time range to see further back.`;
  else if (state.olderDone) text = state.rows.length + state.older.length === 0 ? "" : "Start of the log for this filter.";
  else text = "";
  return (
    <div className="row footer-row" role="row">
      {text ? <span className="faint">{text}</span> : !state.olderDone && <Button size="sm" variant="ghost" onClick={() => void feed.loadOlder()}>Load older events</Button>}
    </div>
  );
}

