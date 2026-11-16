import { WS_PATH, WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX } from "../contracts";
import type { Socket, SocketFactory } from "./client";

/** Same origin as the page: in production the daemon serves both; in development Vite proxies `/ws`. */
export function socketUrl(loc: Pick<Location, "protocol" | "host"> = window.location): string {
  const scheme = loc.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${loc.host}${WS_PATH}`;
}

/**
 * Browsers cannot set headers on a WebSocket, so the token travels as a second offered
 * subprotocol, `pulse.token.<token>`. The daemon selects `pulse` and never echoes the token; a
 * wrong token is closed with 4401 (see `pulse_contracts::auth`).
 */
export function webSocketFactory(url: string, token: string): SocketFactory {
  return () => {
    const ws = new WebSocket(url, [WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX + token]);
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
