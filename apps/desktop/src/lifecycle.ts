// The daemon's lifecycle as the shell sees it, as a pure state machine. The driver in
// `shell.ts` performs the effects and feeds the outcomes back as events.
//
// Policy (docs/internals/0010-desktop-shell.md):
// - On launch, use a daemon that already answers for this Pulse home; start the bundled one only
//   if there is none.
// - Quitting the app never stops the daemon. Only the explicit "Quit Pulse and stop the daemon"
//   (or "Stop the daemon") does, whoever started it.
// - A daemon that goes away while the app is open is not restarted behind the user's back (they
//   may have stopped it on purpose). The shell says so, offers to start it, and picks up a daemon
//   started some other way (`pulse start`) by watching the Pulse home for `daemon.json`.

import type { DaemonInfo } from "./contracts.ts";

export interface Daemon {
  info: DaemonInfo;
  /** Started by this app session. Shown to the user; it does not change the quit policy. */
  owned: boolean;
}

export type AfterStop = "quit" | "stay" | "start";

export type LifecycleState =
  /** Reading `daemon.json` and checking that the daemon answers. */
  | { kind: "locating"; spawnIfMissing: boolean }
  | { kind: "starting" }
  | { kind: "connected"; daemon: Daemon }
  | { kind: "stopping"; daemon: Daemon; then: AfterStop }
  | { kind: "unreachable"; reason: "lost" | "start_failed" | "stopped"; detail: string | null }
  | { kind: "quitting" };

export type LifecycleEvent =
  /** A daemon for this home answered (and the RPC connection to it is open). */
  | { type: "found"; info: DaemonInfo }
  /** No `daemon.json`, or a stale one whose daemon does not answer. */
  | { type: "missing" }
  | { type: "started"; info: DaemonInfo }
  | { type: "start_failed"; error: string }
  /** The RPC connection closed without the shell asking for it. */
  | { type: "disconnected" }
  /** Something changed in the Pulse home while no daemon was reachable. */
  | { type: "home_changed" }
  /** The user asked to start the daemon. */
  | { type: "start" }
  /** The user asked to restart a running daemon with the app's own binary. */
  | { type: "replace" }
  /** The user asked to stop the daemon, then quit the app or keep it open. */
  | { type: "stop"; then: "quit" | "stay" }
  | { type: "stopped" }
  | { type: "stop_failed"; error: string }
  /** The user quit the app. The daemon is left alone. */
  | { type: "quit" };

export type Effect =
  | { type: "locate" }
  | { type: "spawn" }
  | { type: "stop_daemon" }
  | { type: "watch_home"; on: boolean }
  | { type: "alert"; alert: "daemon_lost" | "start_failed" | "stop_failed"; detail: string | null }
  | { type: "exit" };

export interface Step {
  state: LifecycleState;
  effects: Effect[];
}

export const INITIAL: LifecycleState = { kind: "locating", spawnIfMissing: true };
export const LAUNCH_EFFECTS: Effect[] = [{ type: "locate" }];

const stay = (state: LifecycleState): Step => ({ state, effects: [] });

export function transition(state: LifecycleState, event: LifecycleEvent): Step {
  switch (state.kind) {
    case "locating":
      switch (event.type) {
        case "found":
          return stay({ kind: "connected", daemon: { info: event.info, owned: false } });
        case "missing":
          if (state.spawnIfMissing) return { state: { kind: "starting" }, effects: [{ type: "spawn" }] };
          return {
            state: { kind: "unreachable", reason: "lost", detail: null },
            effects: [
              { type: "watch_home", on: true },
              { type: "alert", alert: "daemon_lost", detail: null },
            ],
          };
        case "quit":
          return { state: { kind: "quitting" }, effects: [{ type: "exit" }] };
        default:
          return stay(state);
      }

    case "starting":
      switch (event.type) {
        case "started":
          return stay({ kind: "connected", daemon: { info: event.info, owned: true } });
        case "start_failed":
          return {
            state: { kind: "unreachable", reason: "start_failed", detail: event.error },
            effects: [
              { type: "watch_home", on: true },
              { type: "alert", alert: "start_failed", detail: event.error },
            ],
          };
        case "quit":
          // A daemon being started keeps starting: it runs in its own process group.
          return { state: { kind: "quitting" }, effects: [{ type: "exit" }] };
        default:
          return stay(state);
      }

    case "connected":
      switch (event.type) {
        case "disconnected":
          // It may have been restarted on another port; look before saying it is gone.
          return {
            state: { kind: "locating", spawnIfMissing: false },
            effects: [{ type: "locate" }],
          };
        case "replace":
          return {
            state: { kind: "stopping", daemon: state.daemon, then: "start" },
            effects: [{ type: "stop_daemon" }],
          };
        case "stop":
          return {
            state: { kind: "stopping", daemon: state.daemon, then: event.then },
            effects: [{ type: "stop_daemon" }],
          };
        case "quit":
          return { state: { kind: "quitting" }, effects: [{ type: "exit" }] };
        default:
          return stay(state);
      }

    case "stopping":
      switch (event.type) {
        case "stopped":
          switch (state.then) {
            case "quit":
              return { state: { kind: "quitting" }, effects: [{ type: "exit" }] };
            case "start":
              return { state: { kind: "starting" }, effects: [{ type: "spawn" }] };
            case "stay":
              return {
                state: { kind: "unreachable", reason: "stopped", detail: null },
                effects: [{ type: "watch_home", on: true }],
              };
          }
          break;
        case "stop_failed":
          return {
            state: { kind: "locating", spawnIfMissing: false },
            effects: [
              { type: "alert", alert: "stop_failed", detail: event.error },
              { type: "locate" },
            ],
          };
        case "quit":
        case "stop":
          // Finish stopping, then quit.
          return stay(
            event.type === "stop" && event.then === "stay" ? state : { ...state, then: "quit" },
          );
        default:
          // `disconnected` is expected here.
          return stay(state);
      }
      break;

    case "unreachable":
      switch (event.type) {
        case "home_changed":
          return { state, effects: [{ type: "locate" }] };
        case "found":
          return {
            state: { kind: "connected", daemon: { info: event.info, owned: false } },
            effects: [{ type: "watch_home", on: false }],
          };
        case "start":
          return {
            state: { kind: "starting" },
            effects: [{ type: "watch_home", on: false }, { type: "spawn" }],
          };
        case "quit":
          return {
            state: { kind: "quitting" },
            effects: [{ type: "watch_home", on: false }, { type: "exit" }],
          };
        case "stop":
          // Nothing to stop.
          return event.then === "quit"
            ? { state: { kind: "quitting" }, effects: [{ type: "watch_home", on: false }, { type: "exit" }] }
            : stay(state);
        default:
          return stay(state);
      }

    case "quitting":
      return stay(state);
  }
  return stay(state);
}

/** The daemon the shell is talking to, if any. */
export function currentDaemon(state: LifecycleState): Daemon | null {
  return state.kind === "connected" || state.kind === "stopping" ? state.daemon : null;
}
