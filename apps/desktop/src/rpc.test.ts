import { describe, expect, it } from "vitest";

import { RpcClient, type SocketLike } from "./rpc.ts";

class FakeSocket implements SocketLike {
  protocol = "pulse";
  sent: Record<string, unknown>[] = [];
  closed = false;
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: unknown }) => void) | null = null;
  onclose: ((ev: unknown) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  constructor(
    readonly url: string,
    readonly protocols: string[],
  ) {}
  send(data: string) {
    this.sent.push(JSON.parse(data) as Record<string, unknown>);
  }
  close() {
    this.closed = true;
  }
  receive(msg: unknown) {
    this.onmessage?.({ data: JSON.stringify(msg) });
  }
}

async function connected() {
  let socket: FakeSocket | undefined;
  const pending = RpcClient.connect({
    port: 4242,
    token: "t".repeat(64),
    factory: (url, protocols) => (socket = new FakeSocket(url, protocols)),
  });
  socket!.onopen?.({});
  return { client: await pending, socket: socket! };
}

describe("RpcClient", () => {
  it("authenticates with the subprotocol, never the URL", async () => {
    const { socket } = await connected();
    expect(socket.url).toBe("ws://127.0.0.1:4242/ws");
    expect(socket.protocols).toEqual(["pulse", `pulse.token.${"t".repeat(64)}`]);
  });

  it("matches responses to requests, in any order, and passes notifications on", async () => {
    const { client, socket } = await connected();
    const notes: unknown[] = [];
    client.onNotification((n) => notes.push(n));
    const a = client.call("system.pause", {});
    const b = client.call("runs.get", { id: "r1" });
    expect(socket.sent.map((m) => [m.id, m.method])).toEqual([
      [1, "system.pause"],
      [2, "runs.get"],
    ]);
    socket.receive({ jsonrpc: "2.0", method: "state.changed", params: { topics: ["pause"] } });
    socket.receive({ jsonrpc: "2.0", id: 2, error: { code: -32004, message: "no run r1" } });
    socket.receive({ jsonrpc: "2.0", id: 1, result: { paused: true } });
    await expect(b).rejects.toMatchObject({ message: "no run r1", code: -32004 });
    await expect(a).resolves.toEqual({ paused: true });
    expect(notes).toEqual([{ method: "state.changed", params: { topics: ["pause"] } }]);
  });

  it("a dropped connection fails what is in flight and is reported once", async () => {
    const { client, socket } = await connected();
    let closes = 0;
    client.onClose(() => closes++);
    const p = client.call("system.status", {});
    socket.onclose?.({});
    socket.onerror?.({});
    await expect(p).rejects.toThrow("closed");
    expect(closes).toBe(1);
    await expect(client.call("system.status", {})).rejects.toThrow("not connected");
  });

  it("closing on purpose is not reported as a drop", async () => {
    const { client, socket } = await connected();
    let closes = 0;
    client.onClose(() => closes++);
    client.close();
    socket.onclose?.({});
    expect(closes).toBe(0);
    expect(socket.closed).toBe(true);
  });

  it("a refused connection rejects", async () => {
    let socket: FakeSocket | undefined;
    const p = RpcClient.connect({ port: 1, token: "x", factory: (u, pr) => (socket = new FakeSocket(u, pr)) });
    socket!.onerror?.({});
    await expect(p).rejects.toThrow("cannot connect");
  });
});
