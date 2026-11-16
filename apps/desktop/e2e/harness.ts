// End-to-end harness: boots the real shell (src/main.ts) under Electron and drives it the way a
// person would, through the real menu items, the page and the daemon. Not part of the app; built
// with `node scripts/build.mjs --e2e` and run by e2e/run.sh.
//
// Only side effects that would leave the machine are replaced, so they can be observed:
// `shell.openExternal` (the system browser), `shell.showItemInFolder` (the file manager) and
// message boxes (answered from PULSE_E2E_ANSWER). Everything else is the app as shipped.
//
// PULSE_E2E selects the scenario; results go to $PULSE_E2E_OUT/<scenario>.json, screenshots next
// to it. Waits are on events, with one ceiling per wait.

import { execFile, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { EventEmitter } from "node:events";
import { readFileSync, writeFileSync } from "node:fs";
import { type AddressInfo, createServer } from "node:net";
import { join } from "node:path";

import electron, { type BrowserWindow, type MenuItem, type Notification, app } from "electron";

import { boot } from "../src/main.ts";
import type { LifecycleState } from "../src/lifecycle.ts";
import type { ShownNotice } from "../src/shell.ts";

const scenario = process.env.PULSE_E2E ?? "main";
const OUT = process.env.PULSE_E2E_OUT ?? ".";
const BIN = process.env.PULSE_BIN ?? "pulse";
const HOME = process.env.PULSE_HOME ?? "";
const CEILING = 30_000;

const results: Record<string, unknown> = { scenario };
const record = (key: string, value: unknown) => {
  results[key] = value;
  process.stderr.write(`[e2e] ${key}: ${JSON.stringify(value)}\n`);
};

// Any of these means "something may have changed": re-check what is being waited for.
const tick = new EventEmitter();
tick.setMaxListeners(100);

const opened: string[] = [];
electron.shell.openExternal = async (url: string) => {
  opened.push(url);
  tick.emit("tick");
};
const revealed: string[] = [];
electron.shell.showItemInFolder = (path: string) => {
  revealed.push(path);
  tick.emit("tick");
};
const dialogs: string[] = [];
const answer = Number(process.env.PULSE_E2E_ANSWER ?? "1");
electron.dialog.showMessageBox = (async (...args: unknown[]) => {
  const opts = (args.length > 1 ? args[1] : args[0]) as { message: string };
  dialogs.push(opts.message);
  tick.emit("tick");
  return { response: answer, checkboxChecked: false };
}) as typeof electron.dialog.showMessageBox;

/** The URL of the last page that finished loading in any web contents (there is one window). */
let lastFinished = "";

app.on("web-contents-created", (_e, wc) => {
  wc.on("did-finish-load", () => {
    lastFinished = wc.getURL();
    tick.emit("tick");
  });
  wc.on("did-finish-load", () => process.stderr.write(`[e2e] did-finish-load ${wc.getURL().replace(/#.*/, "").slice(0, 80)}\n`));
  wc.on("did-fail-load", (_ev, code, desc, url) => process.stderr.write(`[e2e] did-fail-load ${code} ${desc} ${url.replace(/#.*/, "").slice(0, 80)}\n`));
  for (const ev of ["did-finish-load", "did-navigate", "did-navigate-in-page", "did-fail-load", "will-frame-navigate"] as const) {
    wc.on(ev as "did-finish-load", () => tick.emit("tick"));
  }
});

async function until<T>(what: string, check: () => T | null | undefined | false | Promise<T | null | undefined | false>): Promise<T> {
  const first = await check();
  if (first) return first;
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      tick.off("tick", on);
      reject(new Error(`timed out waiting for ${what}`));
    }, CEILING);
    let busy = false;
    // A tick during a check may come with the change the check just missed: check again.
    let again = false;
    const on = async () => {
      if (busy) {
        again = true;
        return;
      }
      busy = true;
      try {
        do {
          again = false;
          const v = await check();
          if (v) {
            clearTimeout(timer);
            tick.off("tick", on);
            resolve(v);
            return;
          }
        } while (again);
      } finally {
        busy = false;
      }
    };
    tick.on("tick", on);
  });
}

