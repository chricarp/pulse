import { describe, expect, it } from "vitest";

import type { EventDetail, EventOutcome } from "../contracts";
import { OutcomeStore } from "./outcomes";

const loop: EventOutcome = { event: "E1", rule: "fmt", outcome: { kind: "blocked", reason: { reason: "loop_self_trigger", run: "R0" } }, run: null };
const fired: EventOutcome = { event: "E1", rule: "other", outcome: { kind: "fired" }, run: "R1" };

describe("OutcomeStore", () => {
  it("merges batches and pages without duplicates and tells listeners", () => {
    const s = new OutcomeStore();
    let calls = 0;
    s.subscribe("E1", () => calls++);
    s.add([loop]);
    s.add([loop, fired]);
    expect(s.get("E1")).toEqual([loop, fired]);
    expect(calls).toBe(2);
    expect(s.get("E2")).toBeUndefined();
  });

  it("takes an event's full deliveries from the detail pane", () => {
    const s = new OutcomeStore();
    s.add([loop]);
    const detail = {
      event: { id: "E1" },
      deliveries: [{ id: "D1", event: "E1", rule: "other", agent: "a", at: "", outcome: { kind: "fired" }, run: "R1" }],
      runs: [],
    } as unknown as EventDetail;
    s.put(detail);
    expect(s.get("E1")).toEqual([fired]);
  });
});
