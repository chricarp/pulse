# 0010: The desktop shell and packaging

Status: v1, 2026-11-16. How `apps/desktop` implements the shell that [0001](0001-stack-and-desktop-shell.md) decided on, and how the project is built and packaged. Each point says why.

## What the shell is

An Electron app that owns a window, a tray menu, notifications, and making sure a daemon runs. It holds no Pulse state the daemon does not have (it keeps a bounded view of pushes: the status, six recent runs, five waiting approvals), never opens the database or the config file, and never spawns agents. The window shows the UI **served by the daemon**, so there is one UI build and one origin.

The main process is a handful of modules. The decisions live in pure modules with unit tests (no display needed); the Electron modules only render and wire them.

| Module | Role | Tested by |
|---|---|---|
| `lifecycle.ts` | The daemon lifecycle as a pure state machine: states, events, effects | `lifecycle.test.ts` |
| `menu.ts`, `activity.ts` | The tray menu as data, built from daemon state; the bounded run lists | `menu.test.ts` |
| `notify.ts` | Which changes interrupt, de-duplication, the token-bucket throttle (injected timers) | `notify.test.ts` |
| `urls.ts`, `text.ts`, `settings.ts`, `paths.ts`, `version.ts` | Navigation and link policy, untrusted-text handling, window placement, tray-less fallback, paths, version skew | `policy.test.ts` |
| `rpc.ts` | JSON-RPC over the daemon's WebSocket (the protocol every client uses) | `rpc.test.ts` |
| `daemon.ts` | Find, verify, start, stop the daemon; mint login codes (through the `pulse` binary) | e2e |
| `shell.ts`, `window.ts`, `tray.ts`, `main.ts`, `preload.ts` | Electron wiring | e2e |

Wire types come from the generated `apps/web/src/contracts/index.ts`, imported type-only (`src/contracts.ts`), so nothing is hand-written and nothing from `apps/web` ends up in the shell's bundle.

## Daemon lifecycle

