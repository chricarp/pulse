import { memo, useEffect, useSyncExternalStore } from "react";

import { useApp } from "../../app/context";
import type { Event, EventDetail } from "../../contracts";
import { deliveryLabel, deliveryTone } from "../../lib/explain";
import { clockTime, count, sourceName } from "../../lib/format";
import { Icon } from "../../ui/icons";
import { Trunc, cx } from "../../ui/primitives";

export function typeFamily(type: string): string {
  const head = type.split(".")[0] ?? "";
  return ["fs", "git", "process", "schedule", "calendar", "ext"].includes(head) ? head : "ext";
}

/** Deliveries for an event, fetched lazily while its row is on screen. */
export function useEventDetail(event: Event): EventDetail | undefined {
  const { deliveries } = useApp();
  const detail = useSyncExternalStore(
    (l) => deliveries.subscribe(event.id, l),
    () => deliveries.get(event.id),
  );
  useEffect(() => {
    deliveries.want(event.id, event.type);
    return () => deliveries.unwant(event.id);
  }, [deliveries, event.id, event.type]);
  return detail;
}

function Markers({ event }: { event: Event }) {
  const detail = useEventDetail(event);
  if (!detail || detail.deliveries.length === 0) return null;
  // The most important outcome first: loop protection, then approvals, fires, the rest.
  const rank = (d: EventDetail["deliveries"][number]) => {
    const t = deliveryTone(d.outcome);
    return t === "loop" ? 0 : t === "attention" ? 1 : t === "ok" ? 2 : t === "waiting" ? 3 : 4;
  };
  const sorted = [...detail.deliveries].sort((a, b) => rank(a) - rank(b));
  const shown = sorted.slice(0, 2);
  const rest = sorted.length - shown.length;
  return (
    <span className="markers">
      {shown.map((d) => {
        const tone = deliveryTone(d.outcome);
        return (
          <span key={d.id} className={cx("marker", `tone-${tone}`)} title={`${d.rule}: ${deliveryLabel(d.outcome)}`}>
            {tone === "loop" && <Icon name="loop" size={11} />}
            <span className="marker-rule">{d.rule}</span>
            <span className="marker-outcome">{deliveryLabel(d.outcome)}</span>
          </span>
        );
      })}
      {rest > 0 && <span className="marker tone-neutral">+{rest}</span>}
    </span>
  );
}

export const EventRow = memo(function EventRow({ event, selected, onSelect }: { event: Event; selected: boolean; onSelect: (id: string) => void }) {
  const family = typeFamily(event.type);
  return (
    <div
      className={cx("row", "event-row", selected && "is-selected")}
      role="row"
      aria-selected={selected}
      data-id={event.id}
      onClick={() => onSelect(event.id)}
    >
      <span className="c-time num" role="cell" title={new Date(event.time).toLocaleString()}>
        {clockTime(event.time)}
      </span>
      <span className={cx("c-type", "mono", `fam-${family}`)} role="cell" title={event.type}>
        {event.type}
      </span>
      <span className="c-source" role="cell" title={event.source}>
        {sourceName(event.source)}
      </span>
      <span className="c-subject" role="cell">
        {event.subject ? <Trunc text={event.subject} /> : <span className="faint">no subject</span>}
      </span>
      <span className="c-flags" role="cell">
        {event.pulsecausationrun && (
          <span
            className={cx("flag", "flag-caused")}
            title={`Caused by an agent run (${event.pulsecausation === "exact" ? "reported by the run itself" : "inferred: it happened in the run's working directory"}). Chain depth ${event.pulsechaindepth}.`}
          >
            <Icon name="agents" size={11} />
            {event.pulsecausation === "exact" ? "by agent" : "by agent?"}
          </span>
        )}
        {event.pulsereplayof && (
          <span className="flag" title={`Replay of ${event.pulsereplayof}`}>
            <Icon name="rerun" size={10} />
            replay
          </span>
        )}
        {event.pulsecoalesced && event.pulsecoalesced > 1 && (
          <span className="flag num" title={`Folded from ${event.pulsecoalesced} raw signals`}>
            ×{count(event.pulsecoalesced)}
          </span>
        )}
      </span>
      <span className="c-markers" role="cell">
        <Markers event={event} />
      </span>
    </div>
  );
});
