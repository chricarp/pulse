// Wires the pieces: performs lifecycle effects, keeps the tray model current from daemon pushes,
// owns the window, and shows notifications. Holds no Pulse state the daemon does not have; what
// it keeps (recent runs, counts) is a bounded view of pushes.

import { execFile } from "node:child_process";
import { EventEmitter } from "node:events";
import { type FSWatcher, watch } from "node:fs";
import { chmod, copyFile, mkdir, readFile, readdir, rename, stat, unlink, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, join } from "node:path";

import { type BrowserWindow, Menu, Notification, app, dialog } from "electron";

import { type Activity, EMPTY_ACTIVITY, RECENT_MAX, withRuns, withStatus } from "./activity.ts";
import { installCli } from "./cli.ts";
import type { DaemonInfo, Notification as RpcNotification, RuleInfo } from "./contracts.ts";
import { type DaemonEnv, binaryVersion, locate, mintLoginCode, spawnDaemon, stopDaemon } from "./daemon.ts";
import {
  type Effect,
  INITIAL,
  LAUNCH_EFFECTS,
  type LifecycleEvent,
  type LifecycleState,
  currentDaemon,
  transition,
} from "./lifecycle.ts";
import { type Action, type TrayModel, trayModel } from "./menu.ts";
import { BreakerNotices, DEFAULT_THROTTLE, type Notice, RunNotices, Throttle } from "./notify.ts";
import { statusPage } from "./page.ts";
import { loginShellCommand, mergedPath, stableBinaryPath } from "./paths.ts";
import type { RpcClient } from "./rpc.ts";
import { DEFAULT_SETTINGS, type Settings, closeHidesToTray, parseSettings } from "./settings.ts";
import { notificationText, plainText } from "./text.ts";
import { TrayView, applicationMenu } from "./tray.ts";
import { type Route, daemonOrigin, isDaemonUrl, loginUrl, redact } from "./urls.ts";
import { type VersionAdvice, versionAdvice } from "./version.ts";
import { createWindow, windowPlacement } from "./window.ts";

export interface ShellConfig {
  home: string;
  /** The bundled (or development) `pulse` binary, if one was found. */
  binary: string | null;
  appVersion: string;
  userData: string;
  platform: NodeJS.Platform;
  preload: string;
  windowIcon: string | null;
  trayIconDir: string;
  startHidden: boolean;
  devTools: boolean;
  sandboxOff: boolean;
  env: NodeJS.ProcessEnv;
  log: (msg: string) => void;
}

/** What a notification looked like when shown; emitted as `notification` for tests and logs. */
export interface ShownNotice {
  kind: Notice["kind"];
  title: string;
  body: string;
  route: Route;
}

const KEEP_NOTIFICATIONS = 10;

export class Shell extends EventEmitter {
  state: LifecycleState = INITIAL;
  activity: Activity = EMPTY_ACTIVITY;
  settings: Settings = DEFAULT_SETTINGS;
  advice: VersionAdvice | null = null;
  window: BrowserWindow | null = null;
  /** What the window shows: the daemon's UI (on which origin), a status page, or nothing yet. */
  private showing: { kind: "ui"; origin: string } | { kind: "status" } | null = null;
  private client: RpcClient | null = null;
  private tray: TrayView | null = null;
  private homeWatcher: FSWatcher | null = null;
  private locating = false;
  private locateAgain = false;
  private refreshingRules = false;
  private refreshRulesAgain = false;
  private binaryVersionCache: string | null | undefined;
  private shellPath: Promise<string | undefined> | null = null;
  private readonly runNotices = new RunNotices();
  private readonly breakerNotices = new BreakerNotices();
  private readonly throttle: Throttle;
  private readonly shownNotifications: Notification[] = [];
  private lastMenuKey = "";
  private trayHintShown = false;
  private unsupportedLogged = false;
  private askedAbout = new Set<string>();
  quitting = false;

  constructor(readonly config: ShellConfig) {
    super();
    this.throttle = new Throttle(
      DEFAULT_THROTTLE,
      { now: () => Date.now(), setTimeout: (fn, ms) => setTimeout(fn, ms), clearTimeout: (h) => clearTimeout(h as NodeJS.Timeout) },
      (n) => void this.present(n),
    );
  }

