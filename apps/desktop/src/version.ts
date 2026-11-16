// The app and the daemon it finds can come from different releases: the user updated one and
// not the other, or started a daemon from another checkout. The window always shows the daemon's
// own UI, so it matches the daemon; what can drift is the shell's tray and notifications.

export type VersionAdvice =
  | { kind: "same" }
  /** The daemon is older: offer to restart it with the app's binary. Never done unasked. */
  | { kind: "daemon_older"; daemon: string; app: string }
  /** The daemon is newer. Replacing it with an older binary could meet a newer database, so
   * the shell only says so. */
  | { kind: "daemon_newer"; daemon: string; app: string }
  | { kind: "unknown"; daemon: string; app: string };

/** `[major, minor, patch]` from `1.2.3`, `pulse 1.2.3` or `1.2.3-beta.1` (pre-release ignored). */
export function parseVersion(v: string): [number, number, number] | null {
  const m = /(\d+)\.(\d+)\.(\d+)/.exec(v);
  if (m === null) return null;
  return [Number(m[1]), Number(m[2]), Number(m[3])];
}

export function compareVersions(a: string, b: string): -1 | 0 | 1 | null {
  const x = parseVersion(a);
  const y = parseVersion(b);
  if (x === null || y === null) return null;
  for (let i = 0; i < 3; i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d < 0 ? -1 : 1;
  }
  return 0;
}

export function versionAdvice(app: string, daemon: string): VersionAdvice {
  switch (compareVersions(daemon, app)) {
    case 0:
      return { kind: "same" };
    case -1:
      return { kind: "daemon_older", daemon, app };
    case 1:
      return { kind: "daemon_newer", daemon, app };
    default:
      return { kind: "unknown", daemon, app };
  }
}