function pulse(args: string[], input?: string): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const child = execFile(BIN, ["--home", HOME, ...args], { timeout: 60_000 }, (err, stdout, stderr) => {
      resolve({ code: err === null ? 0 : typeof err.code === "number" ? err.code : 1, stdout, stderr });
    });
    if (input !== undefined) child.stdin?.end(input);
  });
}

async function daemonPaused(): Promise<boolean | null> {
  const r = await pulse(["--json", "status"]);
  try {
    return (JSON.parse(r.stdout) as { status: { paused: boolean } }).status.paused;
  } catch {
    return null;
  }
}

function menuItem(label: string): MenuItem {
  const menu = electron.Menu.getApplicationMenu();
  const pulseMenu = menu?.items[0]?.submenu;
  const find = (items: MenuItem[]): MenuItem | undefined => {
    for (const i of items) {
      if (i.label === label) return i;
      const sub = i.submenu ? find(i.submenu.items) : undefined;
      if (sub) return sub;
    }
    return undefined;
  };
  const item = pulseMenu ? find(pulseMenu.items) : undefined;
  if (!item) throw new Error(`no menu item "${label}"`);
  return item;
}

function menuLabels(): string[] {
  return electron.Menu.getApplicationMenu()?.items[0]?.submenu?.items.map((i) => i.label || "---") ?? [];
}

/** Clicks a menu item the way Electron does when a person picks it. */
function click(label: string): void {
  const item = menuItem(label);
  if (!item.enabled) throw new Error(`menu item "${label}" is disabled`);
  record(`clicked`, label);
  item.click();
}

async function screenshot(win: BrowserWindow, name: string): Promise<void> {
  // Rendered text is not yet painted: let two frames go by, or the capture can be blank.
  await inPage(win, "new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))");
  const image = await win.webContents.capturePage();
  const file = join(OUT, `${scenario}-${name}.png`);
  writeFileSync(file, image.toPNG());
  record(`screenshot:${name}`, file);
}

const inPage = <T>(win: BrowserWindow, code: string): Promise<T> => win.webContents.executeJavaScript(code, true) as Promise<T>;

/** Waits until the page has rendered something into its root element. */
function rendered(win: BrowserWindow): Promise<boolean> {
  return inPage(
    win,
    `new Promise((resolve) => {
      const ready = () => document.body && document.body.innerText.trim().length > 0 && !document.querySelector('[aria-busy="true"]');
      if (ready()) return resolve(true);
      const o = new MutationObserver(() => { if (ready()) { o.disconnect(); resolve(true); } });
      o.observe(document, { subtree: true, childList: true, characterData: true, attributes: true });
    })`,
  );
}

/** Proportional set size per Electron process (shared pages split between their users). */
function memory(): { type: string; pid: number; pssMB: number | null; rssMB: number | null }[] {
  return app.getAppMetrics().map((m) => {
    let pss: number | null = null;
    let rss: number | null = null;
    try {
      const text = readFileSync(`/proc/${m.pid}/smaps_rollup`, "utf8");
      pss = Number(/^Pss:\s+(\d+)/m.exec(text)?.[1]) / 1024;
      rss = Number(/^Rss:\s+(\d+)/m.exec(text)?.[1]) / 1024;
    } catch {
      // Not Linux.
    }
    return { type: m.type, pid: m.pid, pssMB: pss === null ? null : Math.round(pss), rssMB: rss === null ? null : Math.round(rss) };
  });
}

const urlOf = (win: BrowserWindow) => new URL(win.webContents.getURL());

/** Waits (up to the ceiling) until the UI no longer says it is connecting; reports which. */
function uiConnected(win: BrowserWindow): Promise<string> {
  return inPage(
    win,
    `new Promise((resolve) => {
      const done = () => !document.body.innerText.includes("Connecting");
      if (done()) return resolve("connected");
      const o = new MutationObserver(() => { if (done()) { o.disconnect(); resolve("connected"); } });
      o.observe(document, { subtree: true, childList: true, characterData: true });
      setTimeout(() => { o.disconnect(); resolve("still connecting after ${CEILING / 1000}s"); }, ${CEILING});
    })`,
  );
}

