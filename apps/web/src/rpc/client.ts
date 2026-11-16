// The one RPC client: JSON-RPC 2.0 over a socket, typed by the generated method map.
//
// - Requests made while disconnected wait (bounded) for the connection, within their timeout.
// - Requests in flight when the connection drops fail with `disconnected`; they are never
//   resent, because the daemon may already have acted on them (approve, delete).
// - Subscriptions are remembered and re-issued after every reconnect. Their params are a
//   function so a resubscription can resume where it left off (run output `after`).
// - A notification for a subscription id the client has not learned yet (the response to the
//   subscribe call is still in flight) is held briefly and delivered once the id is known.

import type {
  EventsBatch,
  RpcMethodName,
  RpcMethods,
  RunsChanged,
  RunsOutput,
  StateChanged,
} from "../contracts";

/** The minimum a transport has to offer. A browser WebSocket and the mock both fit. */
export interface Socket {
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: (() => void) | null;
  onmessage: ((data: string) => void) | null;
  onclose: ((code: number, reason: string) => void) | null;
}

export type SocketFactory = () => Socket;

/** The daemon closes with this code when the token is missing or wrong. Retrying will not help. */
export const CLOSE_UNAUTHORIZED = 4401;

export type ConnectionState =
  | { kind: "connecting"; attempt: number }
  | { kind: "open"; since: number }
  | { kind: "waiting"; attempt: number; retryAt: number; code: number; reason: string }
  | { kind: "unauthorized"; reason: string }
  | { kind: "closed" };

export type CallErrorKind = "rpc" | "timeout" | "disconnected" | "closed" | "overloaded";

export class RpcCallError extends Error {
  readonly kind: CallErrorKind;
  readonly method: string;
  /** JSON-RPC error code when `kind` is `rpc`. */
  readonly code: number | undefined;
  readonly data: unknown;

  constructor(kind: CallErrorKind, method: string, message: string, code?: number, data?: unknown) {
    super(message);
    this.name = "RpcCallError";
    this.kind = kind;
    this.method = method;
    this.code = code;
    this.data = data;
  }
}

/** Error codes from `pulse_contracts::rpc::ErrorCode`. */
export const ErrorCodes = {
  InvalidParams: -32602,
  Unauthorized: -32001,
  NotFound: -32002,
  Conflict: -32003,
  ConfigInvalid: -32004,
  Unavailable: -32005,
  NotImplemented: -32006,
} as const;

type SubscribeMap = {
  "events.subscribe": { notification: "events.batch"; payload: EventsBatch };
  "runs.subscribe": { notification: "runs.changed"; payload: RunsChanged };
  "runs.subscribe_output": { notification: "runs.output"; payload: RunsOutput };
};
export type SubscribeMethod = keyof SubscribeMap;

const UNSUBSCRIBE: Record<SubscribeMethod, "events.unsubscribe" | "runs.unsubscribe"> = {
  "events.subscribe": "events.unsubscribe",
  "runs.subscribe": "runs.unsubscribe",
  "runs.subscribe_output": "runs.unsubscribe",
};

export interface SubscriptionHooks {
  /** The daemon accepted the subscription (first time, or again after a reconnect). */
  onSubscribed?: (info: { resumed: boolean }) => void;
  /** The connection dropped; notifications may be missed until `onSubscribed` fires again. */
  onLost?: () => void;
  /** The daemon refused the subscription. It is not retried until the next reconnect. */
  onError?: (error: RpcCallError) => void;
}

export interface Subscription {
  close(): void;
}

export interface ClientOptions {
  connect: SocketFactory;
  /** Default per-call timeout. */
  timeoutMs?: number;
  /** Reconnect delays; the last one repeats. */
  backoffMs?: readonly number[];
  /** 0..1, fraction of the delay added at random. */
  jitter?: number;
  random?: () => number;
  now?: () => number;
  /** Requests waiting for a connection, at most. */
  maxQueued?: number;
}

interface Pending {
  method: string;
  resolve: (value: unknown) => void;
  reject: (err: RpcCallError) => void;
  timer: ReturnType<typeof setTimeout>;
  sent: boolean;
  frame: string;
}

