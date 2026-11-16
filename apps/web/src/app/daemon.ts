// What the app knows about the daemon: a cache of daemon state, kept fresh by notifications.
// Nothing here is the app's own opinion; every field is something the daemon said.
//
// - Lists (rules, agents, sources, projects, config errors) are fetched on connect and refetched
//   when `state.changed` names their topic.
// - Runs: one `runs.subscribe` for all runs keeps an index of active runs and the newest
//   finished ones. Pending approvals come from it, so they are visible from every view.
// - A refetch never undoes a newer push or a newer refetch: run changes that arrive while the
//   index loads are applied on top of what it loaded, a run's state only moves forward
//   (`newerRun`), and a list response older than the latest request for that list is dropped.

import type {
  AgentInfo,
  RunFilter,
  ConfigError,
  EventTypeInfo,
  Project,
  RuleInfo,
  RunSummary,
  SourceInfo,
  StateTopic,
  SystemStatus,
} from "../contracts";
import { RpcCallError, type ConnectionState, type RpcClient } from "../rpc/client";
import { createStore, type Store } from "../store/store";

export interface DaemonState {
  connection: ConnectionState;
  /** Consecutive failed attempts, for the "is the daemon running?" message. */
  failures: number;
  status: SystemStatus | null;
  rules: RuleInfo[] | null;
  agents: AgentInfo[] | null;
  sources: SourceInfo[] | null;
  projects: Project[] | null;
  configErrors: ConfigError[] | null;
  /** Definitions in effect that cannot work as written (a rule that can never fire). */
  configWarnings: ConfigError[] | null;
  eventTypes: EventTypeInfo[] | null;
  /** Run index: every active run plus the newest finished ones. */
  runs: ReadonlyMap<string, RunSummary>;
  /** Bumped when a run changes; lets views refetch run details. */
  runsVersion: number;
  loadError: string | null;
}

const RUN_INDEX_FINISHED = 300;

export const ACTIVE: ReadonlySet<RunSummary["status"]> = new Set(["pending_approval", "queued", "running"]);

export function isActive(status: RunSummary["status"]): boolean {
  return ACTIVE.has(status);
}

type ListKey = "rules" | "agents" | "sources" | "projects" | "errors";

export class Daemon {
  readonly store: Store<DaemonState>;
  private readonly runListeners = new Set<(runs: RunSummary[]) => void>();
  /** Run changes received while the index loads, applied on top of what it loaded. */
  private duringLoad: RunSummary[] | null = null;
  /** Per list, the number of the latest request; older responses are dropped. */
  private readonly requested: Record<ListKey, number> = { rules: 0, agents: 0, sources: 0, projects: 0, errors: 0 };

  constructor(readonly client: RpcClient) {
    this.store = createStore<DaemonState>({
      connection: client.state,
      failures: 0,
      status: null,
      rules: null,
      agents: null,
      sources: null,
      projects: null,
      configErrors: null,
      configWarnings: null,
      eventTypes: null,
      runs: new Map(),
      runsVersion: 0,
      loadError: null,
    });

    client.onStateChange((connection) => {
      this.store.set((s) => ({
        ...s,
        connection,
        failures: connection.kind === "open" ? 0 : connection.kind === "waiting" ? connection.attempt : s.failures,
      }));
    });
    client.onOpen(() => {
      void this.refreshAll();
    });
    client.onDaemonStateChanged((change) => {
      this.store.set((s) => ({ ...s, status: change.status }));
      void this.refetch(change.topics);
    });
    client.subscribe(
      "runs.subscribe",
      () => ({ filter: { status: [] } }),
      // After a reconnect `onOpen` reloads the index; the subscription is issued first, so
      // nothing changes unseen in between.
      (n) => this.applyRuns(n.runs),
    );
  }

  onRunsChanged(listener: (runs: RunSummary[]) => void): () => void {
    this.runListeners.add(listener);
    return () => this.runListeners.delete(listener);
  }

