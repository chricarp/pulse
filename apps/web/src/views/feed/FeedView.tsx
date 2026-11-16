import { useEffect } from "react";

import { useApp, useDaemon } from "../../app/context";
import { useBindings } from "../../app/keys";
import { navigate, setSearch, useLocation } from "../../app/router";
import { fromSearchParams, isEmptyFilter, toSearchParams, type FeedFilter } from "../../lib/filter";
import { count, plural } from "../../lib/format";
import { useStore } from "../../store/store";
import { Button, Notice, cx } from "../../ui/primitives";
import { EventDetailPane } from "./EventDetail";
import { FeedList } from "./FeedList";
import { FilterBar } from "./FilterBar";

export function FeedView({ eventId }: { eventId: string | null }) {
  const { feed } = useApp();
  const state = useStore(feed.store, (s) => s);
  const { search } = useLocation();
  const connected = useDaemon((s) => s.connection.kind === "open");

  // The URL holds the filter. Coming back to the feed without one restores the last filter.
  useEffect(() => {
    const fromUrl = fromSearchParams(new URLSearchParams(search));
    const current = feed.store.get().filter;
    if (!search && !isEmptyFilter(current)) {
      setSearch(toSearchParams(current));
      feed.setFilter(current);
      return;
    }
    feed.setFilter(fromUrl);
  }, [search, feed]);

  const applyFilter = (f: FeedFilter) => {
    feed.setFilter(f);
    setSearch(toSearchParams(f));
  };
  const select = (id: string | null) => navigate({ view: "feed", eventId: id }, { keepSearch: true, replace: true });

  useBindings([
    { keys: "space", label: "Pause or resume the live tail", group: "Feed", run: () => feed.setPaused(!feed.store.get().paused) },
  ]);

  const live = state.mode === "live";
  const events = state.rows.reduce((n, r) => n + (r.kind === "event" ? 1 : 0), 0) + state.older.length;
  return (
    <div className={cx("split", eventId && "has-detail")}>
      <div className="split-main">
        <header className="view-head">
          <h1>Feed</h1>
          <div className="feed-status" role="status" aria-live="off">
            {live ? (
              <span className={cx("live-dot", state.connection === "live" && !state.paused && "is-live")}>
                {state.connection === "live" ? (state.paused ? "Paused" : "Live") : state.connection === "lost" ? "Reconnecting" : "Connecting"}
              </span>
            ) : (
              <span className="live-dot">From the log</span>
            )}
            <span className="faint num">{plural(events, "event")} shown</span>
            {state.stats.dropped > 0 && (
              <span className="num tone-text-attention" title="Events the daemon summarized out of the live feed during bursts. They are all in the log.">
                {count(state.stats.dropped)} summarized
              </span>
            )}
          </div>
          <div className="view-actions">
            {live ? (
              <Button size="sm" icon={state.paused ? "play" : "pause"} onClick={() => feed.setPaused(!state.paused)} shortcut="space">
                {state.paused ? "Resume live" : "Pause live"}
              </Button>
            ) : (
              <Button size="sm" icon="play" onClick={() => applyFilter({ ...state.filter, until: null, since: null })}>
                Back to live
              </Button>
            )}
          </div>
        </header>
        <FilterBar filter={state.filter} onChange={applyFilter} />
        {state.error && (
          <Notice tone="error" title="The feed could not load" actions={<Button size="sm" onClick={() => feed.retry()} disabled={!connected}>Retry</Button>}>
            {state.error}
          </Notice>
        )}
        <FeedList state={state} selectedId={eventId} onSelect={select} onFilter={applyFilter} />
      </div>
      {eventId && <EventDetailPane id={eventId} onClose={() => select(null)} onFilter={applyFilter} />}
    </div>
  );
}