interface SubRecord {
  key: number;
  method: SubscribeMethod;
  params: () => unknown;
  notify: (payload: never) => void;
  hooks: SubscriptionHooks;
  serverId: string | null;
  /** Bumped on every (re)subscribe so a stale response cannot claim the record. */
  generation: number;
  everSubscribed: boolean;
  closed: boolean;
}

const ORPHAN_LIMIT = 64;
const DEFAULT_BACKOFF = [250, 500, 1000, 2000, 4000, 8000, 15000, 30000] as const;

export class RpcClient {
  private readonly opts: Required<Omit<ClientOptions, "connect">> & { connect: SocketFactory };
  private socket: Socket | null = null;
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private readonly queue: number[] = [];
  private readonly subs = new Map<number, SubRecord>();
  private readonly byServerId = new Map<string, SubRecord>();
  private readonly orphans = new Map<string, unknown[]>();
  private orphanCount = 0;
  private nextSubKey = 1;
  private attempt = 0;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private stateValue: ConnectionState = { kind: "closed" };
  private readonly stateListeners = new Set<(s: ConnectionState) => void>();
  private readonly changeListeners = new Set<(c: StateChanged) => void>();
  private readonly openListeners = new Set<(info: { reconnect: boolean }) => void>();
  private everOpened = false;
  private stopped = true;

  constructor(options: ClientOptions) {
    this.opts = {
      timeoutMs: 15_000,
      backoffMs: DEFAULT_BACKOFF,
      jitter: 0.2,
      random: Math.random,
      now: Date.now,
      maxQueued: 256,
      ...options,
    };
  }

  get state(): ConnectionState {
    return this.stateValue;
  }

