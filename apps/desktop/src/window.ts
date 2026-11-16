// The window and the security policy around everything web in the app. The renderer shows
// untrusted content (file names, commit messages, run output), so it gets no Node, a sandbox, a
// one-function bridge, and may only ever be on the daemon's origin.

import { stat } from "node:fs/promises";
import { isAbsolute } from "node:path";

import { BrowserWindow, app, ipcMain, nativeTheme, screen, session, shell, type WebContents } from "electron";

import { REVEAL_PATH_CHANNEL } from "./bridge.ts";
import { MIN_SIZE, type Rect, type Settings, fitBounds } from "./settings.ts";
import { externalUrl, isDaemonUrl, redact } from "./urls.ts";

export type Log = (msg: string) => void;

// Permissions the UI may use, and only from the daemon's origin. Everything else (camera,
// microphone, notifications from the page, geolocation, clipboard reads, MIDI, HID, …) is denied.
const ALLOWED_PERMISSIONS = new Set(["clipboard-sanitized-write"]);

/** Sends a link to the system browser if it is safe to; otherwise drops it. */
function openOutside(url: string, log: Log): void {
  const safe = externalUrl(url);
  if (safe === null) {
    log(`refused to open ${redact(url)} outside the app`);
    return;
  }
  void shell.openExternal(safe).catch((e: Error) => log(`could not open a link: ${e.message}`));
}

/**
 * Applies the navigation policy to every web contents the app creates (the window, and anything
 * a page might try to open). Call once, before any window exists.
 */
export function guardWebContents(getOrigin: () => string | null, log: Log, onOpenInWindow: (url: string) => void): void {
  app.on("web-contents-created", (_e, contents: WebContents) => {
    const keepOnDaemon = (event: { preventDefault(): void }, url: string) => {
      if (isDaemonUrl(url, getOrigin())) return;
      event.preventDefault();
      log(`blocked navigation to ${redact(url)}`);
      openOutside(url, log);
    };
    contents.on("will-navigate", keepOnDaemon);
    contents.on("will-redirect", keepOnDaemon);
    contents.on("will-frame-navigate", (details) => {
      if (details.isMainFrame) return; // will-navigate covers it.
      if (!isDaemonUrl(details.url, getOrigin())) {
        details.preventDefault();
        log(`blocked a frame navigation to ${redact(details.url)}`);
      }
    });
    contents.setWindowOpenHandler(({ url }) => {
      // No second window, ever. A link to the UI opens in the window; a web link in the browser.
      if (isDaemonUrl(url, getOrigin())) onOpenInWindow(url);
      else openOutside(url, log);
      log(`denied a new window for ${redact(url)}`);
      return { action: "deny" };
    });
    contents.on("will-attach-webview", (event) => {
      event.preventDefault();
      log("blocked a <webview>");
    });
  });
}

/** Session-wide policy: permissions and downloads. Call after `app` is ready. */
export function guardSession(getOrigin: () => string | null, log: Log): void {
  const ses = session.defaultSession;
  const allowed = (permission: string, origin: string | undefined) =>
    ALLOWED_PERMISSIONS.has(permission) && origin !== undefined && isDaemonUrl(origin, getOrigin());
  ses.setPermissionRequestHandler((_wc, permission, callback, details) => {
    const ok = allowed(permission, details.requestingUrl);
    if (!ok) log(`denied the permission request "${permission}"`);
    callback(ok);
  });
  ses.setPermissionCheckHandler((_wc, permission, requestingOrigin) => allowed(permission, requestingOrigin));
  ses.setDevicePermissionHandler(() => false);
  ses.on("will-download", (event) => {
    event.preventDefault();
    log("blocked a download");
  });
}

/**
 * The renderer's one shell-only feature: show a file in the file manager. Only the daemon's own
 * page may ask, only for an absolute path that exists, and the file is revealed, never opened.
 */
export function handleBridge(getOrigin: () => string | null, log: Log): void {
  ipcMain.handle(REVEAL_PATH_CHANNEL, async (event, path: unknown) => {
    const from = event.senderFrame?.url ?? "";
    if (!isDaemonUrl(from, getOrigin())) {
      log("refused revealPath from a page that is not the daemon's");
      return false;
    }
    if (typeof path !== "string" || path.length > 4096 || path.includes("\0") || !isAbsolute(path)) return false;
    try {
      await stat(path);
    } catch {
      return false;
    }
    shell.showItemInFolder(path);
    return true;
  });
}

export interface WindowOptions {
  settings: Settings;
  preload: string;
  icon: string | null;
  devTools: boolean;
}

export function createWindow(opts: WindowOptions): BrowserWindow {
  const display = screen.getPrimaryDisplay();
  const saved = opts.settings.window;
  const bounds = fitBounds(
    saved,
    screen.getAllDisplays().map((d) => d.workArea),
    display.workArea,
  );
  const win = new BrowserWindow({
    ...bounds,
    minWidth: MIN_SIZE.width,
    minHeight: MIN_SIZE.height,
    show: false,
    title: "Pulse",
    backgroundColor: nativeTheme.shouldUseDarkColors ? "#141217" : "#fbfafc",
    ...(opts.icon !== null && process.platform === "linux" ? { icon: opts.icon } : {}),
    webPreferences: {
      preload: opts.preload,
      contextIsolation: true,
      nodeIntegration: false,
      nodeIntegrationInWorker: false,
      nodeIntegrationInSubFrames: false,
      sandbox: true,
      webSecurity: true,
      allowRunningInsecureContent: false,
      webviewTag: false,
      navigateOnDragDrop: false,
      // Chromium's spellchecker downloads dictionaries from Google on Linux.
      spellcheck: false,
      devTools: opts.devTools,
      safeDialogs: true,
    },
  });
  if (saved?.maximized) win.maximize();
  return win;
}

/** Where the window is now, for the settings file. */
export function windowPlacement(win: BrowserWindow): Settings["window"] {
  const b: Rect = win.getNormalBounds();
  return { ...b, maximized: win.isMaximized() };
}
