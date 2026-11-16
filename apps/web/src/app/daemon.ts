// What the app knows about the daemon: a cache of daemon state, kept fresh by notifications.
// Nothing here is the app's own opinion; every field is something the daemon said.
//
// - Lists (rules, agents, sources, projects, config errors) are fetched on connect and refetched
//   when `state.changed` names their topic.
// - Runs: one `runs.subscribe` for all runs keeps an index of active runs and the newest
//   finished ones. Pending approvals come from it, so they are visible from every view.

import type {
  AgentInfo,
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

export class Daemon {
  readonly store: Store<DaemonState>;
  private readonly runListeners = new Set<(runs: RunSummary[]) => void>();

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
    this.store.set((s) => ({ ...s, runs: indexRuns(s.runs, runs), runsVersion: s.runsVersion + 1 }));
    for (const l of this.runListeners) l(runs);
  }

  async refreshAll(): Promise<void> {
    const c = this.client;
    try {
      const [status, rules, agents, sources, projects, errors] = await Promise.all([
        c.call("system.status", {}),
        c.call("rules.list", { include_archived: true }),
        c.call("agents.list", {}),
        c.call("sources.list", {}),
        c.call("projects.list", {}),
        c.call("system.config_errors", {}),
      ]);
      this.store.set((s) => ({
        ...s,
        status,
        rules: rules.rules,
        agents: agents.agents,
        sources: sources.sources,
        projects: projects.projects,
        configErrors: errors.errors,
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

  /** Active runs (all of them) and the newest finished ones. */
  async loadRuns(): Promise<void> {
    const c = this.client;
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
    this.store.set((s) => ({ ...s, runs: indexRuns(new Map(), [...recent.runs, ...active]), runsVersion: s.runsVersion + 1 }));
  }

  private async refetch(topics: readonly StateTopic[]): Promise<void> {
    const c = this.client;
    const t = new Set(topics);
    const tasks: Promise<void>[] = [];
    if (t.has("rules") || t.has("config")) {
      tasks.push(c.call("rules.list", { include_archived: true }).then((r) => this.store.set((s) => ({ ...s, rules: r.rules }))));
    }
    if (t.has("agents") || t.has("config")) {
      tasks.push(c.call("agents.list", {}).then((r) => this.store.set((s) => ({ ...s, agents: r.agents }))));
    }
    if (t.has("sources") || t.has("config")) {
      tasks.push(c.call("sources.list", {}).then((r) => this.store.set((s) => ({ ...s, sources: r.sources }))));
    }
    if (t.has("projects") || t.has("config")) {
      tasks.push(c.call("projects.list", {}).then((r) => this.store.set((s) => ({ ...s, projects: r.projects }))));
    }
    if (t.has("config")) {
      tasks.push(c.call("system.config_errors", {}).then((r) => this.store.set((s) => ({ ...s, configErrors: r.errors }))));
    }
    await Promise.allSettled(tasks);
  }
}

/** Merges run summaries into the index, keeping every active run and the newest finished ones. */
export function indexRuns(prev: ReadonlyMap<string, RunSummary>, updates: readonly RunSummary[]): Map<string, RunSummary> {
  const next = new Map(prev);
  for (const r of updates) next.set(r.id, r);
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
