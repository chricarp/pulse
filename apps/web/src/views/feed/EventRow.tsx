import { memo, useSyncExternalStore } from "react";

import { useApp } from "../../app/context";
import type { Event, EventOutcome } from "../../contracts";
import { CAUSATION, currentOutcomes, type CurrentOutcome } from "../../lib/explain";
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

function Markers({ event }: { event: Event }) {
  const list = useOutcomes(event);
  if (!list || list.length === 0) return null;
  // The most important outcome first: loop protection, then approvals, fires, the rest.
  const rank = (d: CurrentOutcome) => (d.tone === "loop" ? 0 : d.tone === "attention" ? 1 : d.tone === "ok" ? 2 : d.tone === "waiting" ? 3 : 4);
  const sorted = currentOutcomes(list).sort((a, b) => rank(a) - rank(b));
  const shown = sorted.slice(0, 2);
  const rest = sorted.length - shown.length;
  return (
    <span className="markers">
      {shown.map((d) => (
        <span key={d.rule} className={cx("marker", `tone-${d.tone}`)} title={`${d.rule}: ${d.label}`}>
          {d.tone === "loop" && <Icon name="loop" size={11} />}
          <span className="marker-rule">{d.rule}</span>
          <span className="marker-outcome">{d.label}</span>
        </span>
      ))}
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
