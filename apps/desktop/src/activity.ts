// What the tray shows, kept small: the daemon's status, a few recent runs, a few runs waiting for
// approval. Updated from pushes (`state.changed`, `runs.changed`); bounded however much happens.

import type { RunStatus, RunSummary, SystemStatus } from "./contracts.ts";

export const RECENT_MAX = 6;
export const APPROVALS_MAX = 5;

export interface Activity {
  status: SystemStatus | null;
  /** Newest first. */
  recent: RunSummary[];
  /** Oldest first: the one waiting longest is at the top. */
  approvals: RunSummary[];
}

export const EMPTY_ACTIVITY: Activity = { status: null, recent: [], approvals: [] };

const STATUSES: ReadonlySet<string> = new Set<RunStatus>([
  "pending_approval",
  "queued",
  "running",
  "succeeded",
  "failed",
  "timed_out",
  "cancelled",
  "rejected",
  "interrupted",
]);

/** A daemon from another release could send other shapes; drop what the tray cannot show. */
export function isRunSummary(x: unknown): x is RunSummary {
  if (typeof x !== "object" || x === null) return false;
  const r = x as Record<string, unknown>;
  return (
    typeof r.id === "string" &&
    typeof r.agent === "string" &&
    typeof r.created_at === "string" &&
    typeof r.status === "string" &&
    STATUSES.has(r.status)
  );
}

const newestFirst = (a: RunSummary, b: RunSummary) =>
  a.created_at < b.created_at ? 1 : a.created_at > b.created_at ? -1 : b.id.localeCompare(a.id);

function upsert(list: RunSummary[], run: RunSummary): RunSummary[] {
  const i = list.findIndex((r) => r.id === run.id);
  if (i === -1) return [...list, run];
  const copy = list.slice();
  copy[i] = run;
  return copy;
}

export function withStatus(a: Activity, status: SystemStatus): Activity {
  return { ...a, status };
}

/** Applies run states from `runs.list` or `runs.changed`. */
export function withRuns(a: Activity, runs: readonly unknown[]): Activity {
  let recent = a.recent;
  let approvals = a.approvals;
  for (const run of runs) {
    if (!isRunSummary(run)) continue;
    recent = upsert(recent, run);
    approvals =
      run.status === "pending_approval"
        ? upsert(approvals, run)
        : approvals.filter((r) => r.id !== run.id);
  }
  recent = recent.sort(newestFirst).slice(0, RECENT_MAX);
  // Keep the longest-waiting ones; the count in `status` says how many there are in all.
  approvals = approvals.sort(newestFirst).reverse().slice(0, APPROVALS_MAX);
  return { ...a, recent, approvals };
}

export function approvalsWaiting(a: Activity): number {
  return Math.max(a.status?.runs.pending_approval ?? 0, a.approvals.length);
}
