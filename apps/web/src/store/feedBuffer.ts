// The live window of the feed: the newest events, newest first, bounded.
//
// Rows are events plus gap markers. A gap says "events belong here that this window does not
// hold": either the daemon left them out of a live batch (`dropped`, a count) or the connection
// was down (`count: null`, unknown). Gaps are honest placeholders, not errors; the log has every
// event and the UI offers to show the range from it.
//
// Event ids are ULIDs assigned by one daemon, so they sort in log order. Every insert is a merge
// by id, which makes live batches and history pages commute and removes duplicates when the two
// overlap (subscribe first, then fetch the newest page).

import type { Event } from "../contracts";

export interface GapRow {
  kind: "gap";
  /** Stable key for rendering. */
  key: string;
  /** Events missing here; `null` when unknown (the connection was down). */
  count: number | null;
  /** The event just above the gap (newer), if the window still holds one. */
  newerId: string | null;
  newerTime: string | null;
  /** The event just below the gap (older), once known. */
  olderTime: string | null;
  reason: "dropped" | "disconnected";
}

export interface EventRow {
  kind: "event";
  event: Event;
}

export type FeedRow = EventRow | GapRow;

export interface FeedStats {
  /** Events received live (shown or not). */
  received: number;
  /** Events the daemon left out of live batches. */
  dropped: number;
  /** Events pushed out of the window by newer ones (still in the log). */
  evicted: number;
}

let gapSeq = 0;

export class FeedBuffer {
  private list: FeedRow[] = [];
  private ids = new Set<string>();
  private statsValue: FeedStats = { received: 0, dropped: 0, evicted: 0 };
  /** Bumped on every change, so views can tell when to re-read. */
  version = 0;

  constructor(readonly capacity: number) {}

  get rows(): readonly FeedRow[] {
    return this.list;
  }

  get stats(): FeedStats {
    return this.statsValue;
  }

  get eventCount(): number {
    return this.ids.size;
  }

  /** The oldest event in the window: where "older from the log" continues. */
  get oldest(): Event | null {
    for (let i = this.list.length - 1; i >= 0; i--) {
      const r = this.list[i];
      if (r?.kind === "event") return r.event;
    }
    return null;
  }

  get newest(): Event | null {
    for (const r of this.list) if (r.kind === "event") return r.event;
    return null;
  }

  has(id: string): boolean {
    return this.ids.has(id);
  }

  reset(): void {
    this.list = [];
    this.ids = new Set();
    this.statsValue = { received: 0, dropped: 0, evicted: 0 };
    this.version += 1;
  }

  /** An `events.batch`: `events` oldest first, `dropped` left out before them. */
  applyBatch(events: readonly Event[], dropped: number): void {
    this.statsValue = {
      ...this.statsValue,
      received: this.statsValue.received + events.length + dropped,
      dropped: this.statsValue.dropped + dropped,
    };
    const newestFirst = [...events].reverse();
    this.merge(newestFirst);
    if (dropped > 0) {
      const anchor = newestFirst.at(-1) ?? null;
      this.insertGap(anchor, dropped, "dropped");
    }
    this.trim();
    this.version += 1;
  }

  /**
   * A page from `events.query` (newest first). Events already present are skipped. A page that
   * reaches below a disconnection gap proves nothing is missing there, so the gap closes.
   */
  mergeHistory(newestFirst: readonly Event[]): void {
    if (newestFirst.length === 0) return;
    const pageOldest = newestFirst.at(-1)?.id;
    this.merge(newestFirst);
    if (pageOldest !== undefined) {
      const next: FeedRow[] = [];
      for (let i = 0; i < this.list.length; i++) {
        const row = this.list[i];
        if (!row) continue;
        if (row.kind === "gap" && row.reason === "disconnected") {
          const below = this.nextEvent(i + 1);
          if (below === null || below.id >= pageOldest) continue;
        }
        if (row.kind === "gap" && row.count === 0) continue;
        next.push(row);
      }
      this.list = next;
    }
    this.trim();
    this.version += 1;
  }