  private log(msg: string): void {
    this.config.log(msg);
  }

  /** The daemon's origin while connected; the window may only show pages from it. */
  origin(): string | null {
    const d = currentDaemon(this.state);
    return d === null ? null : daemonOrigin(d.info.port);
  }

  private daemonEnv(env: NodeJS.ProcessEnv = this.config.env): DaemonEnv {
    return { home: this.config.home, binary: this.config.binary ?? "pulse", env, log: (m) => this.log(m) };
  }

  // ---- start and stop ----

  async start(): Promise<void> {
    this.settings = await this.loadSettings();
    this.throttle.setEnabled(this.settings.notifications);
    try {
      this.tray = new TrayView(this.config.trayIconDir, (a) => void this.dispatch(a));
    } catch (e) {
      this.log(`no tray: ${(e as Error).message}`);
    }
    if (!this.config.startHidden) this.ensureWindow(true);
    this.render();
    this.run(LAUNCH_EFFECTS);
  }

  send(event: LifecycleEvent): void {
    const before = this.state.kind;
    const step = transition(this.state, event);
    this.state = step.state;
    if (before !== this.state.kind) this.log(`daemon: ${before} → ${this.state.kind} (${event.type})`);
    this.emit("state", this.state);
    this.run(step.effects);
    this.render();
    this.syncWindow();
  }

  private run(effects: readonly Effect[]): void {
    for (const e of effects) {
      switch (e.type) {
        case "locate":
          void this.locate();
          break;
        case "spawn":
          void this.spawn();
          break;
        case "stop_daemon":
          void this.stopDaemon();
          break;
        case "watch_home":
          this.watchHome(e.on);
          break;
        case "alert":
          this.alert(e.alert, e.detail);
          break;
        case "exit":
          void this.exit();
          break;
      }
    }
  }

  private async locate(): Promise<void> {
    if (this.locating) {
      this.locateAgain = true;
      return;
    }
    this.locating = true;
    try {
      do {
        this.locateAgain = false;
        const found = await locate(this.daemonEnv());
        if (found === null) {
          this.send({ type: "missing" });
        } else if (this.state.kind === "locating" || this.state.kind === "unreachable") {
          this.attach(found.client, found.info);
          this.send({ type: "found", info: found.info });
          this.afterConnect(found.info, false);
        } else {
          found.client.close();
        }
      } while (this.locateAgain);
    } finally {
      this.locating = false;
    }
  }

  private async spawn(): Promise<void> {
    if (this.config.binary === null) {
      this.send({ type: "start_failed", error: "No pulse binary was found. Set PULSE_BIN, or build it with `cargo build -p pulse-cli`." });
      return;
    }
    try {
      const binary = await this.stableBinary(this.config.binary);
      const env: NodeJS.ProcessEnv = { ...this.config.env, PATH: await this.loginPath() };
      delete env.PULSE_HOME; // `--home` says it.
      const started = await spawnDaemon({ ...this.daemonEnv(env), binary });
      this.log(`started the daemon (pid ${started.pid ?? "?"}, port ${started.info.port})`);
      if (this.state.kind !== "starting") {
        started.client.close();
        return;
      }
      this.attach(started.client, started.info);
      this.send({ type: "started", info: started.info });
      this.afterConnect(started.info, true);
    } catch (e) {
      this.send({ type: "start_failed", error: (e as Error).message });
    }
  }

  private async stopDaemon(): Promise<void> {
    try {
      await stopDaemon(this.daemonEnv());
      this.detach();
      this.send({ type: "stopped" });
    } catch (e) {
      this.send({ type: "stop_failed", error: (e as Error).message });
    }
  }

  /**
   * The binary to start the daemon from. Inside an AppImage the bundled one disappears when the
   * app quits, so it is copied (once per version) to `<userData>/bin` and started from there.
   */
  private async stableBinary(binary: string): Promise<string> {
    const version = (await this.binaryVersion()) ?? this.config.appVersion;
    const target = stableBinaryPath(binary, this.config.env, this.config.userData, version);
    if (target === null) return binary;
    const have = await stat(target).catch(() => null);
    const want = await stat(binary);
    if (have === null || have.size !== want.size) {
      await mkdir(dirname(target), { recursive: true });
      const tmp = `${target}.tmp-${process.pid}`;
      await copyFile(binary, tmp);
      await chmod(tmp, 0o755);
      await rename(tmp, target);
      // Older copies: a daemon still running from one keeps its open file; the name can go.
      for (const f of await readdir(dirname(target))) {
        if (f.startsWith("pulse-") && f !== basename(target) && !f.includes(".tmp-")) {
          await unlink(join(dirname(target), f)).catch(() => {});
        }
      }
      this.log(`copied the daemon binary out of the AppImage to ${target}`);
    }
    return target;
  }

