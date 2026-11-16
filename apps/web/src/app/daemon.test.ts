import { describe, expect, it } from "vitest";

import type { RunSummary } from "../contracts";
import type { RpcClient } from "../rpc/client";
import { Daemon, indexRuns, newerRun } from "./daemon";

const run = (id: string, status: RunSummary["status"], more: Partial<RunSummary> = {}): RunSummary => ({
  id,
  rule: "r",
  agent: "a",
  event: "e",
  status,
  created_at: "2026-11-16T09:00:00Z",
  started_at: null,
  ended_at: null,
  exit_code: null,
  chain_depth: 1,
  ...more,
});

/** A client whose calls the test answers by hand, in any order, and whose pushes it sends. */
function fakeClient() {
  const calls: { method: string; params: unknown; resolve: (v: unknown) => void }[] = [];
  let push: ((n: { runs: RunSummary[] }) => void) | null = null;
  const client = {
    state: { kind: "open" },
    onStateChange: () => () => {},
    onOpen: () => () => {},
    onDaemonStateChanged: () => () => {},
    subscribe: (_m: string, _p: unknown, handler: (n: { runs: RunSummary[] }) => void) => {
      push = handler;
      return () => {};
    },
    call: (method: string, params: unknown) => new Promise((resolve) => calls.push({ method, params, resolve })),
  };
  const flush = () => new Promise((r) => setTimeout(r, 0));
  return {
    client: client as unknown as RpcClient,
    calls,
    push: (runs: RunSummary[]) => push?.({ runs }),
    /** Answers the oldest unanswered call to `method`. */
    async answer(method: string, result: unknown) {
      const i = calls.findIndex((c) => c.method === method);
      expect(i, `a pending ${method} call`).toBeGreaterThanOrEqual(0);
      const [c] = calls.splice(i, 1);
      c?.resolve(result);
      await flush();
    },
  };
}

describe("the run index", () => {
  it("keeps a change pushed while it loads (approved, now running)", async () => {
    const f = fakeClient();
    const daemon = new Daemon(f.client);
    const loading = daemon.loadRuns();
    // The active runs are read while the run still waits for approval…
    await f.answer("runs.list", { runs: [run("1", "pending_approval")], next_cursor: null });
    // …then it is approved and starts, before the second list call is answered, which read
    // the log before that happened.
    f.push([run("1", "running", { started_at: "2026-11-16T09:00:05Z" })]);
    await f.answer("runs.list", { runs: [run("1", "queued")], next_cursor: null });
    await loading;
    expect(daemon.store.get().runs.get("1")?.status).toBe("running");
  });

  it("never moves a run backwards, whatever order summaries arrive in", () => {
    const order: RunSummary["status"][] = ["pending_approval", "queued", "running", "succeeded"];
    for (let i = 0; i < order.length; i++) {
      for (let j = 0; j < order.length; j++) {
        const a = run("1", order[i] ?? "queued");
        const b = run("1", order[j] ?? "queued");
        expect(indexRuns(indexRuns(new Map(), [a]), [b]).get("1")?.status).toBe(order[Math.max(i, j)]);
      }
    }
    const started = run("1", "running", { started_at: "t" });
    expect(newerRun(started, run("1", "running"))).toBe(started);
  });
});

describe("lists", () => {
  it("drop an answer older than the latest request for that list", async () => {
    const f = fakeClient();
    const daemon = new Daemon(f.client);
    const onTopic = (daemon as unknown as { refetch(t: string[]): Promise<void> }).refetch.bind(daemon);
    const first = onTopic(["rules"]);
    const second = onTopic(["rules"]);
    const rule = (enabled: boolean) => ({ config: { name: "r", enabled }, origin: { kind: "user", path: "/p" }, revision: String(enabled), state: {} });
    // The second request (after a disable) is answered first; the first one, read before it,
    // answers late and must not bring the old state back.
    const [older, newer] = f.calls.filter((c) => c.method === "rules.list");
    newer?.resolve({ rules: [rule(false)] });
    await second;
    older?.resolve({ rules: [rule(true)] });
    await first;
    expect(daemon.store.get().rules?.[0]?.config.enabled).toBe(false);
  });
});
