import { describe, expect, it } from "vitest";

import type { RunSummary } from "./contracts.ts";
import { BreakerNotices, type Notice, RunNotices, Throttle, type Timers } from "./notify.ts";

const run = (id: string, status: RunSummary["status"], extra: Partial<RunSummary> = {}): RunSummary => ({
  id,
  rule: "summarize",
  agent: "claude",
  event: `ev-${id}`,
  status,
  created_at: "2026-11-16T10:00:00.000Z",
  chain_depth: 1,
  ...extra,
});

describe("RunNotices", () => {
  it("announces approvals, failures and timeouts once each", () => {
    const n = new RunNotices();
    const first = n.notices([
      run("a", "pending_approval"),
      run("b", "failed", { exit_code: 2 }),
      run("c", "timed_out"),
      run("d", "succeeded"),
      run("e", "running"),
      run("f", "cancelled"),
      run("g", "interrupted"),
      run("h", "rejected"),
    ]);
    expect(first.map((x) => [x.kind, x.title, x.route])).toEqual([
      ["approval", "Approval needed: summarize", { kind: "run", id: "a" }],
      ["failed", "Run failed: summarize", { kind: "run", id: "b" }],
      ["failed", "Run timed out: summarize", { kind: "run", id: "c" }],
    ]);
    expect(first[1]?.body).toBe("claude exited with code 2.");
    expect(first[0]?.event).toBe("ev-a");
    // The same states again (a coalesced push repeating them): nothing new.
    expect(n.notices([run("a", "pending_approval"), run("b", "failed")])).toEqual([]);
    // A new state of a known run is new.
    expect(n.notices([run("a", "failed")]).map((x) => x.kind)).toEqual(["failed"]);
  });

  it("does not announce what was already so when the shell connected", () => {
    const n = new RunNotices();
    n.seed([run("a", "pending_approval"), run("b", "failed")]);
    expect(n.notices([run("a", "pending_approval"), run("b", "failed")])).toEqual([]);
  });

  it("ignores shapes it does not know and stays bounded", () => {
    const n = new RunNotices(3);
    expect(n.notices([null, { id: 1 }, { ...run("x", "failed"), status: "exploded" }])).toEqual([]);
    for (const id of ["1", "2", "3", "4"]) n.notices([run(id, "failed")]);
    // "1" was forgotten (capacity 3), so it would be announced again; "4" was not.
    expect(n.notices([run("1", "failed"), run("4", "failed")]).map((x) => x.route)).toEqual([{ kind: "run", id: "1" }]);
  });

  it("keeps untrusted names to one bounded line", () => {
    const n = new RunNotices();
    const [notice] = n.notices([run("a", "pending_approval", { rule: `evil\nrule‮${"x".repeat(200)}` })]);
    expect(notice?.title).not.toMatch(/[\n‮]/);
    expect(Array.from(notice?.title ?? "").length).toBeLessThanOrEqual("Approval needed: ".length + 60);
  });
});

describe("BreakerNotices", () => {
  const now = Date.parse("2026-11-16T10:00:00Z");
  const rule = (name: string, until: string | null) => ({ config: { name }, state: { circuit_open_until: until } });
  const fmt = (iso: string) => iso.slice(11, 16);

  it("announces a breaker when it trips, not while it stays open", () => {
    const b = new BreakerNotices();
    b.seed([rule("old", "2026-11-16T11:00:00Z"), rule("hot", null)], now);
    expect(b.notices([rule("old", "2026-11-16T11:00:00Z"), rule("hot", null)], now, fmt)).toEqual([]);
    const tripped = b.notices([rule("old", "2026-11-16T11:00:00Z"), rule("hot", "2026-11-16T10:30:00Z")], now, fmt);
    expect(tripped.map((x) => [x.title, x.route])).toEqual([["Rule stopped: hot", { kind: "rule", name: "hot" }]]);
    expect(tripped[0]?.body).toContain("until 10:30");
    // Tripping again later (a new deadline) is new; an expired one is not announced.
    expect(b.notices([rule("hot", "2026-11-16T12:00:00Z")], now, fmt)).toHaveLength(1);
    expect(b.notices([rule("hot", "2026-11-16T09:00:00Z")], now, fmt)).toEqual([]);
  });
});

/** Timers driven by hand: `advance` runs what is due. */
function fakeTimers(): Timers & { advance(ms: number): void; armed(): number } {
  let now = 0;
  let next = 1;
  const queue = new Map<number, { at: number; fn: () => void }>();
  return {
    now: () => now,
    setTimeout(fn, ms) {
      const id = next++;
      queue.set(id, { at: now + ms, fn });
      return id;
    },
    clearTimeout(h) {
      queue.delete(h as number);
    },
    advance(ms) {
      const end = now + ms;
      for (;;) {
        const due = [...queue.entries()].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (due === undefined) break;
        queue.delete(due[0]);
        now = due[1].at;
        due[1].fn();
      }
      now = end;
    },
    armed: () => queue.size,
  };
}

const notice = (kind: Notice["kind"], title: string): Notice => ({ kind, title, body: "", route: { kind: "home" }, event: null });

describe("Throttle", () => {
  it("lets a burst through, then summarizes a storm", () => {
    const timers = fakeTimers();
    const shown: Notice[] = [];
    const t = new Throttle({ burst: 3, intervalMs: 20_000 }, timers, (n) => shown.push(n));
    for (let i = 0; i < 50; i++) t.offer(notice(i === 10 ? "approval" : "failed", `n${i}`));
    expect(shown.map((n) => n.title)).toEqual(["n0", "n1", "n2"]);
    expect(timers.armed()).toBe(1);
    timers.advance(20_000);
    expect(shown).toHaveLength(4);
    expect(shown[3]).toMatchObject({ kind: "summary", body: "47 more updates. Open Pulse to see them.", route: { kind: "approvals" } });
    // Idle afterwards: no timer.
    expect(timers.armed()).toBe(0);
    // Tokens come back with time.
    timers.advance(60_000);
    t.offer(notice("failed", "later"));
    expect(shown.at(-1)?.title).toBe("later");
  });

  it("shows nothing and holds no timer when turned off", () => {
    const timers = fakeTimers();
    const shown: Notice[] = [];
    const t = new Throttle({ burst: 1, intervalMs: 1000 }, timers, (n) => shown.push(n));
    t.offer(notice("failed", "a"));
    t.offer(notice("failed", "b"));
    expect(t.pending).toBe(true);
    t.setEnabled(false);
    expect(t.pending).toBe(false);
    t.offer(notice("failed", "c"));
    timers.advance(10_000);
    expect(shown.map((n) => n.title)).toEqual(["a"]);
  });
});
