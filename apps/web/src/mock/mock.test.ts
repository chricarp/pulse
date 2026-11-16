// The mock is the fixture the UI is built and reviewed against. These check that the scenarios
// it promises are really there, through the real client and JSON frames.

import { afterEach, describe, expect, it } from "vitest";

import type { EventsBatchWithOutcomes as EventsBatch } from "../contracts";
import { RpcClient } from "../rpc/client";
import { MockDaemon } from "./daemon";
import { mockSocketFactory } from "./transport";

let daemon: MockDaemon | null = null;
let client: RpcClient | null = null;

function connect() {
  daemon = new MockDaemon({ trickle: false });
  client = new RpcClient({ connect: mockSocketFactory(daemon, { latencyMs: [0, 2] }) });
  client.start();
  return { daemon, client };
}

afterEach(() => {
  client?.stop();
  daemon?.dispose();
});

describe("mock daemon", () => {
  it("seeds every state the UI must show", async () => {
    const { client } = connect();
    const pending = await client.call("runs.list", { filter: { status: ["pending_approval"] } });
    expect(pending.runs.length).toBeGreaterThanOrEqual(1);

    const page = await client.call("events.query", { filter: { types: ["fs.file.modified"], sources: ["site"] }, order: "newest_first", limit: 500 });
    const looped = page.events.find((e) => e.pulsecausationrun);
    expect(looped?.pulsecausation).toBe("inferred");
    const detail = await client.call("events.get", { id: looped?.id ?? "" });
    expect(detail.deliveries.map((d) => d.outcome)).toContainEqual({ kind: "blocked", reason: { reason: "loop_self_trigger", run: looped?.pulsecausationrun } });
    // The study agent's cards file, attributed from its Write call.
    const created = await client.call("events.query", { filter: { types: ["fs.file.created"], sources: [] }, order: "newest_first", limit: 500 });
    expect(created.events.some((e) => e.pulsecausation === "reported" && e.subject?.endsWith(".cards.md"))).toBe(true);

    const rules = await client.call("rules.list", { include_archived: true });
    const byName = new Map(rules.rules.map((r) => [r.config.name, r]));
    expect(byName.get("sync-notes")?.state.circuit_open_until).toBeTruthy();
    expect(byName.get("morning-brief")?.state.snoozed_until).toBeTruthy();
    expect(byName.get("old-experiment")?.config.archived).toBe(true);
    // Like serde, the mock leaves defaults off the wire.
    expect("where" in (byName.get("old-experiment")?.config ?? {})).toBe(false);

    const sources = await client.call("sources.list", {});
    expect(sources.sources.map((s) => s.health.state)).toEqual(expect.arrayContaining(["failed", "degraded", "healthy", "disabled"]));
    expect((await client.call("system.config_errors", {})).errors.length).toBeGreaterThan(0);
  });

  it("dry-runs an unsaved draft and renders its prompt with the untrusted-data fence", async () => {
    const { client } = connect();
    const page = await client.call("events.query", { filter: { types: ["fs.file.created"], sources: ["university"] }, order: "newest_first", limit: 1 });
    const event = page.events[0];
    if (!event) throw new Error("no event");
    const result = await client.call("rules.test", {
      rule: { kind: "draft", rule: { name: "draft", on: ["fs.*"], where: {}, debounce_by: "rule", supervised: false, enabled: true, archived: false, run: { agent: "study", prompt: "Look at {{subject}} {{nothing}}" } } },
      event: event.id,
      ignore_runtime_state: true,
    });
    expect(result.matched).toBe(true);
    const outcome = result.decision?.outcome;
    if (outcome?.kind !== "fire") throw new Error(`expected fire, got ${outcome?.kind}`);
    expect(outcome.prompt?.missing).toEqual(["data.nothing"]);
    expect(outcome.prompt?.text.split(outcome.prompt.delimiter)).toHaveLength(4);
  });

  it("batches a storm with drop counts instead of sending every event", async () => {
    const { daemon, client } = connect();
    const batches: EventsBatch[] = [];
    let total = 0;
    await new Promise<void>((resolve) => {
      client.subscribe("events.subscribe", () => ({ filter: { types: [], sources: ["camera"] } }), (b) => {
        batches.push(b);
        total += b.events.length + b.dropped;
        if (total >= 2000) resolve();
      }, { onSubscribed: () => daemon.storm(2000, 0.5) });
    });
    expect(total).toBe(2000);
    expect(batches.every((b) => b.events.length <= 200)).toBe(true);
    expect(batches.some((b) => b.dropped > 0)).toBe(true);
    // Oldest first within a batch.
    const ids = batches[0]?.events.map((e) => e.id) ?? [];
    expect([...ids].sort()).toEqual(ids);
  });
});
