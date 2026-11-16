import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CLOSE_UNAUTHORIZED, RpcCallError, RpcClient, type Socket } from "./client";

interface Frame {
  id?: number;
  method: string;
  params?: unknown;
}

/** A socket the test drives by hand: it decides when the server opens, answers and closes. */
class FakeSocket implements Socket {
  onopen: (() => void) | null = null;
  onmessage: ((data: string) => void) | null = null;
  onclose: ((code: number, reason: string) => void) | null = null;
  readonly sent: Frame[] = [];
  closed = false;

  send(data: string): void {
    this.sent.push(JSON.parse(data) as Frame);
  }
  close(): void {
    this.closed = true;
  }
  serverOpen(): void {
    this.onopen?.();
  }
  serverClose(code = 1006, reason = ""): void {
    this.onclose?.(code, reason);
  }
  reply(id: number | undefined, result: unknown): void {
    this.onmessage?.(JSON.stringify({ jsonrpc: "2.0", id, result }));
  }
  fail(id: number | undefined, code: number, message: string, data?: unknown): void {
    this.onmessage?.(JSON.stringify({ jsonrpc: "2.0", id, error: { code, message, data } }));
  }
  notify(method: string, params: unknown): void {
    this.onmessage?.(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }
  last(method: string): Frame {
    const f = [...this.sent].reverse().find((x) => x.method === method);
    if (!f) throw new Error(`no ${method} sent`);
    return f;
  }
}

function setup(opts: { timeoutMs?: number } = {}) {
  const sockets: FakeSocket[] = [];
  const client = new RpcClient({
    connect: () => {
      const s = new FakeSocket();
      sockets.push(s);
      return s;
    },
    backoffMs: [100, 200, 400],
    jitter: 0,
    timeoutMs: opts.timeoutMs ?? 1000,
  });
  const current = () => {
    const s = sockets.at(-1);
    if (!s) throw new Error("no socket");
    return s;
  };
  return { client, sockets, current };
}

const status = { paused: false } as never;

describe("RpcClient", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("matches responses to requests by id when they arrive out of order", async () => {
    const { client, current } = setup();
    client.start();
    current().serverOpen();
    const a = client.call("rules.get", { name: "a" });
    const b = client.call("rules.get", { name: "b" });
    const [fa, fb] = current().sent;
    current().reply(fb?.id, { config: { name: "b" } });
    current().reply(fa?.id, { config: { name: "a" } });
    await expect(a).resolves.toMatchObject({ config: { name: "a" } });
    await expect(b).resolves.toMatchObject({ config: { name: "b" } });
  });

  it("surfaces JSON-RPC errors with their code and data", async () => {
    const { client, current } = setup();
    client.start();
    current().serverOpen();
    const p = client.call("rules.upsert", { rule: {} as never });
    current().fail(current().last("rules.upsert").id, -32004, "invalid", { errors: [{ message: "x" }] });
    const err = await p.catch((e: unknown) => e);
    expect(err).toBeInstanceOf(RpcCallError);
    expect(err).toMatchObject({ kind: "rpc", code: -32004, data: { errors: [{ message: "x" }] } });
  });

  it("times out a request the daemon never answers, and ignores a late answer", async () => {
    const { client, current } = setup({ timeoutMs: 500 });
    client.start();
    current().serverOpen();
    const p = client.call("system.status", {});
    const caught = p.catch((e: unknown) => e);
    vi.advanceTimersByTime(500);
    await expect(caught).resolves.toMatchObject({ kind: "timeout", method: "system.status" });
    current().reply(current().last("system.status").id, status);
  });

  it("queues calls made before the connection opens and sends them in order", async () => {
    const { client, current } = setup();
    client.start();
    const p1 = client.call("rules.get", { name: "one" });
    const p2 = client.call("rules.get", { name: "two" });
    expect(current().sent).toHaveLength(0);
    current().serverOpen();
    expect(current().sent.map((f) => (f.params as { name: string }).name)).toEqual(["one", "two"]);
    current().reply(current().sent[0]?.id, 1);
    current().reply(current().sent[1]?.id, 2);
    await expect(Promise.all([p1, p2])).resolves.toEqual([1, 2]);
  });

  it("fails in-flight calls on disconnect instead of resending them", async () => {
    const { client, sockets, current } = setup();
    client.start();
    current().serverOpen();
    const p = client.call("runs.approve", { id: "R1" });
    const caught = p.catch((e: unknown) => e);
    current().serverClose();
    await expect(caught).resolves.toMatchObject({ kind: "disconnected" });
    vi.advanceTimersByTime(100);
    expect(sockets).toHaveLength(2);
    current().serverOpen();
    expect(current().sent.filter((f) => f.method === "runs.approve")).toHaveLength(0);
  });

  it("reconnects with backoff and reports each state", () => {
    const { client, sockets, current } = setup();
    const kinds: string[] = [];
    client.onStateChange((s) => kinds.push(s.kind));
    client.start();
    current().serverClose();
    expect(client.state).toMatchObject({ kind: "waiting", attempt: 1 });
    vi.advanceTimersByTime(99);
    expect(sockets).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(sockets).toHaveLength(2);
    current().serverClose();
    vi.advanceTimersByTime(199);
    expect(sockets).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(sockets).toHaveLength(3);
    current().serverOpen();
    // Backoff resets after a successful open.
    current().serverClose();
    vi.advanceTimersByTime(100);
    expect(sockets).toHaveLength(4);
    expect(kinds.slice(0, 6)).toEqual(["connecting", "waiting", "connecting", "waiting", "connecting", "open"]);
  });

  it("retryNow skips the remaining backoff", () => {
    const { client, sockets, current } = setup();
    client.start();
    current().serverClose();
    client.retryNow();
    expect(sockets).toHaveLength(2);
  });

  it("stops retrying when the daemon rejects the token", async () => {
    const { client, sockets, current } = setup();
    client.start();
    const p = client.call("system.status", {}).catch((e: unknown) => e);
    current().serverClose(CLOSE_UNAUTHORIZED, "bad token");
    expect(client.state).toEqual({ kind: "unauthorized", reason: "bad token" });
    await expect(p).resolves.toMatchObject({ kind: "closed" });
    vi.advanceTimersByTime(60_000);
    expect(sockets).toHaveLength(1);
  });

  it("resubscribes after a reconnect with fresh params and routes by the new id", async () => {
    const { client, current } = setup();
    client.start();
    current().serverOpen();
    let after = 0;
    const got: number[] = [];
    const lifecycle: string[] = [];
    client.subscribe(
      "runs.subscribe_output",
      () => ({ id: "R1", after }),
      (n) => {
        for (const c of n.chunks) {
          got.push(c.seq);
          after = c.seq;
        }
      },
      {
        onSubscribed: ({ resumed }) => lifecycle.push(resumed ? "resumed" : "subscribed"),
        onLost: () => lifecycle.push("lost"),
      },
    );
    current().reply(current().last("runs.subscribe_output").id, { subscription: "S1" });
    await vi.waitFor(() => expect(lifecycle).toEqual(["subscribed"]));
    current().notify("runs.output", { subscription: "S1", run: "R1", chunks: [{ seq: 1 }, { seq: 2 }], done: false });

    current().serverClose();
    vi.advanceTimersByTime(100);
    current().serverOpen();
    const again = current().last("runs.subscribe_output");
    expect(again.params).toEqual({ id: "R1", after: 2 });
    current().reply(again.id, { subscription: "S2" });
    await vi.waitFor(() => expect(lifecycle).toEqual(["subscribed", "lost", "resumed"]));
    current().notify("runs.output", { subscription: "S1", run: "R1", chunks: [{ seq: 99 }], done: false });
    current().notify("runs.output", { subscription: "S2", run: "R1", chunks: [{ seq: 3 }], done: false });
    expect(got).toEqual([1, 2, 3]);
  });

  it("holds notifications that arrive before the subscribe response", async () => {
    const { client, current } = setup();
    client.start();
    current().serverOpen();
    const seen: number[] = [];
    client.subscribe("events.subscribe", () => ({ filter: {} as never }), (b) => seen.push(b.dropped));
    const sub = current().last("events.subscribe");
    current().notify("events.batch", { subscription: "E1", events: [], dropped: 7 });
    expect(seen).toEqual([]);
    current().reply(sub.id, { subscription: "E1" });
    await vi.waitFor(() => expect(seen).toEqual([7]));
  });

  it("unsubscribes on close, including a subscription closed while its call was in flight", async () => {
    const { client, current } = setup();
    client.start();
    current().serverOpen();
    const sub = client.subscribe("runs.subscribe", () => ({ filter: {} as never }), () => {});
    const frame = current().last("runs.subscribe");
    sub.close();
    current().reply(frame.id, { subscription: "X1" });
    await vi.waitFor(() =>
      expect(current().last("runs.unsubscribe").params).toEqual({ subscription: "X1" }),
    );
  });

  it("delivers state.changed to every listener", () => {
    const { client, current } = setup();
    client.start();
    current().serverOpen();
    const topics: string[][] = [];
    client.onDaemonStateChanged((c) => topics.push(c.topics));
    current().notify("state.changed", { status, topics: ["rules"] });
    expect(topics).toEqual([["rules"]]);
  });
});