  /** PATH for a daemon the app starts: the login shell's, so agents on the user's PATH are found. */
  private loginPath(): Promise<string | undefined> {
    this.shellPath ??= new Promise((resolve) => {
      const shell = this.config.env.SHELL;
      if (shell === undefined || shell === "") {
        resolve(this.config.env.PATH);
        return;
      }
      const { file, args } = loginShellCommand(shell);
      execFile(file, args, { env: this.config.env, timeout: 5000, maxBuffer: 256 * 1024 }, (err, stdout) => {
        if (err !== null) this.log(`could not read the login shell's PATH: ${err.message}`);
        resolve(mergedPath(this.config.env.PATH, err === null ? String(stdout) : null));
      });
    });
    return this.shellPath;
  }

  private async binaryVersion(): Promise<string | null> {
    if (this.binaryVersionCache === undefined) {
      this.binaryVersionCache = this.config.binary === null ? null : await binaryVersion(this.daemonEnv());
    }
    return this.binaryVersionCache;
  }

  private watchHome(on: boolean): void {
    if (!on) {
      this.homeWatcher?.close();
      this.homeWatcher = null;
      return;
    }
    if (this.homeWatcher !== null) return;
    try {
      this.homeWatcher = watch(this.config.home, { persistent: false }, (_event, file) => {
        if (file === null || file === "daemon.json") this.send({ type: "home_changed" });
      });
      this.homeWatcher.on("error", () => this.watchHome(false));
    } catch (e) {
      // The home does not exist yet (no daemon ever started). "Start the daemon" still works.
      this.log(`not watching ${this.config.home}: ${(e as Error).message}`);
    }
  }

  private async exit(): Promise<void> {
    this.quitting = true;
    await this.saveSettings();
    this.watchHome(false);
    this.throttle.setEnabled(false);
    this.detach();
    this.tray?.destroy();
    this.tray = null;
    this.emit("exit");
    app.quit();
  }

  // ---- the connection ----

  private attach(client: RpcClient, info: DaemonInfo): void {
    this.detach();
    this.client = client;
    client.onNotification((n) => this.onPush(n));
    client.onClose(() => {
      if (this.client !== client) return;
      this.client = null;
      this.activity = EMPTY_ACTIVITY;
      this.log(`lost the connection to the daemon on port ${info.port}`);
      this.send({ type: "disconnected" });
    });
  }

  private detach(): void {
    const c = this.client;
    this.client = null;
    c?.close();
    this.activity = EMPTY_ACTIVITY;
  }

  /** Initial state after connecting, then pushes only. */
  private afterConnect(info: DaemonInfo, owned: boolean): void {
    const client = this.client;
    if (client === null) return;
    void (async () => {
      try {
        const [status, recent, pending, rules] = await Promise.all([
          client.call("system.status", {}),
          client.call("runs.list", { filter: { status: [] }, limit: RECENT_MAX }),
          client.call("runs.list", { filter: { status: ["pending_approval"] }, limit: 5 }),
          client.call("rules.list", { include_archived: false }),
          client.call("runs.subscribe", { filter: { status: [] } }),
        ]);
        if (this.client !== client) return;
        this.runNotices.seed([...recent.runs, ...pending.runs]);
        this.breakerNotices.seed(rules.rules, Date.now());
        this.activity = withRuns(withStatus(this.activity, status), [...recent.runs, ...pending.runs]);
        this.render();
        this.emit("ready");
      } catch (e) {
        this.log(`could not load the daemon's state: ${(e as Error).message}`);
      }
    })();
    void this.checkVersion(info, owned);
  }

