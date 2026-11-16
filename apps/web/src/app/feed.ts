// The feed: a live subscription feeding the bounded window, plus older pages from the log.
//
// It lives as long as the app, so switching views keeps the window. Store updates are
// coalesced to one per animation frame, which also means none while the tab is hidden; the
// buffer keeps up regardless.

import type { Event } from "../contracts";
import type { OutcomeStore } from "./outcomes";
import { isLiveCapable, toEventFilter, toLiveFilter, EMPTY_FILTER, sameFilter, type FeedFilter } from "../lib/filter";
import { RpcCallError, type RpcClient, type Subscription } from "../rpc/client";
import { FeedBuffer, type FeedRow, type FeedStats } from "../store/feedBuffer";
import { createStore, type Store } from "../store/store";

export const LIVE_WINDOW = 5000;
/** Older events held at most; past this the view asks for a narrower filter. */
export const OLDER_MAX = 20_000;
const PAGE = 300;

export interface FeedState {
  filter: FeedFilter;
  /** `history`: the filter has an upper bound, so there is no live tail. */
  mode: "live" | "history";
  rows: readonly FeedRow[];
  older: readonly Event[];
  olderDone: boolean;
  olderLoading: boolean;
  stats: FeedStats;
  /** Paused by the user. */
  paused: boolean;
  /** Held because the user scrolled away from the newest events. */
  held: boolean;
  /** Arrived since the view was frozen (shown or dropped). */
  arrivedWhileFrozen: number;
  connection: "subscribing" | "live" | "lost" | "off";
  error: string | null;
}

export class FeedController {
  readonly store: Store<FeedState>;
  private readonly buffer = new FeedBuffer(LIVE_WINDOW);
  private sub: Subscription | null = null;
  private olderCursor: string | null = null;
  private generation = 0;
  private frame: number | null = null;

