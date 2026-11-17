import { describe, expect, it } from "vitest";

import type { EventDetail, EventOutcome, RunStatus, RunSummary } from "../contracts";
import { OutcomeStore } from "./outcomes";

const loop: EventOutcome = { delivery: "D1", event: "E1", rule: "fmt", outcome: { kind: "blocked", reason: { reason: "loop_self_trigger", run: "R0" } }, run: null, run_status: null };
const fired: EventOutcome = { delivery: "D2", event: "E1", rule: "other", outcome: { kind: "fired" }, run: "R1", run_status: "queued" };
const supervised: EventOutcome = { delivery: "D3", event: "E2", rule: "careful", outcome: { kind: "awaiting_approval" }, run: "R2", run_status: "pending_approval" };

const run = (id: string, event: string, status: RunStatus): RunSummary =>
  ({ id, event, status, rule: "careful", agent: "a", created_at: "", chain_depth: 1 }) as RunSummary;

describe("OutcomeStore", () => {
  it("merges batches and pages without duplicates, in the order deliveries were recorded, and tells listeners", () => {
    const s = new OutcomeStore();
    let calls = 0;
    s.subscribe("E1", () => calls++);
    s.add([fired]);
    // A page read before the batch arrived carries an earlier delivery too.
    s.add([loop, fired]);
    s.add([loop]);
    expect(s.get("E1")).toEqual([loop, fired]);
    expect(calls).toBe(2);
    expect(s.get("E2")).toBeUndefined();
  });

  it("merges an event's full deliveries from the detail pane, keeping outcomes that arrived after it was read", () => {
    const s = new OutcomeStore();
    s.add([loop]);
    const detail = {
      event: { id: "E1" },
      deliveries: [{ id: "D2", event: "E1", rule: "other", agent: "a", at: "", outcome: { kind: "fired" }, run: "R1" }],
      runs: [run("R1", "E1", "running")],
    } as unknown as EventDetail;
    s.put(detail);
    expect(s.get("E1")).toEqual([loop, { ...fired, run_status: "running" }]);
  });

  it("follows the run a delivery created through its life, never backwards", () => {
    const s = new OutcomeStore();
    const seen: (RunStatus | null | undefined)[] = [];
    s.subscribe("E2", () => seen.push(s.get("E2")?.[0]?.run_status));
    s.add([supervised]);
    s.applyRuns([run("R2", "E2", "queued")]);
    s.applyRuns([run("R2", "E2", "running"), run("R9", "E9", "running")]);
    s.applyRuns([run("R2", "E2", "succeeded")]);
    // A page read before the run ended, answered after.
    s.add([{ ...supervised, run_status: "running" }]);
    s.put({
      event: { id: "E2" },
      deliveries: [{ id: "D3", event: "E2", rule: "careful", agent: "a", at: "", outcome: { kind: "awaiting_approval" }, run: "R2" }],
      runs: [run("R2", "E2", "pending_approval")],
    } as unknown as EventDetail);
    expect(seen).toEqual(["pending_approval", "queued", "running", "succeeded", "succeeded"]);
    expect(s.get("E2")?.[0]?.run_status).toBe("succeeded");
  });

  it("takes a run's later state from the run index when the change arrived before the outcome", () => {
    const index = new Map<string, RunStatus>([["R2", "running"]]);
    const s = new OutcomeStore((id) => index.get(id));
    s.add([supervised]);
    expect(s.get("E2")?.[0]?.run_status).toBe("running");
  });
});
