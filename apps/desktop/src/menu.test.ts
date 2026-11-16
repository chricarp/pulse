import { describe, expect, it } from "vitest";

import { APPROVALS_MAX, EMPTY_ACTIVITY, RECENT_MAX, approvalsWaiting, withRuns, withStatus } from "./activity.ts";
import type { RunSummary, SystemStatus } from "./contracts.ts";
import type { LifecycleState } from "./lifecycle.ts";
import { type MenuInput, type MenuItem, trayModel } from "./menu.ts";
import { DEFAULT_SETTINGS } from "./settings.ts";

const status = (over: Partial<SystemStatus> = {}): SystemStatus => ({
  version: "0.1.0",
  started_at: "2026-11-16T09:00:00Z",
  paused: false,
  pulse_home: "/tmp/h",
  user_home: "/tmp",
  port: 7477,
  sources: { healthy: 1, degraded: 0, failed: 0, other: 0 },
  runs: { running: 0, queued: 0, pending_approval: 0 },
  rules: 2,
  agents: 1,
  config_errors: 0,
  ...over,
});

let seq = 0;
const run = (status: RunSummary["status"], rule = "r", minute = seq++): RunSummary => ({
  id: `run${minute}`,
  rule,
  agent: "a",
  event: "e",
  status,
  created_at: `2026-11-16T10:${String(minute).padStart(2, "0")}:00.000Z`,
  chain_depth: 1,
});

const connected: LifecycleState = {
  kind: "connected",
  daemon: { info: { pid: 1, port: 7477, version: "0.1.0", started_at: "x" }, owned: false },
};

const input = (over: Partial<MenuInput> = {}): MenuInput => ({
  lifecycle: connected,
  activity: withStatus(EMPTY_ACTIVITY, status()),
  settings: DEFAULT_SETTINGS,
  platform: "linux",
  advice: { kind: "same" },
  closeHides: true,
  sandboxOff: false,
  ...over,
});

/** The menu as lines: labels, `[x]`/`[ ]` checks, `(off)` for disabled, `>` for submenus. */
function lines(items: MenuItem[], indent = ""): string[] {
  return items.flatMap((i) => {
    switch (i.kind) {
      case "separator":
        return [`${indent}---`];
      case "label":
        return [`${indent}${i.label}`];
      case "action":
        return [`${indent}${i.label}${i.enabled ? "" : " (off)"}`];
      case "check":
        return [`${indent}[${i.checked ? "x" : " "}] ${i.label}`];
      case "submenu":
        return [`${indent}${i.label} >`, ...lines(i.items, `${indent}  `)];
    }
  });
}

