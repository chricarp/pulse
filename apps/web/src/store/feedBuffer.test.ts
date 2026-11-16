import { describe, expect, it } from "vitest";

import type { Event } from "../contracts";
import { FeedBuffer, type FeedRow } from "./feedBuffer";

// Ids only need to sort like ULIDs; zero-padded numbers do.
function ev(n: number): Event {
  return {
    specversion: "1.0",
    id: String(n).padStart(26, "0"),
    type: "fs.file.created",
    source: "pulse://fs/test",
    subject: `/tmp/${n}`,
    time: new Date(1_700_000_000_000 + n * 1000).toISOString(),
    data: {},
    pulsechaindepth: 0,
  };
}
const range = (from: number, to: number) =>
  Array.from({ length: to - from + 1 }, (_, i) => ev(from + i));

/** `12 11 [gap 3] 7` */
function shape(rows: readonly FeedRow[]): string {
  return rows
    .map((r) =>
      r.kind === "event"
        ? String(Number(r.event.id))
        : `[${r.reason === "dropped" ? "gap" : "lost"} ${r.count ?? "?"}]`,
    )
    .join(" ");
}

describe("FeedBuffer", () => {
  it("keeps the newest events first and the window bounded", () => {
    const b = new FeedBuffer(5);
    b.applyBatch(range(1, 3), 0);
    b.applyBatch(range(4, 8), 0);
    expect(shape(b.rows)).toBe("8 7 6 5 4");
    expect(b.stats).toEqual({ received: 8, dropped: 0, evicted: 3 });
    expect(b.eventCount).toBe(5);
    expect(b.oldest?.id).toBe(ev(4).id);
  });

  it("marks where a batch left events out, below the events it carried", () => {
    const b = new FeedBuffer(100);
    b.applyBatch(range(1, 2), 0);
    b.applyBatch(range(9, 10), 6);
    expect(shape(b.rows)).toBe("10 9 [gap 6] 2 1");
    expect(b.stats.dropped).toBe(6);
  });

  it("merges consecutive drops into one gap, including a batch with only a count", () => {
    const b = new FeedBuffer(100);
    b.applyBatch(range(1, 1), 0);
    b.applyBatch([], 4);
    b.applyBatch(range(8, 9), 2);
    expect(shape(b.rows)).toBe("9 8 [gap 6] 1");
  });

  it("de-duplicates live events against a history page and fills gaps with what it finds", () => {
    const b = new FeedBuffer(100);
    // Subscribed first; the first batch came during a storm.
    b.applyBatch(range(901, 1000).slice(-3), 997);
    expect(shape(b.rows)).toBe("1000 999 998 [gap 997]");
    // Then the newest page from the log: overlaps the live events, and reaches into the gap.
    const page = range(990, 1000).reverse();
    b.mergeHistory(page);
    expect(shape(b.rows)).toBe("1000 999 998 [gap 989] 997 996 995 994 993 992 991 990");
    expect(b.eventCount).toBe(11);
  });

  it("removes a gap once history has filled it completely", () => {
    const b = new FeedBuffer(100);
    b.applyBatch(range(1, 1), 0);
    b.applyBatch(range(5, 5), 3);
    b.mergeHistory(range(1, 5).reverse());
    expect(shape(b.rows)).toBe("5 4 3 2 1");
  });

  it("drops gaps that fall out of the window with the events below them", () => {
    const b = new FeedBuffer(3);
    b.applyBatch(range(1, 2), 0);
    b.applyBatch(range(10, 10), 7);
    expect(shape(b.rows)).toBe("10 [gap 7] 2 1");
    b.applyBatch(range(11, 12), 0);
    expect(shape(b.rows)).toBe("12 11 10");
  });

  it("marks a disconnection once, and closes it when history reaches below it", () => {
    const b = new FeedBuffer(100);
    b.applyBatch(range(1, 3), 0);
    b.markDisconnected();
    b.markDisconnected();
    expect(shape(b.rows)).toBe("[lost ?] 3 2 1");
    // A page that does not reach the last known event leaves the gap: events may be missing.
    b.mergeHistory(range(10, 12).reverse());
    expect(shape(b.rows)).toBe("12 11 10 [lost ?] 3 2 1");
    // A page that reaches it proves nothing is missing.
    b.mergeHistory(range(3, 12).reverse());
    expect(shape(b.rows)).toBe("12 11 10 9 8 7 6 5 4 3 2 1");
  });

  it("returns a new rows array on every change and leaves earlier arrays alone", () => {
    const b = new FeedBuffer(100);
    b.applyBatch(range(1, 1), 0);
    b.applyBatch(range(5, 5), 3);
    const before = b.rows;
    const snapshot = shape(before);
    b.mergeHistory([ev(4)]);
    expect(b.rows).not.toBe(before);
    expect(shape(before)).toBe(snapshot);
  });

  it("stays bounded under a storm of batches", () => {
    const b = new FeedBuffer(1000);
    let n = 0;
    for (let batch = 0; batch < 200; batch++) {
      const events = range(n + 1, n + 100);
      n += 100 + 400; // 400 dropped per batch
      b.applyBatch(events, 400);
    }
    expect(b.eventCount).toBe(1000);
    expect(b.rows.length).toBeLessThanOrEqual(1000 + 10);
    expect(b.stats.received).toBe(200 * 500);
    expect(b.stats.dropped).toBe(200 * 400);
  });
});
