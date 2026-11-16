// Editing definitions that someone else may change at the same time (the config file by hand,
// the CLI, another window). Every definition the daemon hands out carries a `revision`; a save
// names the revision it was based on, and the daemon refuses with a `stale` error when the
// definition changed since. Pure helpers for that; the UI is `views/common/Conflict.tsx`.

import { ERROR_STALE, type Expected, type Stale } from "../contracts";
import { RpcCallError } from "../rpc/client";

/** What a save of an editor opened on `base` (null: a new definition) expects. */
export function expectedFor(base: { revision: string } | null): Expected {
  return base ? { kind: "revision", revision: base.revision } : { kind: "absent" };
}

/** The `stale` error's data, if `err` is one. */
export function staleOf(err: unknown): Stale | null {
  if (!(err instanceof RpcCallError) || err.code !== ERROR_STALE) return null;
  const current = (err.data as { current?: string | null } | undefined)?.current;
  return { current: current ?? null };
}

/**
 * The definition to show after a save was refused: what the daemon reports now, carrying the
 * revision the refusal named (the file's, which a list that has not reloaded yet may lag), so
 * "save mine anyway" names exactly what is in the file. `null`: it does not exist.
 */
export function refusedCurrent<T extends { revision: string }>(stale: Stale, now: T | null): T | null {
  if (stale.current === null || now === null) return stale.current === null ? null : now;
  return { ...now, revision: stale.current };
}

export interface FieldChange {
  field: string;
  before: unknown;
  after: unknown;
}

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.entries(value as Record<string, unknown>)
      .filter(([, v]) => v !== undefined && v !== null)
      .sort(([a], [b]) => (a < b ? -1 : 1))
      .map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value ?? null);
}

/**
 * The top-level fields that differ between two versions of a definition, in a stable order.
 * A field missing on one side (a default the wire leaves out) counts as changed only if the
 * other side has a value.
 */
export function fieldChanges(before: object, after: object): FieldChange[] {
  const b = before as Record<string, unknown>;
  const a = after as Record<string, unknown>;
  const keys = [...new Set([...Object.keys(b), ...Object.keys(a)])].sort();
  return keys.filter((k) => canonical(b[k]) !== canonical(a[k])).map((k) => ({ field: k, before: b[k], after: a[k] }));
}

/** A value from a definition, for showing in a change list. */
export function showValue(v: unknown): string {
  if (v === undefined || v === null) return "(not set)";
  if (typeof v === "string") return v;
  return JSON.stringify(v);
}