- **Finding a daemon.** Read `$PULSE_HOME/daemon.json`, ask its port for the proof that it knows the token (`proof.ts`; [0004](0004-daemon-and-config.md)), connect with the token, call `system.status`, and check that `pulse_home` (after `realpath`) and `port` match. A stale file (the daemon crashed; the port is free or now someone else's) counts as no daemon, and a port that cannot prove itself is never sent the token (the log says "something else is listening on Pulse's port"). Only then is the bundled binary started; if the other program holds the configured port, the daemon's start fails and says the port is in use.
- **Starting one** is `pulse --home <home> start --detach`: the daemon runs in its own process group, so it outlives the app and no signal aimed at the app reaches it, and the command returns once the daemon listens. The shell reuses the CLI's code path instead of reimplementing it.
- **PATH.** A daemon started by an app launched from the Dock or a desktop entry would inherit a minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin` on macOS), and agents installed with Homebrew or into `~/.local/bin` would not be found. Before starting a daemon, the shell asks the login shell for its PATH (`$SHELL -ilc`, five-second limit, between markers because rc files print) and puts its entries first. A daemon started from a terminal (`pulse start`) gets that terminal's environment as before.
- **Quit policy.** Quitting the app never stops the daemon, whoever started it: Pulse watches and wakes agents without a window, and that is the point of the architecture. **"Quit Pulse and stop the daemon"** in the tray and window menus does stop it (with `pulse stop`, which waits until the daemon has exited and terminated its runs). The app never stops a daemon unless the user picks that, or agrees to replace an older one (below). Every way of quitting (menu, Cmd+Q, logout) goes through the lifecycle, so a quit during a stop waits for the stop.
- **The daemon dies while the app is open.** The RPC socket closes. The shell looks once more (it may have been restarted on another port) and otherwise says so: the window shows a status page, the tray shows "The daemon is not running" with **Start the daemon**, and one notification is raised. It does not restart the daemon by itself: the user may have stopped it on purpose. While no daemon is reachable it watches the Pulse home for `daemon.json` (an `fs.watch`, not a poll), so a daemon started from a terminal is picked up.
- **Version skew.** The app's version is the bundled binary's (`scripts/package.mjs` sets it). The window always shows the daemon's own UI, so the window matches the daemon; what can drift is the shell's use of RPC. A daemon **older** than the app: the shell asks once (per daemon start) whether to restart it with the app's binary, and offers it in the menu afterwards. A daemon **newer** than the app: the menu says so and nothing else, because replacing it with an older binary could meet a newer database. Run summaries that do not parse are skipped, not fatal.
- **Second instance.** `requestSingleInstanceLock` per Pulse home: a second launch focuses the first window and exits.
- **`PULSE_HOME`** is honoured like the CLI does. A development instance with a non-default home gets its own data directory (`<userData>/homes/<hash>`), so its window state and single-instance lock are its own and it can run next to the installed app. `PULSE_BIN` overrides the binary; without it a development build uses `target/release/pulse` or `target/debug/pulse`.
- **AppImage.** An AppImage is mounted under `/tmp/.mount_*` only while the app runs. A daemon started from inside it would lose its executable when the app quits. So inside an AppImage the shell copies `pulse` to `<userData>/bin/pulse-<version>` (once per version, removing older copies) and starts the daemon from there.

## Authentication

The shell is a local client of the same user, like the CLI: it reads `$PULSE_HOME/token` and, once the port has proven it knows the token, connects to `/ws` with the token as the WebSocket subprotocol `pulse.token.<token>` (what browsers do; Node's built-in WebSocket is used, no dependency). The proof request and the WebSocket are separate connections (Node's WebSocket cannot reuse one); the threat model says what that leaves open. The token is never put in a URL, a log line, or a crash report (no crash reporter is started).

The **window** signs in the way a browser does ([0004](0004-daemon-and-config.md)): the shell runs `pulse open --print --json` to get a fresh single-use, two-minute login code (over a pipe: the code is in no process's arguments; `pulse open` checks the daemon's proof itself first), loads `http://127.0.0.1:<port>/<view>#login=<code>` into the window (not a process argument either), and the page exchanges the code at `POST /auth/login`. Asking the binary keeps the code format in one place. A code is minted for every load the shell starts; the page keeps the token in memory and a reload signs in with an instance-bound resume code, so its own navigation needs none. Logs show URLs without their fragment (`redact`).

## Window

- Loads only the daemon's origin. Size, position and maximized state are saved in `<userData>/settings.json` and restored onto a display that still exists (shrunk to fit; centred otherwise).
- **Closing the window** hides it to the tray where a tray is known to be visible: always on macOS (the menu bar), and on Linux on desktops whose panels show StatusNotifierItem icons out of the box (KDE, Xfce, Cinnamon, MATE, LXQt, Budgie, Ubuntu's GNOME session, and others; `settings.ts`). Electron cannot tell whether a Linux tray icon is actually shown, and hiding the only window behind an icon nobody can see would strand an invisible process. So **on other Linux desktops (plain GNOME) closing the window quits the app**; the daemon keeps running, and launching the app again brings the window back. A menu checkbox, "Keep running in the tray when the window closes", overrides the guess either way.
- The first time the window hides, one notification says Pulse is still in the tray.
- `--hidden` starts with no window (for an autostart entry).

## Tray

Status at a glance (running, paused, starting, daemon not running or failed to start, approvals waiting), **Pause everything / Resume**, the runs waiting for approval (clicking one opens it), recent runs with their state (clicking opens the run), Open Pulse, Start the daemon (when there is none), Restart the daemon with this version (older daemon), Notifications on/off, the close-to-tray checkbox (Linux), Install command line tool, Quit Pulse, Quit Pulse and stop the daemon. The window's menu bar (the app menu on macOS) carries the same items, so everything also works where no tray shows.

Cost: on connect, one `system.status`, two `runs.list`, one `rules.list` and a `runs.subscribe`; afterwards the shell only reacts to pushes (`state.changed`, `runs.changed`). The menu is rebuilt only when its content changes. With nothing happening, nothing runs: no timer, no poll.

On macOS the icon is a template image (tinted by the system) with the number of waiting approvals as its title; on Linux a dimmed icon means paused or no daemon. All icons are rendered at build time from `assets/icon.svg` (`scripts/icons.mjs`).

## Notifications

Raised for what a person would want to be interrupted for: a run **waiting for approval**, a run that **failed or timed out**, a rule **held by its circuit breaker**, and the **daemon going away** or failing to start. Not for runs that start or succeed, runs the user cancelled or rejected, runs interrupted by a shutdown, or anything already so when the shell connected. Each (run, state) is announced once (bounded memory of 512).

- **Rate limit**: a token bucket, three back to back, then one per twenty seconds. What does not fit is counted and announced later as one summary ("12 more updates", opening the approvals view if any were approvals). A timer exists only while something is held back.
- **Off switch**: "Notifications" in the menu; stored in the shell's settings.
- **Click** opens the run, the rule, or the approvals list in the window.
- **Untrusted text**: the title holds the rule name, the body the agent and, fetched with `events.get` only for a notice that passed the limiter, the event's subject. Everything is reduced to one line, stripped of control and invisible formatting characters (the list of [0003](0003-threat-model.md) §1, tag characters included; `text.ts`), and bounded (80 characters title, 180 + 120 body). On Linux, `&`, `<` and `>` are escaped because notification servers may interpret markup in the body.
- **Circuit breaker**: the daemon now pushes `state.changed` with the `rules` topic when a breaker trips (`pulse-daemon/src/engine.rs`, the only Rust change for the shell), and the shell compares `circuit_open_until` from `rules.list`. Before, a client could only learn of a trip by polling.
- **Not notified (gap):** a single delivery blocked by the loop guard (`loop_self_trigger`, `loop_chain_depth`). There is no push for deliveries; adding one is a contract change. The circuit breaker, which is what actually stops a looping rule, is notified.

## Security

The renderer shows untrusted content (file names, commit messages, run output).

- `contextIsolation: true`, `nodeIntegration: false` (also in workers and subframes), `sandbox: true` and `app.enableSandbox()`, `webSecurity: true`, no `<webview>`, no insecure content, spellcheck off (it downloads dictionaries on Linux), DevTools only in development builds (`PULSE_DESKTOP_DEVTOOLS=1` re-enables them).
- **Navigation** is locked to the daemon's origin (`http://127.0.0.1:<port>`, exact; not `localhost`, not another port), for the main frame (`will-navigate`, `will-redirect`) and subframes (`will-frame-navigate`). Anything else is cancelled. Before a daemon is known, nothing is allowed.
- **New windows** are always denied. A link to the UI opens in the window; an `http`/`https` link goes to the system browser; every other scheme (`file:`, `javascript:`, custom app handlers that would start local programs) is dropped, as are links with credentials (`https://bank@evil.example`).
- **Permissions** are denied by default, for requests and checks; the only one allowed is `clipboard-sanitized-write` from the daemon's origin (copy buttons). Device permissions are denied; downloads are cancelled.
- **Preload**: one frozen object, `window.pulseDesktop = { version: 1, platform, revealPath(path) }` (`bridge.ts`). `revealPath` shows a file in the file manager, selected, never opened; the main process accepts it only from the daemon's page and only for an absolute path that exists. `ipcRenderer` is not exposed. The web UI can use the bridge where present and must work without it.
- If Chromium runs with `--no-sandbox` (see Packaging: the AppImage launcher adds it where user namespaces are blocked), the menus say so.
- **Electron fuses** set in the packaged binary: `RunAsNode` off, `NODE_OPTIONS` and `--inspect` arguments ignored, ASAR integrity validation on, the app loads only from its ASAR, cookie encryption on, no extra `file:` privileges.
- The status page shown without a daemon is a `data:` URL with a CSP that allows nothing but inline style, and escaped text without invisible formatting characters.
- The UI the window loads carries the daemon's own Content-Security-Policy ([0004](0004-daemon-and-config.md)), which refuses frames before the shell's navigation lock sees them; the e2e harness accepts either layer stopping a frame.
- Packaging removes group- and world-write permission from every file in the app (`scripts/after-pack.cjs`): Electron's Linux archives can carry such modes, and electron-builder copies them into the AppImage.

Not a safety boundary: anything running as the same user can read the token file, start the app with Chromium debugging flags, or replace the app. That is the threat model's "same user is trusted" ([0003](0003-threat-model.md)).

## Packaging

- **electron-builder** (`apps/desktop/electron-builder.yml`): Linux AppImage and macOS dmg, for x64 and arm64, each built on its own platform and architecture (the bundled `pulse` is native). The app has no runtime `node_modules`: main and preload are two esbuild bundles. The `pulse` binary is in `resources/bin/pulse`, the same binary as the headless install. Alternatives (Electron Forge, hand-rolled packaging) offered nothing this needs that electron-builder lacks, and electron-builder does AppImage, dmg, fuses and icon conversion in one tool.
- **Version** of the app = `pulse --version` of the bundled binary.
- **Names.** The product name, Lictory Pulse, is what a person reads: `productName` (the macOS bundle `Lictory Pulse.app`, the Linux launcher entry), the window title, the menus, the tray tooltip, notification titles and `app.setName`. Artifact files are `Lictory-Pulse-<version>-linux-<arch>.AppImage` and `Lictory-Pulse-<version>-mac-<arch>.dmg`: no spaces, so they work unquoted in a shell. The app id is `com.lictory.pulse`, reverse DNS under the owner's `lictory.com`; macOS keys preferences and notification permission by it, so it must not change after the first released package. Identifiers keep `pulse`: the bundled CLI, the Linux executable `pulse-desktop` and its `pulse-desktop.desktop` file (`desktopName`, which also sets the window's WM_CLASS), and the `[pulse-desktop]` log prefix the end-to-end harness reads.
- **macOS signing**: an ad-hoc signature only (`identity: "-"`), because Apple Silicon refuses to run unsigned code at all. There is no Developer ID signature and no notarization, so Gatekeeper blocks the first open. **Workaround**, after dragging Lictory Pulse to Applications: open it once, then System Settings → Privacy & Security → "Open Anyway"; or in a terminal `xattr -dr com.apple.quarantine "/Applications/Lictory Pulse.app"`. Install by dragging to Applications: run from the mounted dmg, a daemon the app starts would run from the dmg.
- **AppImage runtime**: the upstream static runtime (`toolsets.appimage: "1.0.3"`, a beta option of electron-builder 26). The default runtime links `libfuse.so.2`, which Ubuntu 22.04 and later do not install, and its arm64 build links the unversioned `libz.so`, which only zlib's development package provides: on this project's arm64 test machine the default AppImage did not even start (`--appimage-extract` included). The static runtime needs neither. Being a beta of electron-builder's, it is the first thing to revisit if AppImages misbehave.
- **Linux sandbox**: Chromium's OS-level sandbox needs either a setuid helper (impossible inside an AppImage) or unprivileged user namespaces. Ubuntu 23.10 and later restrict those with AppArmor (`kernel.apparmor_restrict_unprivileged_userns=1`). electron-builder's AppImage launcher (`AppRun`) probes with `unshare -Ur true` and, when that fails, **starts the app with `--no-sandbox`**. So on a default Ubuntu 24.04 the window runs without the OS-level sandbox; context isolation, `sandbox: true` (no Node in the renderer or preload), the navigation lock, the permission policy and the bridge's checks still hold, but a renderer exploit is not contained by the OS. The app says so: the tray and window menus show "Running without the Chromium sandbox (--no-sandbox)", and the log records it. Restoring the sandbox needs an AppArmor profile that allows user namespaces for the app (root, once): a `.deb` with that profile is the proper fix and a v1 gap.
- **CLI on PATH**: "Install command line tool…" links `~/.local/bin/pulse` to the app's binary (inside an AppImage, to the stable copy described above; re-run after updating the app). It never overwrites a `pulse` that is not such a link, and says how to add `~/.local/bin` to PATH if it is missing. By hand: `ln -s "/Applications/Lictory Pulse.app/Contents/Resources/bin/pulse" /usr/local/bin/pulse` on macOS (may need `sudo`), or for an AppImage, `./Lictory-Pulse-*.AppImage --appimage-extract` and link `squashfs-root/resources/bin/pulse`.

## The build command

`./scripts/build.sh` (or `pnpm build`) from the repository root: `pnpm install --frozen-lockfile`, build `apps/web`, build `pulse` in release mode with the UI embedded, then package the app for this machine. `--no-app` stops after the binary. Prerequisites: Rust (stable) with a C compiler, Node 22 or later, pnpm (the version in `package.json`; `corepack enable`). The binary alone still needs only `cargo build --release -p pulse-cli`.

rust-embed reads `apps/web/dist` when `pulse-daemon` compiles but cargo does not notice new files there, so the script recompiles `pulse-daemon` (`cargo clean --release -p pulse-daemon`) before the release build. Without that, a binary built before the UI keeps serving the placeholder.

CI (`.github/workflows/desktop.yml`) runs the shell's typecheck and unit tests, and on four runners (Linux x64 and arm64, macOS arm64 and x64) runs the same script and uploads the binary and the package.

## Known gaps in v1

- **No code signing or notarization** (macOS: Gatekeeper workaround above); **no auto-update**. Both need accounts and a release process; electron-builder supports both when they exist.
- On Ubuntu 23.10 and later (AppArmor user-namespace restriction) the AppImage runs without Chromium's OS-level sandbox, and says so (see Packaging). A `.deb` with an AppArmor profile would fix it.
- No Windows build (Pulse does not support Windows in v1).
- Loop-guard blocks of single deliveries are not notified (see Notifications).
- The tray's look and behaviour on real desktops, real notification delivery, and everything on macOS were not verified by running them. What was verified, on a headless Linux arm64 machine under Xvfb, is driven by `apps/desktop/e2e/harness.ts` (`e2e/run.sh`): lifecycle (start, adopt, stale `daemon.json` naming a port another program holds, which is asked for a proof and never sent the token, older daemon, daemon stopped from outside, restart), the window signing in by itself with the login code (the page exchanges it, removes it from the address bar, stores no token, and stays signed in across a reload), pause and resume through the menu, notifications and their throttle, the navigation and permission policy, close-to-tray and the tray-less quit, a second instance, and the quit policy.

### Running the end-to-end scenarios on a machine without GTK

Electron's binary is not unpacked by `pnpm install` here (only esbuild's install script is allowed); `node apps/desktop/node_modules/electron/install.js` unpacks it from `~/.cache/electron`. On a headless Ubuntu 24.04 arm64 machine without GTK, twelve packages unpacked without root are enough (`apt-get download` each, then `dpkg -x` into one directory): `libgtk-3-0t64 libcairo-gobject2 libepoxy0 libgdk-pixbuf-2.0-0 libpangocairo-1.0-0 libpangoft2-1.0-0 libwayland-client0 libwayland-cursor0 libwayland-egl1 libxcursor1 libxinerama1 libjpeg-turbo8`. Then:

```sh
E2E_LD_LIBRARY_PATH=<dir>/usr/lib/aarch64-linux-gnu E2E_ELECTRON_FLAGS=--no-sandbox \
  PULSE_BIN=target/debug/pulse apps/desktop/e2e/run.sh <out-dir>
```

`PULSE_E2E=showcase` (not in `run.sh`'s list) adopts a daemon already running on a Pulse home with activity and captures the feed, in the theme `PULSE_E2E_THEME` sets (`light` or `dark`). `pnpm --dir apps/web docs:images` runs it for `docs/images/desktop-*.png` ([0008](0008-web-ui.md#documentation-images)).
