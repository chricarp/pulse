// What happened because of each event, for the feed's row markers.
//
// The daemon sends outcomes with the events: every `events.batch` and every `events.query` page
// carries an `EventOutcome` per delivery of its events, and a batch also carries outcomes
// recorded later for events sent earlier (a debounce that elapsed). So the feed never asks per
// row; this store only merges what arrives. The event detail pane, which fetches the full
// deliveries anyway, replaces an event's entry with them.
//
// Bounded: only events with outcomes are kept, at most MAX of them (the oldest stored go first).

import type { EventDetail, EventOutcome } from "../contracts";

const MAX = 30_000;

const keyOf = (o: EventOutcome) => `${o.rule}\u0000${o.run ?? ""}\u0000${JSON.stringify(o.outcome)}`;

export class OutcomeStore {
  private readonly byEvent = new Map<string, EventOutcome[]>();
  private readonly listeners = new Map<string, Set<() => void>>();

  get(id: string): readonly EventOutcome[] | undefined {
    return this.byEvent.get(id);
  }

  subscribe(id: string, l: () => void): () => void {
    let set = this.listeners.get(id);
    if (!set) {
      set = new Set();
      this.listeners.set(id, set);
    }
    set.add(l);
    return () => {
      set.delete(l);
      if (set.size === 0) this.listeners.delete(id);
    };
  }

  /** Outcomes from a batch or a page. A live batch and a page can carry the same ones. */
  add(outcomes: readonly EventOutcome[]): void {
    if (outcomes.length === 0) return;
    const touched = new Set<string>();
    for (const o of outcomes) {
      const prev = this.byEvent.get(o.event) ?? [];
      const k = keyOf(o);
      if (prev.some((p) => keyOf(p) === k)) continue;
      this.set(o.event, [...prev, o]);
      touched.add(o.event);
    }
    for (const id of touched) this.emit(id);
  }

  /** The full deliveries of one event (the detail pane fetched them). */
  put(detail: EventDetail): void {
    const id = detail.event.id;
    this.set(
      id,
      detail.deliveries.map((d) => ({ event: id, rule: d.rule, outcome: d.outcome, run: d.run ?? null })),
    );
    this.emit(id);
  }

  private set(id: string, list: EventOutcome[]): void {
    this.byEvent.delete(id);
    this.byEvent.set(id, list);
    if (this.byEvent.size > MAX) {
      const oldest = this.byEvent.keys().next();
      if (!oldest.done) this.byEvent.delete(oldest.value);
    }
  }

  private emit(id: string): void {
    for (const l of this.listeners.get(id) ?? []) l();
  }
}
