import { describe, expect, it } from "vitest";

import { ERROR_STALE } from "../contracts";
import { RpcCallError } from "../rpc/client";
import { expectedFor, fieldChanges, refusedCurrent, staleOf } from "./edits";

describe("edits", () => {
  it("names the revision an editor was opened on, or that it creates", () => {
    expect(expectedFor({ revision: "abc" })).toEqual({ kind: "revision", revision: "abc" });
    expect(expectedFor(null)).toEqual({ kind: "absent" });
  });

  it("recognizes a stale error and its current revision", () => {
    expect(staleOf(new RpcCallError("rpc", "rules.upsert", "changed", ERROR_STALE, { current: "def" }))).toEqual({ current: "def" });
    expect(staleOf(new RpcCallError("rpc", "rules.upsert", "removed", ERROR_STALE, { current: null }))).toEqual({ current: null });
    expect(staleOf(new RpcCallError("rpc", "rules.upsert", "bad", -32004, {}))).toBeNull();
    expect(staleOf(new Error("x"))).toBeNull();
  });

  it("overwrites against the revision the refusal named, not a list that may lag", () => {
    const listed = { revision: "old", config: { name: "r" } };
    expect(refusedCurrent({ current: "file" }, listed)).toEqual({ revision: "file", config: { name: "r" } });
    expect(refusedCurrent({ current: null }, listed)).toBeNull();
    expect(refusedCurrent({ current: "file" }, null)).toBeNull();
  });

  it("lists the fields that changed, whatever the key order, defaults left out counting as unset", () => {
    const before = { name: "r", on: ["fs.*"], enabled: true, run: { agent: "a", prompt: "x" } };
    const after = { run: { prompt: "x", agent: "a" }, name: "r", on: ["fs.*"], enabled: false, supervised: true };
    expect(fieldChanges(before, after)).toEqual([
      { field: "enabled", before: true, after: false },
      { field: "supervised", before: undefined, after: true },
    ]);
    expect(fieldChanges(before, { ...before })).toEqual([]);
  });
});
