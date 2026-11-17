import { memo, useId, useRef, useSyncExternalStore } from "react";

import { useApp } from "../../app/context";
import type { Event, EventOutcome } from "../../contracts";
import { CAUSATION, currentOutcomes, pickMarkers, type CurrentOutcome } from "../../lib/explain";
import { clockTime, count, sourceName } from "../../lib/format";
import { Icon } from "../../ui/icons";
import { Trunc, cx } from "../../ui/primitives";

export function typeFamily(type: string): string {
  const head = type.split(".")[0] ?? "";
  return ["fs", "git", "process", "schedule", "calendar", "ext"].includes(head) ? head : "ext";
}

/** What an event caused, as the daemon sent it with the event (no request per row). */
export function useOutcomes(event: Event): readonly EventOutcome[] | undefined {
  const { outcomes } = useApp();
  return useSyncExternalStore(
    (l) => outcomes.subscribe(event.id, l),
    () => outcomes.get(event.id),
  );
}

const SLOTS = 2;

function Markers({ event }: { event: Event }) {
  const list = useOutcomes(event);
  const id = useId().replace(/:/g, "");
  const pop = useRef<HTMLDivElement>(null);
  if (!list || list.length === 0) return null;
  const { shown, rest } = pickMarkers(currentOutcomes(list), SLOTS);
  return (
    <span className="markers">
      <span className="markers-shown">
        {shown.map((d) => (
          <Marker key={d.rule} d={d} />
        ))}
      </span>
      {rest.length > 0 && (
        <>
          <button
            type="button"
            className="marker marker-more tone-neutral"
            popoverTarget={`markers-${id}`}
            aria-haspopup="dialog"
            aria-label={`${rest.length} more: ${rest.map((d) => `${d.rule} ${d.label}`).join(", ")}`}
            title={rest.map((d) => `${d.rule}: ${d.label}`).join("\n")}
            onClick={(e) => {
              // Opens the list; it does not select the row.
              e.stopPropagation();
              const r = e.currentTarget.getBoundingClientRect();
              const m = pop.current;
              if (m) {
                m.style.top = `${r.bottom + 4}px`;
                m.style.left = `${Math.max(8, Math.min(r.right - 280, window.innerWidth - 288))}px`;
              }
            }}
          >
            +{rest.length}
          </button>
          <div ref={pop} id={`markers-${id}`} popover="auto" className="menu marker-list" aria-label="What this event caused" onClick={(e) => e.stopPropagation()}>
            {[...shown, ...rest].map((d) => (
              <Marker key={d.rule} d={d} />
            ))}
          </div>
        </>
      )}
    </span>
  );
}

function Marker({ d }: { d: CurrentOutcome }) {
  return (
    <span className={cx("marker", `tone-${d.tone}`)} title={`${d.rule}: ${d.label}`}>
      {d.tone === "loop" && <Icon name="loop" size={11} />}
      <span className="marker-rule">{d.rule}</span>
      <span className="marker-outcome">{d.label}</span>
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
            title={`Caused by an agent run (${CAUSATION[event.pulsecausation ?? "inferred"].label}). Chain depth ${event.pulsechaindepth}.`}
          >
            <Icon name="agents" size={11} />
            {CAUSATION[event.pulsecausation ?? "inferred"].flag}
          </span>
        )}
        {event.pulsereplayof && (
          <span className="flag" title={`Replay of ${event.pulsereplayof}`}>
            <Icon name="rerun" size={10} />
            replay
          </span>
        )}
        {event.pulsecoalesced && event.pulsecoalesced > 1 && (
          <span className="flag num" title={`Folded from ${event.pulsecoalesced} raw signals by the source (only the count is recorded)`}>
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