  private async checkVersion(info: DaemonInfo, owned: boolean): Promise<void> {
    const appVersion = (await this.binaryVersion()) ?? this.config.appVersion;
    this.advice = versionAdvice(appVersion, info.version);
    this.render();
    if (this.advice.kind !== "daemon_older" || owned) return;
    const key = `${info.version}@${info.started_at}`;
    if (this.askedAbout.has(key)) return;
    this.askedAbout.add(key);
    const { response } = await dialog.showMessageBox({
      type: "question",
      buttons: ["Restart the daemon", "Not now"],
      defaultId: 0,
      cancelId: 1,
      message: "The running Pulse daemon is older than this app",
      detail: `The daemon is version ${info.version}; this app comes with ${this.advice.app}. Restarting it with ${this.advice.app} interrupts runs in progress. You can also do this later from the Lictory Pulse menu.`,
    });
    if (response === 0 && currentDaemon(this.state)?.info.started_at === info.started_at) this.send({ type: "replace" });
  }

  private onPush(n: RpcNotification): void {
    switch (n.method) {
      case "state.changed":
        this.activity = withStatus(this.activity, n.params.status);
        if (n.params.topics.includes("rules")) void this.refreshRules();
        break;
      case "runs.changed": {
        const notices = this.runNotices.notices(n.params.runs);
        this.activity = withRuns(this.activity, n.params.runs);
        for (const notice of notices) this.throttle.offer(notice);
        break;
      }
      default:
        return;
    }
    this.render();
  }

  private async refreshRules(): Promise<void> {
    if (this.refreshingRules) {
      this.refreshRulesAgain = true;
      return;
    }
    this.refreshingRules = true;
    try {
      do {
        this.refreshRulesAgain = false;
        const client = this.client;
        if (client === null) return;
        const list = await client.call("rules.list", { include_archived: false });
        const rules: RuleInfo[] = list.rules;
        const fmt = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
        for (const notice of this.breakerNotices.notices(rules, Date.now(), fmt)) this.throttle.offer(notice);
      } while (this.refreshRulesAgain);
    } catch (e) {
      this.log(`could not read rules: ${(e as Error).message}`);
    } finally {
      this.refreshingRules = false;
    }
  }

  // ---- notifications ----

  private alert(kind: "daemon_lost" | "start_failed" | "stop_failed", detail: string | null): void {
    const titles = {
      daemon_lost: "The Pulse daemon stopped",
      start_failed: "The Pulse daemon could not start",
      stop_failed: "The Pulse daemon did not stop",
    };
    this.throttle.offer({
      kind: "daemon",
      title: titles[kind],
      body: detail ?? "Rules are not running. Open Pulse to start the daemon again.",
      route: { kind: "home" },
      event: null,
    });
  }

  private async present(n: Notice): Promise<void> {
    let subject: string | null = null;
    if (n.event !== null && this.client !== null) {
      try {
        const detail = await this.client.call("events.get", { id: n.event });
        subject = typeof detail.event.subject === "string" ? detail.event.subject : null;
      } catch {
        // The notice is still worth showing without it.
      }
    }
    const p = this.config.platform;
    const title = plainText(n.title, 80);
    const body = [notificationText(n.body, 180, p), subject === null ? null : notificationText(subject, 120, p)]
      .filter((x) => x !== null)
      .join("\n");
    const shown: ShownNotice = { kind: n.kind, title, body, route: n.route };
    if (!Notification.isSupported()) {
      if (!this.unsupportedLogged) this.log("this system has no notification service; notifications are not shown");
      this.unsupportedLogged = true;
      this.emit("notification", shown, null);
      return;
    }
    const note = new Notification({ title, body });
    note.on("click", () => void this.openRoute(n.route));
    note.show();
    this.emit("notification", shown, note);
    // Electron drops click handlers of notifications that get garbage-collected.
    this.shownNotifications.push(note);
    if (this.shownNotifications.length > KEEP_NOTIFICATIONS) this.shownNotifications.shift();
  }

  // ---- actions ----

