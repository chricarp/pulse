// Which changes are worth interrupting someone for, and how often. Pure apart from the injected
// timers, so a storm can be tested without waiting for it.
//
// Interrupting: a run waiting for approval, a run that failed or timed out, a rule held by its
// circuit breaker, the daemon going away. Not interrupting: runs that start or succeed, runs
// cancelled or rejected by the user, runs interrupted by a shutdown, anything that was already
// so when the shell connected.

import { isRunSummary } from "./activity.ts";
import type { RunSummary } from "./contracts.ts";
import { plainText } from "./text.ts";
import type { Route } from "./urls.ts";

export type NoticeKind = "approval" | "failed" | "breaker" | "daemon" | "summary";

export interface Notice {
  kind: NoticeKind;
  title: string;
  body: string;
  route: Route;
  /** The event behind it: its subject is added to the body when the notice is shown. */
  event: string | null;
}

/** Remembers which (run, state) pairs were seen, so each is announced at most once. */
export class RunNotices {
  private readonly seen = new Set<string>();
  constructor(private readonly capacity = 512) {}

  private mark(run: RunSummary): boolean {
    const key = `${run.id}:${run.status}`;
    if (this.seen.has(key)) return false;
    this.seen.add(key);
    if (this.seen.size > this.capacity) {
      // Sets iterate in insertion order: drop the oldest.
      const oldest = this.seen.values().next().value;
      if (oldest !== undefined) this.seen.delete(oldest);
    }
    return true;
  }

  /** Runs as they were when the shell connected: known, not announced. */
  seed(runs: readonly unknown[]): void {
    for (const run of runs) if (isRunSummary(run)) this.mark(run);
  }

  notices(runs: readonly unknown[]): Notice[] {
    const out: Notice[] = [];
    for (const run of runs) {
      if (!isRunSummary(run) || !this.mark(run)) continue;
      const rule = plainText(run.rule ?? "a rerun", 60);
      const agent = plainText(run.agent, 40);
      const route: Route = { kind: "run", id: run.id };
      switch (run.status) {
        case "pending_approval":
          out.push({ kind: "approval", title: `Approval needed: ${rule}`, body: `${agent} is waiting for you to approve the run.`, route, event: run.event });
          break;
        case "failed":
          out.push({
            kind: "failed",
            title: `Run failed: ${rule}`,
            body: run.exit_code === null || run.exit_code === undefined ? `${agent} failed.` : `${agent} exited with code ${run.exit_code}.`,
            route,
            event: run.event,
          });
          break;
        case "timed_out":
          out.push({ kind: "failed", title: `Run timed out: ${rule}`, body: `${agent} ran out of time.`, route, event: run.event });
          break;
        default:
          break;
      }
    }
    return out;
  }
}

interface RuleLike {
  config: { name: string };
  state: { circuit_open_until?: string | null };
}

function isRuleLike(x: unknown): x is RuleLike {
  if (typeof x !== "object" || x === null) return false;
  const r = x as Record<string, unknown>;
  const config = r.config as Record<string, unknown> | undefined;
  const state = r.state as Record<string, unknown> | undefined;
  return typeof config?.name === "string" && typeof state === "object" && state !== null;
}

/** Rules newly held by their circuit breaker, from successive `rules.list` results. */
export class BreakerNotices {
  private open = new Map<string, string>();

  private static openUntil(rules: readonly unknown[], nowMs: number): Map<string, string> {
    const m = new Map<string, string>();
    for (const r of rules) {
      if (!isRuleLike(r)) continue;
      const until = r.state.circuit_open_until;
      if (typeof until === "string" && Date.parse(until) > nowMs) m.set(r.config.name, until);
    }
    return m;
  }

  seed(rules: readonly unknown[], nowMs: number): void {
    this.open = BreakerNotices.openUntil(rules, nowMs);
  }

  notices(rules: readonly unknown[], nowMs: number, formatTime: (iso: string) => string): Notice[] {
    const next = BreakerNotices.openUntil(rules, nowMs);
    const out: Notice[] = [];
    for (const [name, until] of next) {
      if (this.open.get(name) === until) continue;
      out.push({
        kind: "breaker",
        title: `Rule stopped: ${plainText(name, 60)}`,
        body: `It fired too often, so its circuit breaker holds it until ${formatTime(until)}. Unsnooze it in Pulse to resume sooner.`,
        route: { kind: "rule", name },
        event: null,
      });
    }
    this.open = next;
    return out;
  }
}

export interface Timers {
  now(): number;
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface ThrottleOptions {
  /** Notices that may be shown back to back. */
  burst: number;
  /** One more may be shown per interval after that. */
  intervalMs: number;
}

export const DEFAULT_THROTTLE: ThrottleOptions = { burst: 3, intervalMs: 20_000 };

/**
 * A token bucket in front of the notification center. What does not fit is counted and later
 * announced as one summary ("12 more updates"). A timer is armed only while something is held
 * back; otherwise this does nothing at all.
 */
export class Throttle {
  private tokens: number;
  private lastRefill: number;
  private held = 0;
  private heldApproval = false;
  private timer: unknown = null;
  private enabled = true;

  constructor(
    private readonly opts: ThrottleOptions,
    private readonly timers: Timers,
    private readonly show: (n: Notice) => void,
  ) {
    this.tokens = opts.burst;
    this.lastRefill = timers.now();
  }

  private refill(): void {
    const now = this.timers.now();
    const gained = (now - this.lastRefill) / this.opts.intervalMs;
    this.tokens = Math.min(this.opts.burst, this.tokens + gained);
    this.lastRefill = now;
  }

  offer(n: Notice): void {
    if (!this.enabled) return;
    this.refill();
    if (this.tokens >= 1) {
      this.tokens -= 1;
      this.show(n);
      return;
    }
    this.held += 1;
    this.heldApproval ||= n.kind === "approval";
    this.arm();
  }

  private arm(): void {
    if (this.timer !== null) return;
    const wait = Math.max(0, Math.ceil((1 - this.tokens) * this.opts.intervalMs));
    this.timer = this.timers.setTimeout(() => {
      this.timer = null;
      this.refill();
      if (this.held === 0) return;
      if (this.tokens < 1) {
        this.arm();
        return;
      }
      this.tokens -= 1;
      const count = this.held;
      const approvals = this.heldApproval;
      this.held = 0;
      this.heldApproval = false;
      this.show({
        kind: "summary",
        title: "Pulse",
        body: `${count} more update${count === 1 ? "" : "s"}. Open Pulse to see them.`,
        route: approvals ? { kind: "approvals" } : { kind: "home" },
        event: null,
      });
    }, wait);
  }

  setEnabled(enabled: boolean): void {
    this.enabled = enabled;
    if (!enabled) {
      this.held = 0;
      this.heldApproval = false;
      if (this.timer !== null) this.timers.clearTimeout(this.timer);
      this.timer = null;
    }
  }

  /** Whether a timer is armed (for tests: idle means none). */
  get pending(): boolean {
    return this.timer !== null;
  }
}
