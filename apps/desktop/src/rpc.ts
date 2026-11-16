// A JSON-RPC 2.0 client over the daemon's WebSocket, the same protocol the CLI and the web UI
// use. One connection; server pushes arrive as notifications. Nothing runs while nothing
// arrives: the only timers are per-request timeouts while a request is in flight.

import type { Notification, RpcMethodName, RpcMethods } from "./contracts.ts";

/** The subset of the WHATWG WebSocket the client needs (injected in tests). */
export interface SocketLike {
  readonly protocol: string;
  send(data: string): void;
  close(): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: unknown) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}

export type SocketFactory = (url: string, protocols: string[]) => SocketLike;

export const defaultSocketFactory: SocketFactory = (url, protocols) =>
  new WebSocket(url, protocols) as unknown as SocketLike;

export class RpcError extends Error {
  constructor(
    message: string,
    readonly code: number | null,
  ) {
    super(message);
    this.name = "RpcError";
  }
}

interface Pending {
  resolve(v: unknown): void;
  reject(e: Error): void;
  timer: ReturnType<typeof setTimeout>;
}

export interface ConnectOptions {
  port: number;
  token: string;
  factory?: SocketFactory;
  /** For the connection and for each request. */
  timeoutMs?: number;
}

export class RpcClient {
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private closed = false;
  private notificationHandler: (n: Notification) => void = () => {};
  private closeHandler: () => void = () => {};

  private constructor(
    private readonly socket: SocketLike,
    private readonly timeoutMs: number,
  ) {
    socket.onmessage = (ev) => this.receive(ev.data);
    socket.onclose = () => this.shutdown();
    socket.onerror = () => this.shutdown();
  }

  /**
   * Opens the connection. The token travels as a WebSocket subprotocol, like the browser's; it
   * is never put in a URL or a log.
   */
  static connect(opts: ConnectOptions): Promise<RpcClient> {
    const factory = opts.factory ?? defaultSocketFactory;
    const timeoutMs = opts.timeoutMs ?? 10_000;
    return new Promise((resolve, reject) => {
      let socket: SocketLike;
      try {
        socket = factory(`ws://127.0.0.1:${opts.port}/ws`, ["pulse", `pulse.token.${opts.token}`]);
      } catch (e) {
        reject(new RpcError(`cannot connect: ${(e as Error).message}`, null));
        return;
      }
      const fail = (why: string) => {
        clearTimeout(timer);
        socket.onopen = socket.onclose = socket.onerror = null;
        try {
          socket.close();
        } catch {
          // Already closed.
        }
        reject(new RpcError(why, null));
      };
      const timer = setTimeout(() => fail("the daemon did not answer in time"), timeoutMs);
      socket.onopen = () => {
        clearTimeout(timer);
        resolve(new RpcClient(socket, timeoutMs));
      };
      socket.onerror = () => fail("cannot connect to the daemon");
      socket.onclose = () => fail("the daemon closed the connection");
    });
  }

  onNotification(handler: (n: Notification) => void): void {
    this.notificationHandler = handler;
  }

  /** Called once, when the connection ends for any reason other than `close()`. */
  onClose(handler: () => void): void {
    this.closeHandler = handler;
  }

  get isOpen(): boolean {
    return !this.closed;
  }

  call<M extends RpcMethodName>(method: M, params: RpcMethods[M]["params"]): Promise<RpcMethods[M]["result"]> {
    if (this.closed) return Promise.reject(new RpcError("not connected to the daemon", null));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new RpcError(`${method} timed out`, null));
      }, this.timeoutMs);
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject, timer });
      try {
        this.socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      } catch (e) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(new RpcError(`cannot send ${method}: ${(e as Error).message}`, null));
      }
    });
  }

  /** Closes without calling the close handler: the caller knows. */
  close(): void {
    this.closeHandler = () => {};
    try {
      this.socket.close();
    } catch {
      // Already closed.
    }
    this.shutdown();
  }

  private receive(data: unknown): void {
    if (typeof data !== "string") return;
    let msg: Record<string, unknown>;
    try {
      msg = JSON.parse(data) as Record<string, unknown>;
    } catch {
      return;
    }
    if (typeof msg.id === "number") {
      const p = this.pending.get(msg.id);
      if (p === undefined) return;
      this.pending.delete(msg.id);
      clearTimeout(p.timer);
      const err = msg.error as { message?: unknown; code?: unknown } | undefined;
      if (err !== undefined && err !== null) {
        p.reject(
          new RpcError(
            typeof err.message === "string" ? err.message : "the daemon returned an error",
            typeof err.code === "number" ? err.code : null,
          ),
        );
      } else {
        p.resolve(msg.result);
      }
      return;
    }
    if (typeof msg.method === "string") {
      this.notificationHandler({ method: msg.method, params: msg.params } as Notification);
    }
  }

  private shutdown(): void {
    if (this.closed) return;
    this.closed = true;
    this.socket.onopen = this.socket.onmessage = this.socket.onclose = this.socket.onerror = null;
    for (const p of this.pending.values()) {
      clearTimeout(p.timer);
      p.reject(new RpcError("the connection to the daemon closed", null));
    }
    this.pending.clear();
    this.closeHandler();
  }
}