  private applyRuns(runs: RunSummary[]): void {
    if (runs.length === 0) return;
    this.duringLoad?.push(...runs);
    this.store.set((s) => ({ ...s, runs: indexRuns(s.runs, runs), runsVersion: s.runsVersion + 1 }));
    for (const l of this.runListeners) l(runs);
  }

  /** Calls `fetch` for list `key`; `apply` runs only if no later request for it was made. */
  private latest<T>(key: ListKey, fetch: () => Promise<T>): Promise<{ ok: true; value: T } | { ok: false }> {
    const n = ++this.requested[key];
    return fetch().then((value) => (n === this.requested[key] ? { ok: true as const, value } : { ok: false as const }));
  }

  async refreshAll(): Promise<void> {
    const c = this.client;
    const n = { ...this.requested };
    for (const k of Object.keys(n) as ListKey[]) n[k] = ++this.requested[k];
    const current = (k: ListKey) => n[k] === this.requested[k];
    try {
      const [status, rules, agents, sources, projects, errors] = await Promise.all([
        c.call("system.status", {}),
        c.call("rules.list", { include_archived: true }),
        c.call("agents.list", {}),
        c.call("sources.list", {}),
        c.call("projects.list", {}),
        c.call("system.config_errors", {}),
      ]);
      // A list refetched by a `state.changed` while this was in flight is newer: keep it.
      this.store.set((s) => ({
        ...s,
        status,
        rules: current("rules") ? rules.rules : s.rules,
        agents: current("agents") ? agents.agents : s.agents,
        sources: current("sources") ? sources.sources : s.sources,
        projects: current("projects") ? projects.projects : s.projects,
        configErrors: current("errors") ? errors.errors : s.configErrors,
        configWarnings: current("errors") ? errors.warnings : s.configWarnings,
        loadError: null,
      }));
      if (!this.store.get().eventTypes) {
        const types = await c.call("system.event_types", {});
        this.store.set((s) => ({ ...s, eventTypes: types.types }));
      }
      await this.loadRuns();
    } catch (err) {
      if (err instanceof RpcCallError && err.kind === "disconnected") return;
      this.store.set((s) => ({ ...s, loadError: err instanceof Error ? err.message : String(err) }));
    }
  }

  /**
   * Active runs (all of them) and the newest finished ones. The two lists are read at different
   * moments, and `runs.changed` keeps arriving meanwhile ("approved, now running"): those
   * changes are applied on top of what was loaded, and a state never moves backwards.
   */
  async loadRuns(): Promise<void> {
    const c = this.client;
    const during: RunSummary[] = [];
    this.duringLoad = during;
    try {
      const active: RunSummary[] = [];
      let cursor: string | undefined;
      // Active runs are few (bounded by agents' max_concurrent + max_queued, plus approvals).
      for (let page = 0; page < 10; page++) {
        const res = await c.call("runs.list", { filter: { status: ["pending_approval", "queued", "running"] }, limit: 500, ...(cursor ? { cursor } : {}) });
        active.push(...res.runs);
        if (!res.next_cursor) break;
        cursor = res.next_cursor;
      }
      const recent = await c.call("runs.list", { filter: { status: [] }, limit: 200 });
      this.store.set((s) => ({ ...s, runs: indexRuns(indexRuns(new Map(), [...recent.runs, ...active]), during), runsVersion: s.runsVersion + 1 }));
    } finally {
      if (this.duringLoad === during) this.duringLoad = null;
    }
  }