describe("trayModel", () => {
  it("running, with approvals waiting and recent runs", () => {
    const activity = withRuns(withStatus(EMPTY_ACTIVITY, status({ runs: { running: 1, queued: 0, pending_approval: 7 } })), [
      run("succeeded", "lint", 1),
      run("pending_approval", "deploy", 2),
      run("failed", "summarize & tag", 3),
    ]);
    const m = trayModel(input({ activity }));
    expect(lines(m.items)).toEqual([
      "Pulse: Running · 1 run in progress",
      "---",
      "Open Pulse",
      "---",
      "Waiting for approval (7)",
      "deploy · a",
      "All approvals…",
      "Recent runs >",
      "  Failed: summarize && tag · a",
      "  Waiting: deploy · a",
      "  Done: lint · a",
      "---",
      "Pause everything",
      "---",
      "[x] Notifications",
      "[x] Keep running in the tray when the window closes",
      "Install command line tool…",
      "---",
      "Quit Pulse",
      "Quit Pulse and stop the daemon",
    ]);
    expect(m).toMatchObject({ badge: "7", dim: false, tooltip: "Pulse: Running · 1 run in progress · 7 waiting for approval" });
  });

  it("clicking a run opens it; pause and quit carry their actions", () => {
    const activity = withRuns(withStatus(EMPTY_ACTIVITY, status()), [run("failed", "x", 5)]);
    const m = trayModel(input({ activity }));
    const recent = m.items.find((i) => i.kind === "submenu");
    expect(recent?.kind === "submenu" && recent.items[0]).toMatchObject({ action: { type: "open", route: { kind: "run", id: "run5" } } });
    const actions = m.items.flatMap((i) => (i.kind === "action" ? [i.action.type] : []));
    expect(actions).toEqual(["open", "pause", "install_cli", "quit", "quit_and_stop"]);
  });

  it("paused: resume instead of pause, dim icon", () => {
    const m = trayModel(input({ activity: withStatus(EMPTY_ACTIVITY, status({ paused: true })) }));
    expect(lines(m.items)).toContain("Resume");
    expect(lines(m.items)).not.toContain("Pause everything");
    expect(lines(m.items)[0]).toBe("Pulse: Paused: nothing fires");
    expect(m.dim).toBe(true);
  });

  it("daemon unreachable: say why, offer to start it, nothing that needs a daemon", () => {
    const m = trayModel(
      input({
        lifecycle: { kind: "unreachable", reason: "start_failed", detail: "cannot listen on 127.0.0.1:7477\nin use" },
        activity: EMPTY_ACTIVITY,
        platform: "darwin",
      }),
    );
    expect(lines(m.items)).toEqual([
      "Pulse: The daemon could not start",
      "cannot listen on 127.0.0.1:7477 in use",
      "---",
      "Open Pulse",
      "---",
      "Pause everything (off)",
      "Start the daemon",
      "---",
      "[x] Notifications",
      "Install command line tool…",
      "---",
      "Quit Pulse",
      "Quit Pulse and stop the daemon (off)",
    ]);
    expect(m).toMatchObject({ dim: true, badge: "" });
  });

  it("version skew", () => {
    const older = trayModel(input({ advice: { kind: "daemon_older", daemon: "0.1.0", app: "0.2.0" } }));
    expect(lines(older.items)).toContain("Restart the daemon with version 0.2.0");
    const newer = trayModel(input({ advice: { kind: "daemon_newer", daemon: "0.3.0", app: "0.2.0" } }));
    expect(lines(newer.items)[1]).toBe("The daemon (0.3.0) is newer than this app (0.2.0)");
    expect(lines(newer.items).some((l) => l.startsWith("Restart"))).toBe(false);
  });

  it("says when the Chromium sandbox is off", () => {
    expect(lines(trayModel(input({ sandboxOff: true })).items)[1]).toBe("Running without the Chromium sandbox (--no-sandbox)");
  });

  it("starting and stopping", () => {
    expect(lines(trayModel(input({ lifecycle: { kind: "starting" }, activity: EMPTY_ACTIVITY })).items)[0]).toBe("Pulse: Starting the Pulse daemon…");
    const stoppingDaemon = { info: { pid: 1, port: 1, version: "0.1.0", started_at: "x" }, owned: true };
    const m = trayModel(input({ lifecycle: { kind: "stopping", daemon: stoppingDaemon, then: "quit" } }));
    expect(lines(m.items)).toContain("Pause everything (off)");
  });
});

describe("activity", () => {
  it("keeps a bounded, newest-first list and drops approvals once decided", () => {
    let a = withStatus(EMPTY_ACTIVITY, status({ runs: { running: 0, queued: 0, pending_approval: 1 } }));
    const many = Array.from({ length: 20 }, (_, i) => run(i % 2 ? "pending_approval" : "succeeded", "r", 10 + i));
    a = withRuns(a, many);
    expect(a.recent).toHaveLength(RECENT_MAX);
    expect(a.recent[0]?.id).toBe("run29");
    expect(a.approvals).toHaveLength(APPROVALS_MAX);
    // Oldest waiting first.
    expect(a.approvals[0]?.id).toBe("run11");
    expect(approvalsWaiting(a)).toBe(APPROVALS_MAX);
    a = withRuns(a, [{ ...many[1]!, status: "queued" }]);
    expect(a.approvals.map((r) => r.id)).not.toContain("run11");
  });

  it("ignores runs it cannot read", () => {
    expect(withRuns(EMPTY_ACTIVITY, [{ id: "x" }, 3, null])).toEqual(EMPTY_ACTIVITY);
  });
});
