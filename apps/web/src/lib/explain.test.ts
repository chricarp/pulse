import { describe, expect, it } from "vitest";

import type { DeliveryOutcome, EventOutcome, RunStatus } from "../contracts";
import { currentOutcomes } from "./explain";

let n = 0;
const o = (rule: string, outcome: DeliveryOutcome, run_status: RunStatus | null = null): EventOutcome => ({
  delivery: `D${String(++n).padStart(3, "0")}`,
  event: "E1",
  rule,
  outcome,
  run: run_status ? `R${n}` : null,
  run_status,
});
const busy = { reason: "agent_busy", backlog: 2, max: 2 } as const;

describe("currentOutcomes", () => {
  it.each<[string, EventOutcome[], [string, string][]]>([
    ["an approved run that finished", [o("careful", { kind: "awaiting_approval" }, "succeeded")], [["careful", "succeeded"]]],
    ["a run still awaiting approval", [o("careful", { kind: "awaiting_approval" }, "pending_approval")], [["careful", "awaiting approval"]]],
    ["a rejected run", [o("careful", { kind: "awaiting_approval" }, "rejected")], [["careful", "rejected"]]],
    ["a fired run that is running", [o("fmt", { kind: "fired" }, "running")], [["fmt", "running"]]],
    ["a fired run that failed", [o("fmt", { kind: "fired" }, "failed")], [["fmt", "failed"]]],
    ["a run no longer in the log", [{ ...o("fmt", { kind: "fired" }, "queued"), run_status: null }], [["fmt", "fired"]]],
    ["a debounce that later fired", [o("notes", { kind: "debounced", until: "2026-11-17T10:00:00Z" }), o("notes", { kind: "fired" }, "queued")], [["notes", "queued"]]],
    ["a debounce still held", [o("notes", { kind: "debounced", until: "2026-11-17T10:00:00Z" })], [["notes", "debounced"]]],
    ["a waiting delivery that later ran", [o("notes", { kind: "waiting", reason: busy }), o("notes", { kind: "fired" }, "succeeded")], [["notes", "succeeded"]]],
    ["a waiting delivery", [o("notes", { kind: "waiting", reason: busy })], [["notes", "waiting: agent busy"]]],
  ])("%s", (_, list, want) => {
    expect(currentOutcomes(list).map((c) => [c.rule, c.label])).toEqual(want);
    // Whatever order they arrived in.
    expect(currentOutcomes([...list].reverse()).map((c) => [c.rule, c.label])).toEqual(want);
  });
});
