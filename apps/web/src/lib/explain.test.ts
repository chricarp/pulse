import { describe, expect, it } from "vitest";

import type { DeliveryOutcome, EventOutcome, RunStatus } from "../contracts";
import { causationWhy, currentOutcomes, pickMarkers } from "./explain";

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

describe("pickMarkers", () => {
  const loop = { reason: "loop_self_trigger", run: "R0" } as const;
  it.each<[string, EventOutcome[], string[]]>([
    [
      "a delivery waiting for room is not hidden behind a fire",
      [o("a", { kind: "fired" }, "succeeded"), o("b", { kind: "blocked", reason: loop }), o("c", { kind: "waiting", reason: busy })],
      ["b", "c"],
    ],
    [
      "an approval and a failure come before what went well",
      [o("a", { kind: "fired" }, "succeeded"), o("b", { kind: "fired" }, "failed"), o("c", { kind: "awaiting_approval" }, "pending_approval")],
      ["c", "b"],
    ],
    [
      "loop protection before waiting, waiting before a failure",
      [o("a", { kind: "fired" }, "failed"), o("b", { kind: "waiting", reason: busy }), o("c", { kind: "blocked", reason: loop })],
      ["c", "b"],
    ],
    [
      "an approved run no longer needs anyone",
      [o("a", { kind: "awaiting_approval" }, "succeeded"), o("b", { kind: "fired" }, "running"), o("c", { kind: "blocked", reason: { reason: "cooldown", until: "2026-11-17T10:00:00Z" } })],
      ["b", "a"],
    ],
  ])("%s", (_, list, want) => {
    const { shown, rest } = pickMarkers(currentOutcomes(list), 2);
    expect(shown.map((c) => c.rule)).toEqual(want);
    expect(rest).toHaveLength(list.length - 2);
  });
});

describe("causationWhy", () => {
  it("says which guess an inferred cause is", () => {
    // A command or Codex agent: Pulse never sees its writes; no shell command is involved.
    expect(causationWhy("inferred", "unreported")).toMatch(/never reports what it writes/);
    expect(causationWhy("inferred", "unreported")).not.toMatch(/shell command/);
    // Claude Code ran something opaque.
    expect(causationWhy("inferred", "unseen")).toMatch(/a shell command, say/);
    // Reloaded after a restart: its output is no longer read.
    expect(causationWhy("inferred", "reloaded")).toMatch(/daemon restarted.*no longer reads its output/);
    // Recorded before the reason was kept: no claim about which.
    expect(causationWhy("inferred", null)).not.toMatch(/shell command|never reports|restarted/);
    expect(causationWhy("reported", null)).toMatch(/tool call named this file/);
  });
});
