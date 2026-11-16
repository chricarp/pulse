// The tray menu (and the window's Pulse menu, which is the same list) as data, built from the
// daemon's state. `tray.ts` turns it into Electron menus; tests read it directly.

import { type Activity, approvalsWaiting } from "./activity.ts";
import type { RunStatus, RunSummary } from "./contracts.ts";
import type { LifecycleState } from "./lifecycle.ts";
import type { Settings } from "./settings.ts";
import { menuLabel } from "./text.ts";
import type { Route } from "./urls.ts";
import type { VersionAdvice } from "./version.ts";

export type Action =
  | { type: "open"; route: Route }
  | { type: "pause" }
  | { type: "resume" }
  | { type: "start_daemon" }
  | { type: "replace_daemon" }
  | { type: "toggle_notifications" }
  | { type: "toggle_close_to_tray" }
  | { type: "install_cli" }
  | { type: "quit" }
  | { type: "quit_and_stop" };

export type MenuItem =
  | { kind: "label"; label: string }
  | { kind: "action"; label: string; action: Action; enabled: boolean }
  | { kind: "check"; label: string; action: Action; checked: boolean }
  | { kind: "separator" }
  | { kind: "submenu"; label: string; items: MenuItem[] };

export interface MenuInput {
  lifecycle: LifecycleState;
  activity: Activity;
  settings: Settings;
  platform: NodeJS.Platform;
  advice: VersionAdvice | null;
  /** Whether closing the window currently hides it (see `closeHidesToTray`). */
  closeHides: boolean;
  /** Chromium runs with `--no-sandbox` (the AppImage launcher adds it where user namespaces
   * are not available). Said in the menu: it weakens the isolation of the window. */
  sandboxOff: boolean;
}

export interface TrayModel {
  items: MenuItem[];
  tooltip: string;
  /** Text next to the menu bar icon on macOS: the number of approvals waiting, or nothing. */
  badge: string;
  /** Show the dimmed icon: paused, or no daemon. */
  dim: boolean;
}

const STATUS_WORD: Record<RunStatus, string> = {
  pending_approval: "Waiting",
  queued: "Queued",
  running: "Running",
  succeeded: "Done",
  failed: "Failed",
  timed_out: "Timed out",
  cancelled: "Cancelled",
  rejected: "Rejected",
  interrupted: "Interrupted",
};

export function statusLine(lifecycle: LifecycleState, activity: Activity): string {
  switch (lifecycle.kind) {
    case "locating":
      return "Looking for the Pulse daemon…";
    case "starting":
      return "Starting the Pulse daemon…";
    case "stopping":
      return "Stopping the Pulse daemon…";
    case "quitting":
      return "Quitting…";
    case "unreachable":
      return lifecycle.reason === "start_failed"
        ? "The daemon could not start"
        : lifecycle.reason === "stopped"
          ? "The daemon is stopped"
          : "The daemon is not running";
    case "connected": {
      const s = activity.status;
      if (s?.paused) return "Paused: nothing fires";
      const running = s?.runs.running ?? 0;
      return running > 0 ? `Running · ${running} run${running === 1 ? "" : "s"} in progress` : "Running";
    }
  }
}

function runLabel(run: RunSummary, withStatus: boolean): string {
  const name = `${menuLabel(run.rule ?? "rerun", 32)} · ${menuLabel(run.agent, 20)}`;
  return withStatus ? `${STATUS_WORD[run.status]}: ${name}` : name;
}

const open = (route: Route): Action => ({ type: "open", route });
const action = (label: string, a: Action, enabled = true): MenuItem => ({
  kind: "action",
  label,
  action: a,
  enabled,
});

export function trayModel(input: MenuInput): TrayModel {
  const { lifecycle, activity, settings, advice } = input;
  const connected = lifecycle.kind === "connected";
  const paused = connected && activity.status?.paused === true;
  const waiting = connected ? approvalsWaiting(activity) : 0;
  const status = statusLine(lifecycle, activity);

  const items: MenuItem[] = [{ kind: "label", label: `Pulse: ${status}` }];
  if (connected && advice?.kind === "daemon_newer") {
    items.push({ kind: "label", label: `The daemon (${advice.daemon}) is newer than this app (${advice.app})` });
  }
  if (input.sandboxOff) items.push({ kind: "label", label: "Running without the Chromium sandbox (--no-sandbox)" });
  if (lifecycle.kind === "unreachable" && lifecycle.detail) {
    items.push({ kind: "label", label: menuLabel(lifecycle.detail, 60) });
  }
  items.push({ kind: "separator" }, action("Open Pulse", open({ kind: "home" })));

  if (connected) {
    items.push({ kind: "separator" });
    if (waiting > 0) {
      items.push({ kind: "label", label: `Waiting for approval (${waiting})` });
      for (const run of activity.approvals) {
        items.push(action(runLabel(run, false), open({ kind: "run", id: run.id })));
      }
      if (waiting > activity.approvals.length) {
        items.push(action("All approvals…", open({ kind: "approvals" })));
      }
    }
    items.push({
      kind: "submenu",
      label: "Recent runs",
      items:
        activity.recent.length === 0
          ? [{ kind: "label", label: "No runs yet" }]
          : activity.recent.map((run) => action(runLabel(run, true), open({ kind: "run", id: run.id }))),
    });
  }

  items.push({ kind: "separator" });
  if (paused) items.push(action("Resume", { type: "resume" }));
  else items.push(action("Pause everything", { type: "pause" }, connected));
  if (lifecycle.kind === "unreachable") items.push(action("Start the daemon", { type: "start_daemon" }));
  if (connected && advice?.kind === "daemon_older") {
    items.push(action(`Restart the daemon with version ${advice.app}`, { type: "replace_daemon" }));
  }

  items.push(
    { kind: "separator" },
    { kind: "check", label: "Notifications", action: { type: "toggle_notifications" }, checked: settings.notifications },
  );
  if (input.platform === "linux") {
    items.push({
      kind: "check",
      label: "Keep running in the tray when the window closes",
      action: { type: "toggle_close_to_tray" },
      checked: input.closeHides,
    });
  }
  items.push(
    action("Install command line tool…", { type: "install_cli" }),
    { kind: "separator" },
    action("Quit Pulse", { type: "quit" }),
    action("Quit Pulse and stop the daemon", { type: "quit_and_stop" }, connected),
  );

  const tooltipParts = [`Pulse: ${status}`];
  if (waiting > 0) tooltipParts.push(`${waiting} waiting for approval`);
  return {
    items,
    tooltip: tooltipParts.join(" · "),
    badge: waiting > 0 ? String(waiting) : "",
    dim: !connected || paused,
  };
}