  async dispatch(action: Action): Promise<void> {
    this.emit("action", action);
    switch (action.type) {
      case "open":
        await this.openRoute(action.route);
        return;
      case "pause":
      case "resume": {
        const client = this.client;
        if (client === null) return;
        try {
          const status = await client.call(action.type === "pause" ? "system.pause" : "system.resume", {});
          this.activity = withStatus(this.activity, status);
          this.render();
        } catch (e) {
          this.log(`could not ${action.type}: ${(e as Error).message}`);
        }
        return;
      }
      case "start_daemon":
        this.send({ type: "start" });
        return;
      case "replace_daemon": {
        const { response } = await dialog.showMessageBox({
          type: "question",
          buttons: ["Restart the daemon", "Cancel"],
          defaultId: 0,
          cancelId: 1,
          message: "Restart the Pulse daemon with this app's version?",
          detail: "Runs in progress are interrupted. Queued runs start again after the restart.",
        });
        if (response === 0) this.send({ type: "replace" });
        return;
      }
      case "toggle_notifications":
        this.settings = { ...this.settings, notifications: !this.settings.notifications };
        this.throttle.setEnabled(this.settings.notifications);
        await this.saveSettings();
        this.render();
        return;
      case "toggle_close_to_tray":
        this.settings = { ...this.settings, closeToTray: this.closeHides() ? "never" : "always" };
        await this.saveSettings();
        this.render();
        return;
      case "install_cli":
        await this.installCli();
        return;
      case "quit":
        this.send({ type: "quit" });
        return;
      case "quit_and_stop":
        this.send({ type: "stop", then: "quit" });
        return;
    }
  }

  private async installCli(): Promise<void> {
    if (this.config.binary === null) return;
    const binary = await this.stableBinary(this.config.binary);
    const r = await installCli(binary, homedir(), this.config.env.PATH);
    const message =
      r.kind === "installed"
        ? `Installed ${r.link}`
        : r.kind === "exists"
          ? `${r.link} already exists and is not a link this app made; it was left alone.`
          : `Could not install the command line tool: ${r.message}`;
    const lines: string[] = [];
    if (r.kind === "installed") {
      lines.push(`It runs ${r.target}.`);
      if (!r.onPath) lines.push(`~/.local/bin is not on your PATH. Add it, for example:\necho 'export PATH="$HOME/.local/bin:$PATH"' >> ${this.config.platform === "darwin" ? "~/.zprofile" : "~/.profile"}`);
      if (this.config.env.APPIMAGE !== undefined) lines.push("It is a copy of this version: run this again after updating the app.");
    }
    await dialog.showMessageBox({ type: r.kind === "installed" ? "info" : "warning", message, detail: lines.join("\n\n") });
  }

  // ---- window ----

  closeHides(): boolean {
    return this.tray !== null && closeHidesToTray(this.config.platform, this.settings.closeToTray, this.config.env.XDG_CURRENT_DESKTOP);
  }

  private ensureWindow(show: boolean): BrowserWindow {
    if (this.window !== null && !this.window.isDestroyed()) return this.window;
    const win = createWindow({
      settings: this.settings,
      preload: this.config.preload,
      icon: this.config.windowIcon,
      devTools: this.config.devTools,
    });
    this.window = win;
    this.showing = null;
    if (show) win.once("ready-to-show", () => win.show());
    win.on("close", (e) => {
      this.settings = { ...this.settings, window: windowPlacement(win) };
      void this.saveSettings();
      if (this.quitting || !this.closeHides()) return;
      e.preventDefault();
      win.hide();
      if (!this.trayHintShown) {
        this.trayHintShown = true;
        this.throttle.offer({
          kind: "daemon",
          title: "Lictory Pulse is still in the tray",
          body: "The daemon keeps running either way. Quit from the tray menu to close the app.",
          route: { kind: "home" },
          event: null,
        });
      }
    });
    win.on("closed", () => {
      this.window = null;
      this.showing = null;
      // Without a tray to come back from, closing the window quits the app. The daemon stays.
      if (!this.quitting) this.send({ type: "quit" });
    });
    win.webContents.on("render-process-gone", (_e, details) => {
      this.log(`the window's renderer exited (${details.reason})`);
      this.showing = null;
      this.syncWindow();
    });
    this.syncWindow();
    return win;
  }

  /** Brings the window up on a view; the default view keeps whatever the window shows. */
  async openRoute(route: Route): Promise<void> {
    const win = this.ensureWindow(false);
    if (win.isMinimized()) win.restore();
    win.show();
    win.focus();
    if (route.kind !== "home" || this.showing?.kind !== "ui") await this.load(route);
  }

