import type { Socket, SocketFactory } from "./client";

/** The subprotocol the app speaks. The daemon must echo it back on the upgrade. */
export const PROTOCOL = "pulse.v1";
/**
 * Browsers cannot set headers on a WebSocket, so the token travels as a second offered
 * subprotocol, `pulse.token.<token>`. The daemon reads it from `Sec-WebSocket-Protocol`, checks
 * it, and selects `pulse.v1` (never echoing the token).
 */
export const TOKEN_PROTOCOL_PREFIX = "pulse.token.";

/** Same origin as the page: in production the daemon serves both; in development Vite proxies `/ws`. */
export function socketUrl(loc: Pick<Location, "protocol" | "host"> = window.location): string {
  const scheme = loc.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${loc.host}/ws`;
}

export function webSocketFactory(url: string, token: string): SocketFactory {
  return () => {
    const ws = new WebSocket(url, [PROTOCOL, TOKEN_PROTOCOL_PREFIX + token]);
    const socket: Socket = {
      send: (data) => ws.send(data),
      close: (code, reason) => ws.close(code, reason),
      onopen: null,
      onmessage: null,
      onclose: null,
    };
    ws.onopen = () => socket.onopen?.();
    ws.onmessage = (e) => {
      if (typeof e.data === "string") socket.onmessage?.(e.data);
    };
    ws.onclose = (e) => socket.onclose?.(e.code, e.reason);
    return socket;
  };
}
