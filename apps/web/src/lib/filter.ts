// The feed's filter: view state that lives in the URL, maps onto the daemon's `EventFilter`,
// and can be typed as a query (`type:fs.* source:university path:~/University lecture`).

import type { EventFilter } from "../contracts";

export const TIME_PRESETS = ["15m", "1h", "24h", "7d"] as const;
export type TimePreset = (typeof TIME_PRESETS)[number];

const PRESET_MS: Record<TimePreset, number> = {
  "15m": 15 * 60_000,
  "1h": 3_600_000,
  "24h": 86_400_000,
  "7d": 7 * 86_400_000,
};

export interface FeedFilter {
  types: string[];
  sources: string[];
  /** Glob over the subject. */
  subject: string;
  /** Directory or file; events carrying a path at or under it. */
  path: string;
  /** Case-insensitive substring of subject or data. */
  text: string;
  /** A preset relative to now, or absolute bounds (RFC 3339). */
  since: TimePreset | string | null;
  until: string | null;
  causedBy: string | null;
}

export const EMPTY_FILTER: FeedFilter = {
  types: [],
  sources: [],
  subject: "",
  path: "",
  text: "",
  since: null,
  until: null,
  causedBy: null,
};

export function isPreset(v: string | null): v is TimePreset {
  return v !== null && (TIME_PRESETS as readonly string[]).includes(v);
}

export function isEmptyFilter(f: FeedFilter): boolean {
  return (
    f.types.length === 0 &&
    f.sources.length === 0 &&
    !f.subject &&
    !f.path &&
    !f.text &&
    f.since === null &&
    f.until === null &&
    f.causedBy === null
  );
}

/**
 * A filter with an upper bound describes the past only: the feed shows it from the log, with
 * no live tail.
 */
export function isLiveCapable(f: FeedFilter): boolean {
  return f.until === null;
}

function iso(ms: number): string {
  return new Date(ms).toISOString();
}

/** The daemon wants RFC 3339 with an offset; a typed local time like `2026-11-16T09:00` is local. */
function absolute(v: string): string {
  const ms = Date.parse(v);
  return Number.isNaN(ms) ? v : iso(ms);
}

export function toEventFilter(f: FeedFilter, now: number): EventFilter {
  const out: EventFilter = { types: [...f.types], sources: [...f.sources] };
  if (f.subject.trim()) out.subject = f.subject.trim();
  if (f.path.trim()) out.path = f.path.trim();
  if (f.text.trim()) out.text = f.text.trim();
  if (f.since !== null) out.since = isPreset(f.since) ? iso(now - PRESET_MS[f.since]) : absolute(f.since);
  if (f.until !== null) out.until = absolute(f.until);
  if (f.causedBy !== null) out.caused_by = f.causedBy;
  return out;
}

/** For live subscriptions: a relative `since` means nothing for events that have not happened. */
export function toLiveFilter(f: FeedFilter): EventFilter {
  const out = toEventFilter({ ...f, since: null, until: null }, 0);
  return out;
}

export function toSearchParams(f: FeedFilter): URLSearchParams {
  const p = new URLSearchParams();
  for (const t of f.types) p.append("type", t);
  for (const s of f.sources) p.append("source", s);
  if (f.subject) p.set("subject", f.subject);
  if (f.path) p.set("path", f.path);
  if (f.text) p.set("q", f.text);
  if (f.since) p.set("since", f.since);
  if (f.until) p.set("until", f.until);
  if (f.causedBy) p.set("caused_by", f.causedBy);
  return p;
}

export function fromSearchParams(p: URLSearchParams): FeedFilter {
  return {
    types: p.getAll("type").filter(Boolean),
    sources: p.getAll("source").filter(Boolean),
    subject: p.get("subject") ?? "",
    path: p.get("path") ?? "",
    text: p.get("q") ?? "",
    since: p.get("since") || null,
    until: p.get("until") || null,
    causedBy: p.get("caused_by") || null,
  };
}

export function sameFilter(a: FeedFilter, b: FeedFilter): boolean {
  return toSearchParams(a).toString() === toSearchParams(b).toString();
}

const KEYS = ["type", "source", "subject", "path", "since", "until", "run"] as const;

/** Splits on spaces outside double quotes. */
function tokenize(input: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quoted = false;
  let started = false;
  for (const ch of input) {
    if (ch === '"') {
      quoted = !quoted;
      started = true;
      continue;
    }
    if (!quoted && /\s/.test(ch)) {
      if (started) out.push(cur);
      cur = "";
      started = false;
      continue;
    }
    cur += ch;
    started = true;
  }
  if (started) out.push(cur);
  return out;
}

/**
 * Applies a typed query to a filter. `key:value` tokens set fields (`type` and `source` add to
 * their lists); everything else becomes the text search. Unknown keys stay text, so a search
 * for `http://x` still works.
 */
export function applyQuery(base: FeedFilter, query: string): FeedFilter {
  const next: FeedFilter = { ...base, types: [...base.types], sources: [...base.sources] };
  const words: string[] = [];
  for (const tok of tokenize(query)) {
    const colon = tok.indexOf(":");
    const key = colon > 0 ? tok.slice(0, colon) : "";
    const value = colon > 0 ? tok.slice(colon + 1) : "";
    if (!(KEYS as readonly string[]).includes(key) || !value) {
      words.push(tok);
      continue;
    }
    switch (key) {
      case "type":
        if (!next.types.includes(value)) next.types.push(value);
        break;
      case "source":
        if (!next.sources.includes(value)) next.sources.push(value);
        break;
      case "subject":
        next.subject = value;
        break;
      case "path":
        next.path = value;
        break;
      case "since":
        next.since = value;
        break;
      case "until":
        next.until = value;
        break;
      case "run":
        next.causedBy = value;
        break;
    }
  }
  next.text = words.join(" ");
  return next;
}

/** Problems worth saying before the daemon does. */
export function filterProblems(f: FeedFilter): string[] {
  const out: string[] = [];
  if (f.path && !(f.path.startsWith("/") || f.path.startsWith("~"))) {
    out.push("Path must be absolute (start with / or ~).");
  }
  for (const v of [f.since, f.until]) {
    if (v && !isPreset(v) && Number.isNaN(Date.parse(v))) {
      out.push(`"${v}" is not a time. Use 15m, 1h, 24h, 7d or a date like 2026-11-16T09:00.`);
    }
  }
  return out;
}