  /** Keeps what the window shows in line with the daemon's state. */
  private syncWindow(): void {
    const win = this.window;
    if (win === null || win.isDestroyed()) return;
    const origin = this.origin();
    if (this.state.kind === "connected" && origin !== null) {
      if (this.showing?.kind !== "ui" || this.showing.origin !== origin) void this.load({ kind: "home" });
      return;
    }
    if (this.state.kind === "stopping" && this.showing?.kind === "ui") return;
    this.showStatus();
  }

  private showStatus(): void {
    const win = this.window;
    if (win === null || win.isDestroyed()) return;
    const s = this.state;
    const [title, lines] =
      s.kind === "unreachable"
        ? [
            s.reason === "start_failed" ? "The Pulse daemon could not start" : "The Pulse daemon is not running",
            [
              ...(s.detail ? [s.detail] : []),
              "Rules do not run while it is stopped. Start it from the Lictory Pulse menu, or run pulse start in a terminal: this window picks it up either way.",
            ],
          ]
        : s.kind === "stopping"
          ? ["Stopping the Pulse daemon…", ["Runs in progress are being stopped."]]
          : ["Starting Lictory Pulse…", [`Pulse home: ${this.config.home}`]];
    this.showing = { kind: "status" };
    void win.loadURL(statusPage(title, lines)).catch(() => {});
  }

  private async load(route: Route): Promise<void> {
    const win = this.window;
    const d = currentDaemon(this.state);
    if (win === null || win.isDestroyed() || d === null || this.state.kind !== "connected") {
      this.showStatus();
      return;
    }
    const origin = daemonOrigin(d.info.port);
    this.showing = { kind: "ui", origin };
    try {
      const code = await mintLoginCode(this.daemonEnv(), d.info.port);
      const url = loginUrl(origin, route, code);
      this.log(`window: ${redact(url)}`);
      await win.loadURL(url);
    } catch (e) {
      const msg = (e as Error).message;
      // ERR_ABORTED: another load replaced this one.
      if (msg.includes("ERR_ABORTED")) return;
      this.log(`could not load the UI: ${msg}`);
      if (this.window === win && !win.isDestroyed() && isDaemonUrl(win.webContents.getURL(), origin) === false) {
        this.showing = { kind: "status" };
        void win.loadURL(statusPage("Lictory Pulse could not open its window", [msg])).catch(() => {});
      }
    }
  }

  /** A link to the UI that a page tried to open in a new window opens here instead. */
  openInWindow(url: string): void {
    const win = this.window;
    if (win === null || win.isDestroyed()) return;
    void win.loadURL(url).catch(() => {});
  }

  // ---- rendering ----

  model(): TrayModel {
    return trayModel({
      lifecycle: this.state,
      activity: this.activity,
      settings: this.settings,
      platform: this.config.platform,
      advice: this.advice,
      closeHides: this.closeHides(),
      sandboxOff: this.config.sandboxOff,
    });
  }

  private render(): void {
    const model = this.model();
    this.tray?.render(model);
    const key = JSON.stringify(model.items);
    if (key !== this.lastMenuKey) {
      this.lastMenuKey = key;
      Menu.setApplicationMenu(applicationMenu(model, (a) => void this.dispatch(a), this.config.devTools));
    }
    this.emit("model", model);
  }

  // ---- settings ----

  private get settingsFile(): string {
    return join(this.config.userData, "settings.json");
  }

  private async loadSettings(): Promise<Settings> {
    try {
      return parseSettings(JSON.parse(await readFile(this.settingsFile, "utf8")));
    } catch {
      return DEFAULT_SETTINGS;
    }
  }

  async saveSettings(): Promise<void> {
    const win = this.window;
    if (win !== null && !win.isDestroyed()) this.settings = { ...this.settings, window: windowPlacement(win) };
    try {
      await mkdir(this.config.userData, { recursive: true });
      const tmp = `${this.settingsFile}.tmp`;
      await writeFile(tmp, `${JSON.stringify(this.settings, null, 2)}\n`);
      await rename(tmp, this.settingsFile);
    } catch (e) {
      this.log(`could not save settings: ${(e as Error).message}`);
    }
  }
}