  private async refetch(topics: readonly StateTopic[]): Promise<void> {
    const c = this.client;
    const t = new Set(topics);
    const tasks: Promise<void>[] = [];
    // Two refetches of one list can answer out of order; only the latest request's answer counts.
    const load = <T>(key: ListKey, fetch: () => Promise<T>, apply: (s: DaemonState, v: T) => DaemonState) =>
      tasks.push(this.latest(key, fetch).then((r) => (r.ok ? this.store.set((s) => apply(s, r.value)) : undefined)));
    if (t.has("rules") || t.has("config")) load("rules", () => c.call("rules.list", { include_archived: true }), (s, r) => ({ ...s, rules: r.rules }));
    if (t.has("agents") || t.has("config")) load("agents", () => c.call("agents.list", {}), (s, r) => ({ ...s, agents: r.agents }));
    if (t.has("sources") || t.has("config")) load("sources", () => c.call("sources.list", {}), (s, r) => ({ ...s, sources: r.sources }));
    if (t.has("projects") || t.has("config")) load("projects", () => c.call("projects.list", {}), (s, r) => ({ ...s, projects: r.projects }));
    if (t.has("config")) load("errors", () => c.call("system.config_errors", {}), (s, r) => ({ ...s, configErrors: r.errors, configWarnings: r.warnings }));
    await Promise.allSettled(tasks);
  }
}

const STAGE: Record<RunSummary["status"], number> = {
  pending_approval: 0,
  queued: 1,
  running: 2,
  succeeded: 3,
  failed: 3,
  timed_out: 3,
  cancelled: 3,
  rejected: 3,
  interrupted: 3,
};

/**
 * Of two summaries of one run, the later state. A run only moves forward (pending → queued →
 * running → an end), so whichever has the later stage is newer, whatever order they arrived in;
 * at the same stage, the one with more recorded (a start or end time) is.
 */
export function newerRun(a: RunSummary | undefined, b: RunSummary): RunSummary {
  if (!a) return b;
  const sa = STAGE[a.status];
  const sb = STAGE[b.status];
  if (sa !== sb) return sa > sb ? a : b;
  const filled = (r: RunSummary) => (r.started_at ? 1 : 0) + (r.ended_at ? 1 : 0);
  return filled(a) > filled(b) ? a : b;
}

/** Merges run summaries into the index, keeping every active run and the newest finished ones. */
export function indexRuns(prev: ReadonlyMap<string, RunSummary>, updates: readonly RunSummary[]): Map<string, RunSummary> {
  const next = new Map(prev);
  for (const r of updates) next.set(r.id, newerRun(next.get(r.id), r));
  let finished = 0;
  for (const r of next.values()) if (!isActive(r.status)) finished++;
  if (finished > RUN_INDEX_FINISHED) {
    const old = [...next.values()].filter((r) => !isActive(r.status)).sort((a, b) => (a.id < b.id ? -1 : 1));
    for (const r of old.slice(0, finished - RUN_INDEX_FINISHED)) next.delete(r.id);
  }
  return next;
}

export function pendingApprovals(runs: ReadonlyMap<string, RunSummary>): RunSummary[] {
  return [...runs.values()].filter((r) => r.status === "pending_approval").sort((a, b) => (a.id < b.id ? -1 : 1));
}

export function runMatches(r: RunSummary, f: RunFilter): boolean {
  if (f.rule && r.rule !== f.rule) return false;
  if (f.agent && r.agent !== f.agent) return false;
  if (f.status.length && !f.status.includes(r.status)) return false;
  return true;
}

/**
 * A list of runs with changes applied: known runs take the later state (runs that leave the
 * filter stay, with their new state, until the list reloads); new matching runs go on top.
 */
export function applyRunChanges(list: readonly RunSummary[], changes: readonly RunSummary[], filter: RunFilter): RunSummary[] {
  if (changes.length === 0) return [...list];
  const byId = new Map(list.map((r, i) => [r.id, i]));
  const next = [...list];
  const added = new Map<string, RunSummary>();
  for (const r of changes) {
    const i = byId.get(r.id);
    if (i !== undefined) next[i] = newerRun(next[i], r);
    else if (runMatches(r, filter)) added.set(r.id, newerRun(added.get(r.id), r));
  }
  return added.size ? [...[...added.values()].sort((a, b) => (a.id < b.id ? 1 : -1)), ...next] : next;
}

