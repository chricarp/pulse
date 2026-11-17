// What happened because of each event, for the feed's row markers.
//
// The daemon sends outcomes with the events: every `events.batch` and every `events.query` page
// carries an `EventOutcome` per delivery of its events, and a batch also carries outcomes
// recorded later for events sent earlier (a debounce that elapsed). Each outcome carries the
// status its run had when the daemon sent it; later run changes arrive on `runs.subscribe` and
// are applied here (`applyRuns`), so a marker says what became of a delivery, not only what was
// decided. So the feed never asks per row; this store only merges what arrives, including the
// full deliveries the event detail pane fetches anyway.
//
// Bounded: only events with outcomes are kept, at most MAX of them (the oldest stored go first).

import type { EventDetail, EventOutcome, RunStatus, RunSummary } from "../contracts";
import { laterStatus } from "./daemon";

const MAX = 30_000;

export class OutcomeStore {
  private readonly byEvent = new Map<string, EventOutcome[]>();
  private readonly listeners = new Map<string, Set<() => void>>();

  /**
   * `knownRun` is the run index the app keeps anyway: a run change can arrive before the batch
   * that carries the outcome which created the run, and the outcome then takes the later state.
   */
  constructor(private readonly knownRun: (id: string) => RunStatus | undefined = () => undefined) {}

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
    const touched = new Map<string, EventOutcome[]>();
    for (const o of outcomes) {
      const list = touched.get(o.event) ?? [...(this.byEvent.get(o.event) ?? [])];
      touched.set(o.event, merge(list, [this.withKnownRun(o)]));
    }
    for (const [id, list] of touched) {
      if (!same(this.byEvent.get(id), list)) {
        this.set(id, list);
        this.emit(id);
      }
    }
  }

  /**
   * The full deliveries of one event (the detail pane fetched them), with its runs. Deliveries
   * are only ever added, so this merges: an outcome that arrived in a batch after the detail was
   * read (a debounce elapsing) is kept, and a run state never moves backwards.
   */
  put(detail: EventDetail): void {
    const id = detail.event.id;
    const runs = new Map(detail.runs.map((r) => [r.id, r.status]));
    const fetched: EventOutcome[] = detail.deliveries.map((d) => ({
      delivery: d.id,
      event: id,
      rule: d.rule,
      outcome: d.outcome,
      run: d.run ?? null,
      run_status: d.run ? (runs.get(d.run) ?? null) : null,
    }));
    const list = merge([...(this.byEvent.get(id) ?? [])], fetched.map((o) => this.withKnownRun(o)));
    this.set(id, list);
    this.emit(id);
  }

  /** Run changes from `runs.subscribe`: the outcomes that created those runs take their state. */
  applyRuns(runs: readonly RunSummary[]): void {
    const touched = new Set<string>();
    for (const r of runs) {
      const list = this.byEvent.get(r.event);
      const i = list?.findIndex((o) => o.run === r.id) ?? -1;
      if (!list || i < 0) continue;
      const o = list[i] as EventOutcome;
      const status = laterStatus(o.run_status ?? undefined, r.status);
      if (status === (o.run_status ?? null)) continue;
      const next = [...list];
      next[i] = { ...o, run_status: status };
      this.byEvent.set(r.event, next);
      touched.add(r.event);
    }
    for (const id of touched) this.emit(id);
  }

  private withKnownRun(o: EventOutcome): EventOutcome {
    const known = o.run ? this.knownRun(o.run) : undefined;
    if (!known) return o;
    const status = laterStatus(o.run_status ?? undefined, known);
    return status === (o.run_status ?? null) ? o : { ...o, run_status: status };
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

/** Merges by delivery id, in the order deliveries were recorded; a run's state keeps the later one. */
function merge(list: EventOutcome[], incoming: readonly EventOutcome[]): EventOutcome[] {
  for (const o of incoming) {
    const i = list.findIndex((p) => p.delivery === o.delivery);
    if (i < 0) {
      list.push(o);
      continue;
    }
    const p = list[i] as EventOutcome;
    const status = laterStatus(p.run_status ?? undefined, o.run_status ?? undefined);
    if (status !== (p.run_status ?? null)) list[i] = { ...p, run_status: status };
  }
  return list.sort((a, b) => (a.delivery < b.delivery ? -1 : a.delivery > b.delivery ? 1 : 0));
}

function same(a: readonly EventOutcome[] | undefined, b: readonly EventOutcome[]): boolean {
  return !!a && a.length === b.length && a.every((o, i) => o === b[i]);
}
