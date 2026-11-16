import { useRef, useState } from "react";

import { useDaemon } from "../../app/context";
import { useBindings } from "../../app/keys";
import { EMPTY_FILTER, TIME_PRESETS, applyQuery, filterProblems, isEmptyFilter, isPreset, type FeedFilter } from "../../lib/filter";
import { dateTime, visible } from "../../lib/format";
import { Icon } from "../../ui/icons";
import { Kbd, Menu, cx } from "../../ui/primitives";

export function FilterBar({ filter, onChange }: { filter: FeedFilter; onChange: (f: FeedFilter) => void }) {
  const [query, setQuery] = useState(filter.text);
  const [lastText, setLastText] = useState(filter.text);
  const input = useRef<HTMLInputElement>(null);
  const types = useDaemon((s) => s.eventTypes);
  const sources = useDaemon((s) => s.sources);
  if (filter.text !== lastText) {
    // The filter changed from outside (URL, a "filter by" action): show its text.
    setLastText(filter.text);
    setQuery(filter.text);
  }

  useBindings([
    {
      keys: "/",
      label: "Focus the filter",
      group: "Feed",
      run: () => {
        input.current?.focus();
        input.current?.select();
      },
    },
  ]);

  const apply = () => {
    const next = applyQuery({ ...filter, text: "" }, query);
    onChange(next);
    setQuery(next.text);
  };
  const problems = filterProblems(filter);
  const chips: { key: string; label: string; remove: () => void }[] = [];
  for (const t of filter.types) chips.push({ key: `t-${t}`, label: `type: ${t}`, remove: () => onChange({ ...filter, types: filter.types.filter((x) => x !== t) }) });
  for (const s of filter.sources) chips.push({ key: `s-${s}`, label: `source: ${s}`, remove: () => onChange({ ...filter, sources: filter.sources.filter((x) => x !== s) }) });
  if (filter.path) chips.push({ key: "path", label: `under: ${filter.path}`, remove: () => onChange({ ...filter, path: "" }) });
  if (filter.subject) chips.push({ key: "subject", label: `subject: ${filter.subject}`, remove: () => onChange({ ...filter, subject: "" }) });
  if (filter.since) chips.push({ key: "since", label: isPreset(filter.since) ? `last ${filter.since}` : `since ${dateTime(filter.since)}`, remove: () => onChange({ ...filter, since: null }) });
  if (filter.until) chips.push({ key: "until", label: `until ${dateTime(filter.until)}`, remove: () => onChange({ ...filter, until: null }) });
  if (filter.causedBy) chips.push({ key: "run", label: `caused by run …${filter.causedBy.slice(-8)}`, remove: () => onChange({ ...filter, causedBy: null }) });

  const families = ["fs.*", "git.*", "process.*", "schedule.*", "calendar.*", "ext.*"];
  const typeItems = [...new Set([...families, ...(types ?? []).map((t) => t.type)])].filter((t) => !filter.types.includes(t));

  return (
    <div className="filterbar">
      <div className={cx("filter-input", problems.length > 0 && "is-invalid")}>
        <Icon name="search" size={14} />
        <input
          id="feed-query"
          ref={input}
          value={query}
          spellCheck={false}
          autoComplete="off"
          aria-label="Filter events. Type text, or type:fs.* source:name path:~/dir since:1h, then Enter"
          placeholder="Filter: text, type:fs.* source:site path:~/code since:1h"
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              apply();
            } else if (e.key === "Escape") {
              e.preventDefault();
              if (query !== filter.text) setQuery(filter.text);
              else input.current?.blur();
            }
          }}
        />
        {query !== filter.text ? <span className="filter-pending">Enter to apply</span> : <Kbd keys="/" subtle />}
      </div>
      <Menu
        label="Type"
        icon="filter"
        items={typeItems.map((t) => ({ label: t, onSelect: () => onChange({ ...filter, types: [...filter.types, t] }) }))}
      />
      <Menu
        label="Source"
        items={(sources ?? []).map((s) => ({ label: s.config.name, hint: s.config.kind, onSelect: () => onChange({ ...filter, sources: [...filter.sources, s.config.name] }) }))}
      />
      <Menu
        label="Time"
        icon="clock"
        items={[
          ...TIME_PRESETS.map((p) => ({ label: `Last ${p}`, onSelect: () => onChange({ ...filter, since: p, until: null }) })),
          { label: "Any time", onSelect: () => onChange({ ...filter, since: null, until: null }) },
        ]}
      />
      {chips.length > 0 && (
        <ul className="chips filter-chips" aria-label="Active filters">
          {chips.map((c) => (
            <li key={c.key} className="chip">
              <span className="mono">{visible(c.label, { singleLine: true })}</span>
              <button type="button" aria-label={`Remove ${c.label}`} onClick={c.remove}>
                <Icon name="x" size={10} />
              </button>
            </li>
          ))}
          {!isEmptyFilter({ ...filter, text: "" }) && (
            <li>
              <button type="button" className="link-btn" onClick={() => onChange(EMPTY_FILTER)}>
                Clear all
              </button>
            </li>
          )}
        </ul>
      )}
      {problems.length > 0 && (
        <p className="filter-problems" role="alert">
          {problems.join(" ")}
        </p>
      )}
    </div>
  );
}
