// The Electron main process: decide the Pulse home and data directory, take the single-instance
// lock, apply the security policy, then hand over to the shell.

import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import { app, dialog } from "electron";

import { bundledBinary, pulseHome, userDataFor } from "./paths.ts";
import { Shell } from "./shell.ts";
import { guardSession, guardWebContents, handleBridge } from "./window.ts";

export interface Boot {
  shell: Shell;
  home: string;
}

const log = (msg: string) => {
  // Never given a token or a URL fragment: see `redact`.
  process.stderr.write(`[pulse-desktop] ${msg}\n`);
};

/**
 * Starts the app. Resolves with the shell once it runs, or `null` when another instance already
 * runs for this Pulse home (that one is focused and this one quits).
 */
export function boot(): Promise<Boot | null> {
  app.setName("Lictory Pulse");
  app.enableSandbox();
  const home = pulseHome(process.env, homedir(), process.cwd());
  const userData = userDataFor(app.getPath("userData"), home, homedir());
  app.setPath("userData", userData);

  if (!app.requestSingleInstanceLock({ home })) {
    log("Lictory Pulse is already open for this Pulse home; focusing it");
    app.quit();
    return Promise.resolve(null);
  }

  const packaged = app.isPackaged;
  // In development the bundle is in apps/desktop/dist (or dist-e2e); icons are in ../build.
  const desktopDir = join(__dirname, "..");
  const resources = packaged ? process.resourcesPath : join(desktopDir, "build");
  const shell = new Shell({
    home,
    binary: bundledBinary({
      env: process.env,
      packaged,
      resourcesPath: process.resourcesPath,
      appPath: desktopDir,
      exists: existsSync,
    }),
    appVersion: app.getVersion(),
    userData,
    platform: process.platform,
    preload: join(__dirname, "preload.cjs"),
    windowIcon: join(resources, "icons", "icon.png"),
    trayIconDir: join(resources, "icons"),
    startHidden: process.argv.includes("--hidden"),
    // AppImage launchers add it on argv, where `app.commandLine` does not see it.
    sandboxOff: process.argv.includes("--no-sandbox") || app.commandLine.hasSwitch("no-sandbox"),
    devTools: !packaged || process.env.PULSE_DESKTOP_DEVTOOLS === "1",
    env: process.env,
    log,
  });

  guardWebContents(() => shell.origin(), log, (url) => shell.openInWindow(url));

  app.on("second-instance", () => {
    log("a second instance was started; showing this one");
    void shell.openRoute({ kind: "home" });
  });
  // macOS: clicking the Dock icon.
  app.on("activate", () => void shell.openRoute({ kind: "home" }));
  // Every way of quitting (menu, Cmd+Q, logout) goes through the lifecycle, which may first need
  // to finish stopping the daemon.
  app.on("before-quit", (e) => {
    if (shell.quitting) return;
    e.preventDefault();
    void shell.dispatch({ type: "quit" });
  });
  // Closing the last window is handled by the shell (hide to tray, or quit).
  app.on("window-all-closed", () => {});

  return app.whenReady().then(async () => {
    guardSession(() => shell.origin(), log);
    handleBridge(() => shell.origin(), log);
    try {
      await shell.start();
    } catch (e) {
      dialog.showErrorBox("Lictory Pulse could not start", (e as Error).message);
      app.exit(1);
    }
    log(`home ${home}, data ${userData}, daemon binary ${shell.config.binary ?? "(none)"}`);
    if (shell.config.sandboxOff) log("running without the Chromium sandbox (--no-sandbox)");
    return { shell, home };
  });
}