  /** The connection dropped: anything may have happened since the newest event. */
  markDisconnected(): void {
    const top = this.list[0];
    if (top?.kind === "gap" && top.reason === "disconnected") return;
    const newest = this.newest;
    this.list = [
      {
        kind: "gap",
        key: `gap-${++gapSeq}`,
        count: null,
        newerId: null,
        newerTime: null,
        olderTime: newest?.time ?? null,
        reason: "disconnected",
      },
      ...this.list,
    ];
    this.version += 1;
  }

  // ---- internals ----

  private nextEvent(from: number): Event | null {
    for (let i = from; i < this.list.length; i++) {
      const r = this.list[i];
      if (r?.kind === "event") return r.event;
    }
    return null;
  }

  /** Merges newest-first events into the newest-first list by id, skipping duplicates. */
  private merge(incoming: readonly Event[]): void {
    if (incoming.length === 0) return;
    const out: FeedRow[] = [];
    let i = 0;
    let j = 0;
    // The gap most recently passed with no known event below it yet. A new event placed there
    // is one of the events the gap counted.
    let openGap: GapRow | null = null;
    const old = this.list;
    while (i < old.length || j < incoming.length) {
      const row = old[i];
      const ev = incoming[j];
      if (row?.kind === "gap" && row.newerId === null && ev !== undefined && !this.ids.has(ev.id)) {
        // A gap with no known event above it (a disconnection, or a batch that carried only a
        // count). Events newer than the event below it go above it: they arrived after it.
        const below = this.nextEvent(i + 1);
        if (below === null || ev.id > below.id) {
          out.push({ kind: "event", event: ev });
          this.ids.add(ev.id);
          j++;
          continue;
        }
      }
      if (row?.kind === "gap") {
        // Copied, so a view holding the previous rows never sees them change underneath it.
        const copy: GapRow = { ...row };
        out.push(copy);
        openGap = copy;
        i++;
        continue;
      }
      if (ev === undefined) {
        if (row) out.push(row);
        i++;
        continue;
      }
      if (this.ids.has(ev.id)) {
        j++;
        continue;
      }
      if (row === undefined || ev.id > row.event.id) {
        out.push({ kind: "event", event: ev });
        this.ids.add(ev.id);
        if (openGap && openGap.count !== null && openGap.count > 0) {
          openGap.count -= 1;
          if (openGap.olderTime === null || ev.time < openGap.olderTime) openGap.olderTime = ev.time;
        }
        j++;
      } else {
        out.push(row);
        openGap = null;
        i++;
      }
    }
    this.list = out;
  }

  private insertGap(anchor: Event | null, count: number, reason: GapRow["reason"]): void {
    let at = 0;
    if (anchor) {
      const idx = this.list.findIndex((r) => r.kind === "event" && r.event.id === anchor.id);
      if (idx < 0) return; // Already evicted; nothing to anchor to.
      at = idx + 1;
    }
    const below = this.list[at];
    if (below?.kind === "gap" && below.reason === reason && below.count !== null) {
      const merged: GapRow = {
        ...below,
        count: below.count + count,
        newerId: anchor?.id ?? below.newerId,
        newerTime: anchor?.time ?? below.newerTime,
      };
      this.list = [...this.list.slice(0, at), merged, ...this.list.slice(at + 1)];
      return;
    }
    const olderNeighbour = this.nextEvent(at);
    const gap: GapRow = {
      kind: "gap",
      key: `gap-${++gapSeq}`,
      count,
      newerId: anchor?.id ?? null,
      newerTime: anchor?.time ?? null,
      olderTime: olderNeighbour?.time ?? null,
      reason,
    };
    this.list = [...this.list.slice(0, at), gap, ...this.list.slice(at)];
  }

  private trim(): void {
    if (this.ids.size <= this.capacity) return;
    let seen = 0;
    let cut = this.list.length;
    for (let i = 0; i < this.list.length; i++) {
      const r = this.list[i];
      if (r?.kind !== "event") continue;
      seen += 1;
      if (seen === this.capacity) {
        cut = i + 1;
        break;
      }
    }
    const removed = this.list.slice(cut);
    let evicted = 0;
    for (const r of removed) {
      if (r.kind === "event") {
        this.ids.delete(r.event.id);
        evicted += 1;
      }
    }
    this.list = this.list.slice(0, cut);
    this.statsValue = { ...this.statsValue, evicted: this.statsValue.evicted + evicted };
  }
}