/**
 * For `stale`: a program on the port a stale `daemon.json` names. It records what it receives,
 * answers every request with a 404, and must never see the token.
 */
async function squat(): Promise<{ port: number; received: () => string }> {
  let received = "";
  const server = createServer((socket) => {
    socket.on("data", (d: Buffer) => {
      received += d.toString("latin1");
      if (received.includes("\r\n\r\n")) socket.write("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
    });
    socket.on("error", () => {});
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const port = (server.address() as AddressInfo).port;
  // A home that has had a daemon has a token; without one the shell has nothing to send.
  writeFileSync(join(HOME, "token"), randomBytes(32).toString("hex"), { mode: 0o600 });
  writeFileSync(join(HOME, "daemon.json"), JSON.stringify({ pid: 999999, port, version: "0.1.0", started_at: "2026-01-01T00:00:00Z" }));
  return { port, received: () => received };
}

const token = () => readFileSync(join(HOME, "token"), "utf8").trim();

async function main(): Promise<void> {
  const squatter = scenario === "stale" ? await squat() : null;
  const booted = await boot();
  if (booted === null) {
    record("second_instance", "exited: another instance holds the lock");
    return; // boot() already called app.quit().
  }
  const shell = booted.shell;
  const notices: { shown: ShownNotice; note: Notification | null }[] = [];
  shell.on("state", () => tick.emit("tick"));
  shell.on("model", () => tick.emit("tick"));
  shell.on("ready", () => tick.emit("tick"));
  shell.on("notification", (shown: ShownNotice, note: Notification | null) => {
    notices.push({ shown, note });
    process.stderr.write(`[e2e] notification: ${shown.kind}: ${shown.title} | ${shown.body.replace(/\n/g, " / ")}\n`);
    tick.emit("tick");
  });
  const connected = () => (shell.state.kind === "connected" ? shell.state : null);
  const state = (kind: LifecycleState["kind"]) => () => shell.state.kind === kind;
  const win = () => shell.window as BrowserWindow;
  const onDaemonPage = () => {
    const origin = shell.origin();
    return origin !== null && shell.window !== null && lastFinished.startsWith(origin) && win().webContents.getURL().startsWith(origin);
  };

  switch (scenario) {
    case "main": {
      const c = await until("a daemon", connected);
      record("daemon", { owned: c.daemon.owned, port: c.daemon.info.port, pid: c.daemon.info.pid, version: c.daemon.info.version });
      await until("the UI to load from the daemon", onDaemonPage);
      const url = urlOf(win());
      record("window_url", { origin: url.origin, path: url.pathname, fragment_has_login_code: /^#login=\d+\.[0-9a-f]+$/.test(url.hash), fragment_has_token: url.hash.includes("token") });
      record("web_preferences", (() => {
        // Not in the typings, but present at runtime in recent Electron versions.
        const wc = win().webContents as unknown as { getLastWebPreferences?: () => Record<string, unknown> | null };
        const p = wc.getLastWebPreferences?.() ?? {};
        return { contextIsolation: p.contextIsolation, nodeIntegration: p.nodeIntegration, sandbox: p.sandbox, webSecurity: p.webSecurity, webviewTag: p.webviewTag };
      })());
      await rendered(win());
      // The UI signs in by itself: it takes the login code from the fragment, removes it from
      // the address bar, exchanges it at /auth/login and keeps the token for this window.
      record("ui_connection_on_load", await uiConnected(win()));
      const storage = `JSON.stringify({ ...sessionStorage }) + JSON.stringify({ ...localStorage })`;
      const login = await inPage<{ fragment_left: string; stored: string; no_access_screen: boolean }>(
        win(),
        `({ fragment_left: location.hash, stored: ${storage}, no_access_screen: document.querySelector(".no-access") !== null })`,
      );
      // The page keeps the token in memory only.
      record("login_by_the_page", { fragment_left: login.fragment_left, token_in_storage: login.stored.includes(token()), no_access_screen: login.no_access_screen });
      await screenshot(win(), "1-ui-signed-in");
      // A reload of the window keeps working without a new code.
      lastFinished = "";
      win().webContents.reload();
      await until("the UI to reload", onDaemonPage);
      await rendered(win());
      record("ui_connection_after_login", await uiConnected(win()));
      record("page_text_after_login", (await inPage<string>(win(), "document.body.innerText")).slice(0, 300));
      await screenshot(win(), "2-ui-after-reload");

      // The renderer's reach.
      record(
        "page_globals",
        await inPage(win(), `({ require: typeof require, process: typeof process, bridge: Object.keys(window.pulseDesktop || {}), frozen: Object.isFrozen(window.pulseDesktop) })`),
      );
      record(
        "reveal_path",
        await inPage(win(), `Promise.all([window.pulseDesktop.revealPath("relative/path"), window.pulseDesktop.revealPath("/no/such/file"), window.pulseDesktop.revealPath(${JSON.stringify(HOME)})])`),
      );
      record("revealed", [...revealed]);

      // Navigation, windows, frames, permissions.
      const before = win().webContents.getURL();
      await inPage(win(), `location.href = "file:///etc/passwd"; 1`);
      await inPage(win(), `location.href = "https://example.com/navigate"; 1`);
      await until("the external link to be handed to the browser", () => opened.includes("https://example.com/navigate"));
      const popup = await inPage(win(), `String(window.open("https://example.com/popup"))`);
      await until("the popup link to be handed to the browser", () => opened.includes("https://example.com/popup"));
      await inPage(win(), `window.open("javascript:alert(1)"); window.open("file:///etc/passwd"); 1`);
      // The daemon's Content-Security-Policy refuses the frame before the shell's own
      // navigation lock would see it; either layer must stop it.
      const framed: string[] = [];
      win().webContents.on("will-frame-navigate", (d) => framed.push(`shell ${d.url}`));
      win().webContents.on("did-fail-load", (_e, _code, desc, url) => {
        framed.push(`${desc === "ERR_BLOCKED_BY_CSP" ? "csp" : desc} ${url}`);
        tick.emit("tick");
      });
      await inPage(win(), `const f = document.createElement("iframe"); f.src = "https://example.com/frame"; document.body.appendChild(f); 1`);
      const frameBlockedBy = await until("the frame navigation to be stopped", () => framed.find((f) => f.endsWith(" https://example.com/frame"))?.split(" ")[0]);
      const permissions = await inPage(
        win(),
        `(async () => {
          const settle = (p) => p.then((v) => "granted:" + String(v).slice(0, 20), (e) => "denied:" + e.name);
          return {
            camera: await settle(navigator.mediaDevices.getUserMedia({ video: true })),
            notifications: await Notification.requestPermission(),
            clipboard_read: await settle(navigator.clipboard.readText()),
            geolocation: await new Promise((r) => navigator.geolocation.getCurrentPosition(() => r("granted"), (e) => r("denied:" + e.code))),
            midi: await settle(navigator.requestMIDIAccess()),
          };
        })()`,
      );
      record("security", {
        url_unchanged: win().webContents.getURL() === before,
        windows: electron.BrowserWindow.getAllWindows().length,
        window_open_returned: popup,
        sent_to_browser: [...opened],
        frame_blocked_by: frameBlockedBy,
        frame_blocked: await inPage(win(), `document.querySelector("iframe").contentDocument !== null && document.querySelector("iframe").contentDocument.location.href`),
        permissions,
      });
      lastFinished = "";
      await inPage(win(), `window.open(location.origin + "/rules"); 1`);
      await until("a UI link from window.open to load in the window", () => onDaemonPage() && urlOf(win()).pathname === "/rules");
      record("ui_link_in_window", { path: urlOf(win()).pathname, windows: electron.BrowserWindow.getAllWindows().length });

      // Pause and resume through the menu, checked with an independent client.
      await until("the menu to offer pause", () => menuLabels().includes("Pause everything"));
      click("Pause everything");
      await until("the menu to offer resume", () => menuLabels().includes("Resume"));
      record("paused_per_cli", await daemonPaused());
      record("menu_while_paused", menuLabels());
      click("Resume");
      await until("the menu to offer pause again", () => menuLabels().includes("Pause everything"));
      record("paused_per_cli_after_resume", await daemonPaused());

      // Notifications: a breaker trip, a failed run, then a storm of approvals.
      const lines = (type: string, n: number) => Array.from({ length: n }, (_, i) => JSON.stringify({ type, subject: `${type} <b>#${i}</b>‮` })).join("\n");
      record("ingest_hot", (await pulse(["ingest", "-s", "test"], lines("hot", 6))).code);
      await until("a breaker notification", () => notices.some((n) => n.shown.kind === "breaker"));
      record("ingest_fail", (await pulse(["ingest", "-s", "test"], lines("fail", 1))).code);
      await until("a failure notification", () => notices.some((n) => n.shown.kind === "failed"));
      record("ingest_approve", (await pulse(["ingest", "-s", "test"], lines("approve", 1))).code);
      await until("an approval notification", () => notices.some((n) => n.shown.kind === "approval"));
      // Three shown so far: the burst is used up. A storm now must be summarized, not shown.
      record("ingest_storm", (await pulse(["ingest", "-s", "test"], lines("approve", 12))).code);
      const shownBeforeSummary = notices.length;
      await until("the storm summary", () => notices.some((n) => n.shown.kind === "summary"));
      record("notifications", notices.map((n) => n.shown));
      record("notifications_before_summary", shownBeforeSummary);
      await until("approvals in the menu", () => menuLabels().some((l) => l.startsWith("Waiting for approval")));
      record("menu_with_activity", menuLabels());
      record("tray_model", shell.model());
      const approval = notices.find((n) => n.shown.kind === "approval");
      lastFinished = "";
      // Without a notification service here there is no Notification object to click; then the
      // shell's click handler (openRoute with the notice's route) is called directly.
      record("notification_click", approval?.note ? "emitted click on the Notification" : "no notification service: called the click handler");
      if (approval?.note) approval.note.emit("click");
      else if (approval) await shell.openRoute(approval.shown.route);
      await until("the run to open from its notification", () => {
        if (!onDaemonPage() || approval?.shown.route.kind !== "run") return false;
        return urlOf(win()).pathname === `/runs/${approval.shown.route.id}`;
      });
      record("notification_click_opened", urlOf(win()).pathname);
      await rendered(win());
      await uiConnected(win());
      await screenshot(win(), "3-run-from-notification");

      // Closing the window hides it to the tray (XDG_CURRENT_DESKTOP=KDE here).
      win().close();
      await until("the window to hide", () => !win().isVisible());
      record("after_close", { visible: win().isVisible(), destroyed: win().isDestroyed(), state: shell.state.kind });
      click("Open Lictory Pulse");
      await until("the window to show again", () => win().isVisible());

      // A second instance focuses this one and exits.
      win().hide();
      const second = spawn(process.execPath, process.argv.slice(1), { env: { ...process.env, PULSE_E2E: "second" }, stdio: ["ignore", "ignore", "pipe"] });
      let secondErr = "";
      second.stderr.on("data", (d: Buffer) => (secondErr += d.toString()));
      const secondExit = await new Promise<number | null>((resolve) => second.on("exit", (code) => resolve(code)));
      await until("the window to be shown by the second instance", () => win().isVisible());
      record("second_instance", { pid: second.pid, exit_code: secondExit, first_window_visible: win().isVisible(), said: secondErr.split("\n").filter((l) => l.includes("[pulse-desktop]")) });

      record("memory_window_shown", memory());
      win().hide();
      // Give the hidden page nothing to do; measure what a tray-only app holds.
      await inPage(win(), "new Promise((r) => requestIdleCallback(() => r(1)))");
      record("memory_window_hidden", memory());
      win().show();
      record("app_metrics", app.getAppMetrics().map((m) => ({ type: m.type, pid: m.pid, workingSetMB: Math.round(m.memory.workingSetSize / 1024), privateMB: m.memory.privateBytes === undefined ? null : Math.round(m.memory.privateBytes / 1024) })));
      record("open_external_calls", [...opened]);

      // Quit: the daemon keeps running.
      click("Quit Lictory Pulse");
      break;
    }

    case "adopt": {
      const c = await until("a daemon", connected);
      record("daemon", { owned: c.daemon.owned, pid: c.daemon.info.pid, port: c.daemon.info.port });
      await until("the UI to load", onDaemonPage);
      const stop = await pulse(["stop"]);
      record("external_pulse_stop", stop.code);
      await until("the shell to see the daemon gone", state("unreachable"));
      await until("a notification about it", () => notices.some((n) => n.shown.kind === "daemon"));
      await until("the status page", () => lastFinished.startsWith("data:"));
      await screenshot(win(), "1-daemon-gone");
      record("menu_while_gone", menuLabels());
      record("notifications", notices.map((n) => n.shown));
      lastFinished = "";
      click("Start the daemon");
      const again = await until("a new daemon", connected);
      record("restarted", { owned: again.daemon.owned, pid: again.daemon.info.pid, port: again.daemon.info.port });
      await until("the UI to load again", onDaemonPage);
      click("Quit Lictory Pulse and stop the daemon");
      break;
    }

    case "stale": {
      const c = await until("a daemon", connected);
      record("daemon", { owned: c.daemon.owned, pid: c.daemon.info.pid, port: c.daemon.info.port });
      const seen = squatter?.received() ?? "";
      record("program_on_the_stale_port", {
        port: squatter?.port,
        asked_for_proof: seen.includes("GET /auth/proof?nonce="),
        received_token: seen.includes(token()),
        received_websocket: /upgrade/i.test(seen),
      });
      click("Quit Lictory Pulse and stop the daemon");
      break;
    }

    case "older": {
      const c = await until("a daemon", connected);
      record("first_daemon", { owned: c.daemon.owned, version: c.daemon.info.version, pid: c.daemon.info.pid });
      await until("the restart question", () => dialogs.length > 0);
      record("dialogs", [...dialogs]);
      const replaced = await until("the replacement daemon", () => connected()?.daemon.owned && connected());
      record("replacement", { owned: replaced.daemon.owned, version: replaced.daemon.info.version, pid: replaced.daemon.info.pid });
      record("menu", menuLabels());
      click("Quit Lictory Pulse and stop the daemon");
      break;
    }

    case "gnome": {
      const c = await until("a daemon", connected);
      record("daemon", { owned: c.daemon.owned, pid: c.daemon.info.pid });
      await until("the UI to load", onDaemonPage);
      record("close_hides", shell.closeHides());
      win().close();
      // The app quits by itself; e2e/run.sh checks that the process ended and the daemon did not.
      break;
    }

    // Not in run.sh's list: for the docs screenshot, against a Pulse home that already has
    // activity (a running daemon is adopted). Run with XDG_CURRENT_DESKTOP=GNOME so closing the
    // window quits.
    case "showcase": {
      await until("a daemon", connected);
      await until("the UI to load", onDaemonPage);
      await rendered(win());
      record("ui_connection_on_load", await uiConnected(win()));
      // The demo home has a run waiting for approval: wait for the feed and the run index both.
      record("rows", await inPage(win(), `new Promise((resolve) => {
        const n = () => document.querySelectorAll(".event-row").length;
        const ready = () => n() >= 5 && document.querySelector(".banner-approval") !== null;
        if (ready()) return resolve(n());
        const o = new MutationObserver(() => { if (ready()) { o.disconnect(); resolve(n()); } });
        o.observe(document, { subtree: true, childList: true });
      })`));
      await screenshot(win(), "feed");
      win().close();
      break;
    }

    default:
      throw new Error(`unknown scenario ${scenario}`);
  }
}

app.on("will-quit", () => {
  writeFileSync(join(OUT, `${scenario}.json`), `${JSON.stringify(results, null, 2)}\n`);
});

main().catch((e: Error) => {
  record("error", e.message);
  writeFileSync(join(OUT, `${scenario}.json`), `${JSON.stringify(results, null, 2)}\n`);
  app.exit(1);
});
