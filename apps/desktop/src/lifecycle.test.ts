import { describe, expect, it } from "vitest";

import type { DaemonInfo } from "./contracts.ts";
import { type Effect, INITIAL, type LifecycleEvent, type LifecycleState, transition } from "./lifecycle.ts";

const info: DaemonInfo = { pid: 10, port: 7477, version: "0.1.0", started_at: "2026-11-16T00:00:00Z" };
const connected = (owned = false): LifecycleState => ({ kind: "connected", daemon: { info, owned } });
const stopping = (then: "quit" | "stay" | "start"): LifecycleState => ({ kind: "stopping", daemon: { info, owned: false }, then });
const unreachable: LifecycleState = { kind: "unreachable", reason: "lost", detail: null };

const types = (effects: Effect[]) => effects.map((e) => (e.type === "watch_home" ? `watch_home:${e.on}` : e.type === "alert" ? `alert:${e.alert}` : e.type));

describe("transition", () => {
  const cases: [string, LifecycleState, LifecycleEvent, string, string[]][] = [
    // Launch: use a daemon that answers; start one only if there is none (or the file is stale).
    ["running daemon is used", INITIAL, { type: "found", info }, "connected", []],
    ["no daemon: start one", INITIAL, { type: "missing" }, "starting", ["spawn"]],
    ["started", { kind: "starting" }, { type: "started", info }, "connected", []],
    ["start failed: say so, wait for one", { kind: "starting" }, { type: "start_failed", error: "port in use" }, "unreachable", ["watch_home:true", "alert:start_failed"]],

    // The daemon dies while the app is open: look once, never restart it unasked.
    ["connection lost: look again", connected(), { type: "disconnected" }, "locating", ["locate"]],
    ["restarted elsewhere: reconnect", { kind: "locating", spawnIfMissing: false }, { type: "found", info }, "connected", []],
    ["gone: tell, do not respawn", { kind: "locating", spawnIfMissing: false }, { type: "missing" }, "unreachable", ["watch_home:true", "alert:daemon_lost"]],
    ["home changed: look", unreachable, { type: "home_changed" }, "unreachable", ["locate"]],
    ["started from the CLI: pick it up", unreachable, { type: "found", info }, "connected", ["watch_home:false"]],
    ["still nothing: stay quiet", unreachable, { type: "missing" }, "unreachable", []],
    ["user starts it", unreachable, { type: "start" }, "starting", ["watch_home:false", "spawn"]],

    // Quitting leaves the daemon alone, whoever started it.
    ["quit leaves an adopted daemon", connected(false), { type: "quit" }, "quitting", ["exit"]],
    ["quit leaves an owned daemon", connected(true), { type: "quit" }, "quitting", ["exit"]],
    ["quit while starting", { kind: "starting" }, { type: "quit" }, "quitting", ["exit"]],
    ["quit while unreachable", unreachable, { type: "quit" }, "quitting", ["watch_home:false", "exit"]],

    // Stopping only on request.
    ["quit and stop", connected(), { type: "stop", then: "quit" }, "stopping", ["stop_daemon"]],
    ["stopped, then quit", stopping("quit"), { type: "stopped" }, "quitting", ["exit"]],
    ["stopped, stay open", stopping("stay"), { type: "stopped" }, "unreachable", ["watch_home:true"]],
    ["the expected disconnect", stopping("quit"), { type: "disconnected" }, "stopping", []],
    ["quit during a stop waits for it", stopping("stay"), { type: "quit" }, "stopping", []],
    ["stop failed: report, look again", stopping("quit"), { type: "stop_failed", error: "timeout" }, "locating", ["alert:stop_failed", "locate"]],
    ["nothing to stop: just quit", unreachable, { type: "stop", then: "quit" }, "quitting", ["watch_home:false", "exit"]],

    // Replacing an older daemon is a stop then a start.
    ["replace", connected(), { type: "replace" }, "stopping", ["stop_daemon"]],
    ["replace: start after the stop", stopping("start"), { type: "stopped" }, "starting", ["spawn"]],

    // Stale or late results change nothing.
    ["a late locate result", connected(), { type: "found", info }, "connected", []],
    ["start while running", connected(), { type: "start" }, "connected", []],
    ["after quitting", { kind: "quitting" }, { type: "found", info }, "quitting", []],
  ];

  it.each(cases)("%s", (_name, state, event, kind, effects) => {
    const step = transition(state, event);
    expect(step.state.kind).toBe(kind);
    expect(types(step.effects)).toEqual(effects);
  });

  it("records who started the daemon", () => {
    expect(transition({ kind: "starting" }, { type: "started", info }).state).toEqual(connected(true));
    expect(transition(INITIAL, { type: "found", info }).state).toEqual(connected(false));
  });

  it("quitting during a replace still quits after the stop", () => {
    const step = transition(stopping("start"), { type: "quit" });
    expect(step.state).toEqual(stopping("quit"));
  });
});
