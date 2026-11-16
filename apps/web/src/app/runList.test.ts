import { describe, expect, it } from "vitest";

import type { RunSummary } from "../contracts";
import { applyRunChanges } from "./daemon";

const run = (id: string, status: RunSummary["status"], rule = "r"): RunSummary => ({
  id,
  rule,
  agent: "a",
  event: "e",
  status,
  created_at: "2026-11-16T09:00:00Z",
  started_at: null,
  ended_at: null,
  exit_code: null,
  chain_depth: 1,
});

describe("a run list", () => {
  it("applies changes pushed while its page loaded on top of the page, never backwards", () => {
    // The page was read before run 1 started and before run 3 existed.
    const page = [run("2", "succeeded"), run("1", "pending_approval")];
    const pushed = [run("1", "running"), run("3", "queued"), run("9", "queued", "other")];
    expect(applyRunChanges(page, pushed, { rule: "r", status: [] }).map((r) => `${r.id} ${r.status}`)).toEqual(["3 queued", "2 succeeded", "1 running"]);
    // A summary older than the list's is ignored.
    expect(applyRunChanges([run("1", "running")], [run("1", "queued")], { status: [] })[0]?.status).toBe("running");
  });
});
