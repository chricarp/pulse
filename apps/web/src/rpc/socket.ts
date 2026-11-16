import { WS_PATH, WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX } from "../contracts";
import { CLOSE_UNPROVEN, type Socket, type SocketFactory } from "./client";
import type { ProofResult } from "./proof";

/** Same origin as the page: in production the daemon serves both; in development Vite proxies `/ws`. */
export function socketUrl(loc: Pick<Location, "protocol" | "host"> = window.location): string {
  const scheme = loc.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${loc.host}${WS_PATH}`;
}

export interface SocketDeps {
  /** Asks the port to prove it knows the token (`requestProof`); called once per attempt. */
  prove: () => Promise<ProofResult>;
  /** Told the daemon instance each time one proves itself. */
  onProven?: (instance: string) => void;
  WebSocket?: new (url: string, protocols: string[]) => WebSocket;
}

/**
 * Each connection attempt first asks whatever listens on the port to prove that it knows the
 * token, and only then opens the WebSocket that carries it. One that cannot is closed with
 * `CLOSE_UNPROVEN` and never sees the token.
 *
 * Browsers cannot set headers on a WebSocket, so the token travels as a second offered
 * subprotocol, `pulse.token.<token>`. The daemon selects `pulse` and never echoes the token; a
 * wrong token is closed with 4401 (see `pulse_contracts::auth`).
 */
export function webSocketFactory(url: string, token: string, deps: SocketDeps): SocketFactory {
  const Ws = deps.WebSocket ?? WebSocket;
  return () => {
    let ws: WebSocket | null = null;
    let closed = false;
    const socket: Socket = {
      send: (data) => ws?.send(data),
      close: (code, reason) => {
        closed = true;
        ws?.close(code, reason);
      },
      onopen: null,
      onmessage: null,
      onclose: null,
    };
    void deps.prove().then((proof) => {
      if (closed) return;
      if (proof.kind === "unreachable") {
        socket.onclose?.(1006, "the daemon did not answer");
        return;
      }
      if (proof.kind === "unproven") {
        socket.onclose?.(CLOSE_UNPROVEN, proof.why);
        return;
      }
      deps.onProven?.(proof.instance);
      const w = new Ws(url, [WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX + token]);
      ws = w;
      w.onopen = () => socket.onopen?.();
      w.onmessage = (e) => {
        if (typeof e.data === "string") socket.onmessage?.(e.data);
      };
      w.onclose = (e) => socket.onclose?.(e.code, e.reason);
    });
    return socket;
  };
}
