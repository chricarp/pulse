// The mock as a socket: frames are JSON strings both ways, delivered asynchronously and in
// order, with a little latency. Handlers run concurrently, so responses can overtake each other
// the way they can with the real daemon.

import type { Socket, SocketFactory } from "../rpc/client";
import type { Conn, MockDaemon } from "./daemon";

export function mockSocketFactory(daemon: MockDaemon, opts: { latencyMs?: [number, number] } = {}): SocketFactory {
  const [lo, hi] = opts.latencyMs ?? [2, 12];
  return () => {
    let conn: Conn | null = null;
    let open = false;
    let lastDelivery = 0;
    const socket: Socket = {
      onopen: null,
      onmessage: null,
      onclose: null,
      send(data: string) {
        if (!open || !conn) return;
        const frame = JSON.parse(data) as { id?: number; method?: string; params?: unknown };
        const c = conn;
        setTimeout(() => {
          void daemon.handle(c, frame).then((resp) => {
            if (frame.id !== undefined) deliver(JSON.stringify(resp));
          });
        }, lo + Math.random() * (hi - lo));
      },
      close(code = 1000, reason = "") {
        if (!open) return;
        open = false;
        if (conn) daemon.disconnect(conn);
        conn = null;
        setTimeout(() => socket.onclose?.(code, reason), 0);
      },
    };
    const deliver = (text: string) => {
      // In order, like one TCP stream: never before the previous frame.
      const at = Math.max(performance.now() + lo, lastDelivery);
      lastDelivery = at;
      setTimeout(() => {
        if (open) socket.onmessage?.(text);
      }, at - performance.now());
    };
    setTimeout(() => {
      if (!daemon.available) {
        socket.onclose?.(1006, "");
        return;
      }
      open = true;
      conn = daemon.connect(
        (frame) => deliver(JSON.stringify(frame)),
        () => {
          if (!open) return;
          open = false;
          conn = null;
          socket.onclose?.(1006, "");
        },
      );
      socket.onopen?.();
    }, 20);
    return socket;
  };
}
