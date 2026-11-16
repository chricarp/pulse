// The shell's own preferences, in `<userData>/settings.json`. Nothing here is Pulse state: the
// daemon knows nothing about windows or notifications of the shell.

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type CloseToTray = "auto" | "always" | "never";

export interface Settings {
  window: (Rect & { maximized: boolean }) | null;
  notifications: boolean;
  /** What closing the window does. `auto`: hide to the tray where one is known to show. */
  closeToTray: CloseToTray;
}

export const DEFAULT_SETTINGS: Settings = { window: null, notifications: true, closeToTray: "auto" };
export const DEFAULT_SIZE = { width: 1100, height: 760 };
export const MIN_SIZE = { width: 480, height: 360 };

const finite = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

/** Settings from whatever the file held; anything invalid falls back to its default. */
export function parseSettings(raw: unknown): Settings {
  if (typeof raw !== "object" || raw === null) return DEFAULT_SETTINGS;
  const r = raw as Record<string, unknown>;
  const w = r.window as Record<string, unknown> | null | undefined;
  const window =
    w && finite(w.x) && finite(w.y) && finite(w.width) && finite(w.height)
      ? {
          x: Math.round(w.x),
          y: Math.round(w.y),
          width: Math.round(w.width),
          height: Math.round(w.height),
          maximized: w.maximized === true,
        }
      : null;
  const closeToTray: CloseToTray =
    r.closeToTray === "always" || r.closeToTray === "never" ? r.closeToTray : "auto";
  return {
    window,
    notifications: typeof r.notifications === "boolean" ? r.notifications : true,
    closeToTray,
  };
}

function overlap(a: Rect, b: Rect): { width: number; height: number } {
  return {
    width: Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x),
    height: Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y),
  };
}

/**
 * Where to put the window: the saved place if enough of it is on a display that still exists
 * (monitors get unplugged), shrunk to fit that display; otherwise centred on the primary one.
 */
export function fitBounds(saved: Rect | null, workAreas: readonly Rect[], primary: Rect): Rect {
  if (saved !== null) {
    for (const area of workAreas) {
      const o = overlap(saved, area);
      if (o.width >= 120 && o.height >= 80) {
        const width = Math.max(MIN_SIZE.width, Math.min(saved.width, area.width));
        const height = Math.max(MIN_SIZE.height, Math.min(saved.height, area.height));
        const x = Math.min(Math.max(saved.x, area.x), area.x + area.width - width);
        const y = Math.min(Math.max(saved.y, area.y), area.y + area.height - height);
        return { x, y, width, height };
      }
    }
  }
  const width = Math.min(DEFAULT_SIZE.width, primary.width);
  const height = Math.min(DEFAULT_SIZE.height, primary.height);
  return {
    x: primary.x + Math.round((primary.width - width) / 2),
    y: primary.y + Math.round((primary.height - height) / 2),
    width,
    height,
  };
}

// Linux desktops whose panels show StatusNotifierItem/AppIndicator icons out of the box. Plain
// GNOME does not (it needs an extension); Ubuntu's GNOME session ships one.
const TRAY_DESKTOPS = [
  "kde",
  "xfce",
  "cinnamon",
  "x-cinnamon",
  "mate",
  "lxqt",
  "lxde",
  "budgie",
  "budgie-desktop",
  "unity",
  "ubuntu",
  "pantheon",
  "deepin",
  "enlightenment",
  "cosmic",
];

/**
 * Whether closing the window should hide it to the tray. Electron cannot tell whether a Linux
 * tray icon is actually visible, and hiding the only window behind an icon nobody can see would
 * strand an invisible process. So on Linux the window hides only on desktops known to show tray
 * icons, unless the user chose otherwise; where it does not hide, closing quits the app (the
 * daemon keeps running).
 */
export function closeHidesToTray(
  platform: NodeJS.Platform,
  setting: CloseToTray,
  xdgCurrentDesktop: string | undefined,
): boolean {
  if (setting !== "auto") return setting === "always";
  if (platform === "darwin") return true;
  if (platform !== "linux") return true;
  const desktops = (xdgCurrentDesktop ?? "").toLowerCase().split(":");
  return desktops.some((d) => TRAY_DESKTOPS.includes(d.trim()));
}
