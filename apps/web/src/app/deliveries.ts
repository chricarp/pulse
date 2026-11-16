// What happened because of each event, for the feed's row markers.
//
// Live batches carry events, not deliveries, so the feed asks `events.get` for the rows a user
// can actually see: only events whose type some rule listens to (deliveries need a matching
// rule), only after a row has stayed on screen briefly (a storm scrolling past asks for
// nothing), a few at a time, and cached with a bound.

import type { EventDetail } from "../contracts";
import { typeGlobMatches } from "../lib/syntax";
import type { RpcClient } from "../rpc/client";

const CACHE_MAX = 3000;
const SETTLE_MS = 250;
const CONCURRENCY = 4;

export class DeliveryCache {
  private readonly cache = new Map<string, EventDetail>();
  private readonly wanted = new Map<string, number>();
  private readonly inFlight = new Set<string>();
  private readonly listeners = new Map<string, Set<() => void>>();
  private timer: ReturnType<typeof setTimeout> | null = null;
  private ruleTypes: string[] = [];

  constructor(private readonly client: RpcClient) {}

  /** The `on` globs of every rule that can route. */
  setRuleTypes(globs: string[]): void {
    this.ruleTypes = globs;
  }

  relevant(type: string): boolean {
    return this.ruleTypes.some((g) => typeGlobMatches(g, type));
  }

  get(id: string): EventDetail | undefined {
    return this.cache.get(id);
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

  /** A row for this event is on screen. */
  want(id: string, type: string): void {
    if (this.cache.has(id) || this.inFlight.has(id) || !this.relevant(type)) return;
    if (!this.wanted.has(id)) this.wanted.set(id, performance.now());
    this.arm();
  }

  unwant(id: string): void {
    this.wanted.delete(id);
  }

  /** Something may have changed for this event (a run it caused changed state). */
  invalidate(id: string): void {
    if (!this.cache.has(id)) return;
    this.cache.delete(id);
    if (this.listeners.has(id)) {
      this.wanted.set(id, 0);
      this.arm();
    }
  }

  /** Detail fetched elsewhere (the detail pane); share it. */
  put(detail: EventDetail): void {
    this.store(detail);
  }

  private store(detail: EventDetail): void {
    const id = detail.event.id;
    this.cache.delete(id);
    this.cache.set(id, detail);
    if (this.cache.size > CACHE_MAX) {
      const oldest = this.cache.keys().next();
      if (!oldest.done) this.cache.delete(oldest.value);
    }
    for (const l of this.listeners.get(id) ?? []) l();
  }

  private arm(): void {
    if (this.timer !== null) return;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.pump();
    }, SETTLE_MS);
  }

  private pump(): void {
    const now = performance.now();
    for (const [id, since] of this.wanted) {
      if (this.inFlight.size >= CONCURRENCY) break;
      if (now - since < SETTLE_MS) continue;
      this.wanted.delete(id);
      this.inFlight.add(id);
      this.client
        .call("events.get", { id })
        .then((d) => this.store(d))
        .catch(() => {
          // Pruned or unreachable: the row just shows no markers.
        })
        .finally(() => {
          this.inFlight.delete(id);
          if (this.wanted.size > 0) this.arm();
        });
    }
    if (this.wanted.size > 0) this.arm();
  }
}