  /** Starts connecting. Idempotent. */
  start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    this.open();
  }

  /** Closes for good: pending calls fail, subscriptions are dropped. */
  stop(): void {
    this.stopped = true;
    this.clearRetry();
    const s = this.socket;
    this.socket = null;
    if (s) {
      s.onclose = null;
      s.onmessage = null;
      s.onopen = null;
      s.close(1000, "client closed");
    }
    for (const [id, p] of this.pending) {
      clearTimeout(p.timer);
      p.reject(new RpcCallError("closed", p.method, "The client was closed."));
      this.pending.delete(id);
    }
    this.queue.length = 0;
    this.subs.clear();
    this.byServerId.clear();
    this.orphans.clear();
    this.orphanCount = 0;
    this.setState({ kind: "closed" });
  }

  /** Skips the remaining backoff (window focus, network back, a "Retry" button). */
  retryNow(): void {
    if (this.stopped || this.stateValue.kind !== "waiting") return;
    this.clearRetry();
    this.open();
  }

  call<M extends RpcMethodName>(
    method: M,
    params: RpcMethods[M]["params"],
    options: { timeoutMs?: number } = {},
  ): Promise<RpcMethods[M]["result"]> {
    return new Promise((resolve, reject) => {
      if (this.stopped && this.stateValue.kind === "closed" && !this.socket) {
        reject(new RpcCallError("closed", method, "Not connected to the daemon."));
        return;
      }
      if (this.stateValue.kind === "unauthorized") {
        reject(new RpcCallError("closed", method, "The daemon rejected the token."));
        return;
      }
      const id = this.nextId++;
      const frame = JSON.stringify({ jsonrpc: "2.0", id, method, params });
      const timeoutMs = options.timeoutMs ?? this.opts.timeoutMs;
      const timer = setTimeout(() => {
        const p = this.pending.get(id);
        if (!p) return;
        this.pending.delete(id);
        const qi = this.queue.indexOf(id);
        if (qi >= 0) this.queue.splice(qi, 1);
        p.reject(
          new RpcCallError(
            "timeout",
            method,
            p.sent
              ? `${method} did not answer within ${Math.round(timeoutMs / 1000)}s.`
              : `Not connected to the daemon (waited ${Math.round(timeoutMs / 1000)}s).`,
          ),
        );
      }, timeoutMs);
      const entry: Pending = {
        method,
        resolve: resolve as (v: unknown) => void,
        reject,
        timer,
        sent: false,
        frame,
      };
      this.pending.set(id, entry);
      if (this.stateValue.kind === "open" && this.socket) {
        entry.sent = true;
        this.socket.send(frame);
      } else {
        if (this.queue.length >= this.opts.maxQueued) {
          clearTimeout(timer);
          this.pending.delete(id);
          reject(new RpcCallError("overloaded", method, "Too many requests waiting for the daemon."));
          return;
        }
        this.queue.push(id);
      }
    });
  }

  subscribe<M extends SubscribeMethod>(
    method: M,
    params: () => RpcMethods[M]["params"],
    notify: (payload: SubscribeMap[M]["payload"]) => void,
    hooks: SubscriptionHooks = {},
  ): Subscription {
    const rec: SubRecord = {
      key: this.nextSubKey++,
      method,
      params,
      notify: notify as (payload: never) => void,
      hooks,
      serverId: null,
      generation: 0,
      everSubscribed: false,
      closed: false,
    };
    this.subs.set(rec.key, rec);
    if (this.stateValue.kind === "open") this.issueSubscribe(rec);
    return {
      close: () => {
        if (rec.closed) return;
        rec.closed = true;
        this.subs.delete(rec.key);
        if (rec.serverId) {
          this.byServerId.delete(rec.serverId);
          this.orphans.delete(rec.serverId);
          const id = rec.serverId;
          rec.serverId = null;
          if (this.stateValue.kind === "open") {
            this.call(UNSUBSCRIBE[rec.method], { subscription: id }).catch(() => {
              // The subscription dies with the connection anyway.
            });
          }
        }
      },
    };
  }

  onStateChange(listener: (s: ConnectionState) => void): () => void {
    this.stateListeners.add(listener);
    return () => this.stateListeners.delete(listener);
  }

  /** `state.changed`, sent to every connection. */
  onDaemonStateChanged(listener: (c: StateChanged) => void): () => void {
    this.changeListeners.add(listener);
    return () => this.changeListeners.delete(listener);
  }

  /** Each time the connection opens; `reconnect` is true after the first. */
  onOpen(listener: (info: { reconnect: boolean }) => void): () => void {
    this.openListeners.add(listener);
    return () => this.openListeners.delete(listener);
  }

  // ---- internals ----

  private setState(s: ConnectionState): void {
    this.stateValue = s;
    for (const l of this.stateListeners) l(s);
  }

  private clearRetry(): void {
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
  }

  private open(): void {
    this.setState({ kind: "connecting", attempt: this.attempt + 1 });
    let socket: Socket;
    try {
      socket = this.opts.connect();
    } catch {
      this.attempt += 1;
      this.scheduleRetry(1006, "could not open a connection");
      return;
    }
    this.socket = socket;
    socket.onopen = () => {
      if (this.socket !== socket) return;
      this.attempt = 0;
      const reconnect = this.everOpened;
      this.everOpened = true;
      this.setState({ kind: "open", since: this.opts.now() });
      // Queued requests first, in call order, then subscriptions.
      const queued = this.queue.splice(0);
      for (const id of queued) {
        const p = this.pending.get(id);
        if (!p) continue;
        p.sent = true;
        socket.send(p.frame);
      }
      for (const rec of this.subs.values()) this.issueSubscribe(rec);
      for (const l of this.openListeners) l({ reconnect });
    };
    socket.onmessage = (data) => {
      if (this.socket !== socket) return;
      this.receive(data);
    };
    socket.onclose = (code, reason) => {
      if (this.socket !== socket) return;
      this.socket = null;
      this.connectionLost();
      if (this.stopped) return;
      if (code === CLOSE_UNAUTHORIZED) {
        this.setState({ kind: "unauthorized", reason: reason || "The daemon rejected the token." });
        this.failQueued("The daemon rejected the token.");
        return;
      }
      this.attempt += 1;
      this.scheduleRetry(code, reason);
    };
  }

  private scheduleRetry(code: number, reason: string): void {
    const delays = this.opts.backoffMs;
    const base = delays[Math.min(this.attempt - 1, delays.length - 1)] ?? 1000;
    const delay = Math.round(base * (1 + this.opts.jitter * this.opts.random()));
    this.setState({
      kind: "waiting",
      attempt: this.attempt,
      retryAt: this.opts.now() + delay,
      code,
      reason,
    });
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      this.open();
    }, delay);
  }

  private connectionLost(): void {
    for (const [id, p] of this.pending) {
      if (!p.sent) continue;
      clearTimeout(p.timer);
      this.pending.delete(id);
      p.reject(
        new RpcCallError(
          "disconnected",
          p.method,
          "The connection to the daemon dropped before it answered. The request may or may not have taken effect.",
        ),
      );
    }
    this.byServerId.clear();
    this.orphans.clear();
    this.orphanCount = 0;
    for (const rec of this.subs.values()) {
      const had = rec.serverId !== null || rec.everSubscribed;
      rec.serverId = null;
      rec.generation += 1;
      if (had) rec.hooks.onLost?.();
    }
  }

  private failQueued(message: string): void {
    for (const id of this.queue.splice(0)) {
      const p = this.pending.get(id);
      if (!p) continue;
      clearTimeout(p.timer);
      this.pending.delete(id);
      p.reject(new RpcCallError("closed", p.method, message));
    }
  }

  private issueSubscribe(rec: SubRecord): void {
    rec.generation += 1;
    const generation = rec.generation;
    const resumed = rec.everSubscribed;
    let params: unknown;
    try {
      params = rec.params();
    } catch {
      return;
    }
    this.call(rec.method, params as never)
      .then((result) => {
        const id = (result as { subscription: string }).subscription;
        if (rec.closed || rec.generation !== generation) {
          // Closed (or the connection turned over) while the call was in flight.
          if (rec.closed && this.stateValue.kind === "open") {
            this.call(UNSUBSCRIBE[rec.method], { subscription: id }).catch(() => {});
          }
          return;
        }
        rec.serverId = id;
        rec.everSubscribed = true;
        this.byServerId.set(id, rec);
        rec.hooks.onSubscribed?.({ resumed });
        const held = this.orphans.get(id);
        if (held) {
          this.orphans.delete(id);
          this.orphanCount -= held.length;
          for (const payload of held) rec.notify(payload as never);
        }
      })
      .catch((err: unknown) => {
        if (rec.closed || rec.generation !== generation) return;
        if (err instanceof RpcCallError && err.kind === "disconnected") return;
        rec.hooks.onError?.(
          err instanceof RpcCallError
            ? err
            : new RpcCallError("rpc", rec.method, String(err)),
        );
      });
  }

  private receive(data: string): void {
    let msg: unknown;
    try {
      msg = JSON.parse(data);
    } catch {
      return;
    }
    if (typeof msg !== "object" || msg === null) return;
    const m = msg as Record<string, unknown>;
    if ("id" in m && ("result" in m || "error" in m)) {
      if (typeof m.id !== "number") return;
      const p = this.pending.get(m.id);
      if (!p) return; // Timed out already, or not ours.
      this.pending.delete(m.id);
      clearTimeout(p.timer);
      if ("error" in m && m.error) {
        const e = m.error as { code?: number; message?: string; data?: unknown };
        p.reject(new RpcCallError("rpc", p.method, e.message ?? "error", e.code, e.data));
      } else {
        p.resolve(m.result);
      }
      return;
    }
    if (typeof m.method !== "string") return;
    const params = m.params as Record<string, unknown> | undefined;
    if (m.method === "state.changed") {
      for (const l of this.changeListeners) l(params as unknown as StateChanged);
      return;
    }
    const subId = params?.subscription;
    if (typeof subId !== "string") return;
    const rec = this.byServerId.get(subId);
    if (rec) {
      rec.notify(params as never);
      return;
    }
    // Not known yet: the subscribe response may still be on its way. Hold a few.
    if (this.orphanCount >= ORPHAN_LIMIT) {
      const oldest = this.orphans.keys().next();
      if (!oldest.done) {
        this.orphanCount -= this.orphans.get(oldest.value)?.length ?? 0;
        this.orphans.delete(oldest.value);
      }
    }
    const list = this.orphans.get(subId) ?? [];
    list.push(params);
    this.orphans.set(subId, list);
    this.orphanCount += 1;
  }
}