  constructor(
    private readonly client: RpcClient,
    private readonly outcomes: OutcomeStore,
  ) {
    this.store = createStore<FeedState>({
      filter: EMPTY_FILTER,
      mode: "live",
      rows: [],
      older: [],
      olderDone: false,
      olderLoading: false,
      stats: this.buffer.stats,
      paused: false,
      held: false,
      arrivedWhileFrozen: 0,
      connection: "off",
      error: null,
    });
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") this.publish();
    });
  }

  private get frozen(): boolean {
    const s = this.store.get();
    return s.paused || s.held;
  }

  /** Applies buffer changes to the store at most once per frame. */
  private schedule(): void {
    if (this.frame !== null) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = null;
      this.publish();
    });
  }

  private publish(): void {
    this.store.set((s) => {
      const frozen = s.paused || s.held;
      // Older pages continue below the window. Once the window drops events off its bottom,
      // they no longer join up, so they go (the log still has them).
      const evicted = !frozen && this.buffer.stats.evicted > s.stats.evicted && s.older.length > 0;
      return {
        ...s,
        rows: frozen ? s.rows : this.buffer.rows,
        stats: this.buffer.stats,
        ...(evicted ? { older: [], olderDone: false } : {}),
      };
    });
  }

  private oldestShown(rows: readonly FeedRow[]): string | null {
    for (let i = rows.length - 1; i >= 0; i--) {
      const r = rows[i];
      if (r?.kind === "event") return r.event.id;
    }
    return null;
  }

  setFilter(filter: FeedFilter, force = false): void {
    const s = this.store.get();
    if (!force && sameFilter(filter, s.filter) && (this.sub || s.mode === "history")) return;
    this.generation += 1;
    const gen = this.generation;
    this.sub?.close();
    this.sub = null;
    this.buffer.reset();
    this.olderCursor = null;
    const live = isLiveCapable(filter);
    this.store.set({
      ...s,
      filter,
      mode: live ? "live" : "history",
      rows: [],
      older: [],
      olderDone: false,
      olderLoading: false,
      stats: this.buffer.stats,
      held: false,
      arrivedWhileFrozen: 0,
      connection: live ? "subscribing" : "off",
      error: null,
    });
    if (!live) {
      void this.loadOlder();
      return;
    }
    this.sub = this.client.subscribe(
      "events.subscribe",
      () => ({ filter: toLiveFilter(filter) }),
      (batch) => {
        if (gen !== this.generation) return;
        this.outcomes.add(batch.outcomes);
        this.buffer.applyBatch(batch.events, batch.dropped);
        if (this.frozen) {
          this.store.set((st) => ({ ...st, arrivedWhileFrozen: st.arrivedWhileFrozen + batch.events.length + batch.dropped }));
        }
        this.schedule();
      },
      {
        onSubscribed: () => {
          if (gen !== this.generation) return;
          this.store.set((st) => ({ ...st, connection: "live", error: null }));
          // Subscribed first, then the newest page: the buffer removes the overlap.
          void this.client
            .call("events.query", { filter: toEventFilter(filter, Date.now()), order: "newest_first", limit: PAGE })
            .then((page) => {
              if (gen !== this.generation) return;
              this.outcomes.add(page.outcomes);
              this.buffer.mergeHistory(page.events);
              if (!page.next_cursor) this.store.set((st) => ({ ...st, olderDone: true }));
              this.olderCursor = page.next_cursor ?? null;
              this.schedule();
            })
            .catch((err: unknown) => this.fail(gen, err));
        },
        onLost: () => {
          if (gen !== this.generation) return;
          this.buffer.markDisconnected();
          this.store.set((st) => ({ ...st, connection: "lost" }));
          this.schedule();
        },
        onError: (err) => this.fail(gen, err),
      },
    );
  }

  private fail(gen: number, err: unknown): void {
    if (gen !== this.generation) return;
    if (err instanceof RpcCallError && err.kind === "disconnected") return;
    this.store.set((st) => ({ ...st, error: err instanceof Error ? err.message : String(err), olderLoading: false }));
  }

  /** The next page below whatever is shown. */
  async loadOlder(): Promise<void> {
    const s = this.store.get();
    if (s.olderLoading || s.olderDone || s.older.length >= OLDER_MAX) return;
    const gen = this.generation;
    // Continue below the oldest thing shown: an older page, else the live window.
    const cursor = s.older.at(-1)?.id ?? this.oldestShown(s.rows) ?? this.olderCursor;
    if (s.mode === "live" && cursor === null && this.store.get().connection !== "live") return;
    this.store.set((st) => ({ ...st, olderLoading: true }));
    try {
      const page = await this.client.call("events.query", {
        filter: toEventFilter(s.filter, Date.now()),
        order: "newest_first",
        limit: PAGE,
        ...(cursor ? { cursor } : {}),
      });
      if (gen !== this.generation) return;
      this.outcomes.add(page.outcomes);
      const fresh = page.events.filter((e) => !this.buffer.has(e.id));
      this.store.set((st) => ({
        ...st,
        older: [...st.older, ...fresh].slice(0, OLDER_MAX),
        olderDone: !page.next_cursor,
        olderLoading: false,
      }));
    } catch (err) {
      this.fail(gen, err);
    }
  }

  setPaused(paused: boolean): void {
    this.store.set((s) => ({ ...s, paused, arrivedWhileFrozen: paused || s.held ? s.arrivedWhileFrozen : 0 }));
    if (!this.frozen) this.publish();
  }

  /** The list scrolled away from (or back to) the newest events. */
  setHeld(held: boolean): void {
    if (this.store.get().held === held) return;
    this.store.set((s) => ({ ...s, held, arrivedWhileFrozen: held || s.paused ? s.arrivedWhileFrozen : 0 }));
    if (!this.frozen) this.publish();
  }

  /** Restart from scratch with the same filter (after an error). */
  retry(): void {
    this.setFilter(this.store.get().filter, true);
  }
}
