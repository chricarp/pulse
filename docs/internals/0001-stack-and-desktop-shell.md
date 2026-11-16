# 0001: Stack and desktop shell

Status: decided, 2026-11-16. Revisit when GPUI reaches 1.0.

## Decision

- **Daemon and CLI: Rust.** One binary, `pulse`. `pulse start` runs the daemon; every other subcommand is a client of it.
- **UI: a React + TypeScript web app**, built with Vite, embedded in the `pulse` binary and served by the daemon on its local port.
- **Desktop: a thin Electron shell** around that same UI. It owns the window, the tray, notifications, and starting the daemon if it is not running. It contains no product logic.
- **Boundary: one typed protocol.** JSON-RPC 2.0 over a WebSocket on `127.0.0.1`, authenticated by a token. The CLI, the web UI, and the Electron shell all use it. Types are defined once in Rust (`pulse-contracts`) and generated for TypeScript.

## Why not GPUI

GPUI is the better fit for the *feel* of a tray utility: one language, low memory, fast. We did not pick it for v1 because:

1. **It would be the only client.** A GPUI app does not give us a browser client. With the web UI served by the daemon, "remote-ready" is a routing and auth problem later, not a second UI.
2. **Pre-1.0 churn lands on the wrong part of the product.** Pulse's value is the engine: sources, coalescing, routing, permissions, loop protection. A framework that breaks its API regularly would spend maintenance time on the least differentiated layer.
3. **Fork fragmentation.** Several GPUI distributions exist whose types cannot be mixed. Contributors would have to know which one we track.
4. **Tray, auto-update and packaging** are solved in Electron and still do-it-yourself with GPUI.
5. **Build requirements.** GPUI on Linux needs system development headers (xcb, xkbcommon, Wayland, Vulkan, fontconfig). A plain `cargo build` of the engine should need nothing but a C compiler, so the UI must not be a dependency of the daemon either way.

## Why not a webview shell (Tauri and similar)

It keeps the web UI and drops Chromium's weight, which is attractive. But on Linux it renders with WebKitGTK, so we would maintain the UI against two engines (WebKit in the app, Chromium/Firefox in the browser client), and the list virtualization and scroll behaviour we depend on is exactly where they differ. It also needs WebKitGTK development packages at build time. We can revisit this: because the shell holds no logic, swapping Electron for another shell is a small, contained change.

## What this costs, honestly

- Electron is heavy: on the order of 150–300 MB of RAM while the window or tray is alive. The mitigation is structural, not cosmetic: **the daemon does not need the app.** Quit the app and Pulse keeps watching, routing and waking agents at Rust-daemon cost. The app is something you open to look, not something that must stay resident.
- Two languages and a generated-types step.
- The tray lives in the Electron shell, so tray presence costs Electron's memory. A native tray helper in the daemon is a possible later change.

## Consequences that must hold

- The daemon runs headless and is fully usable from the CLI. Nothing the UI can do is impossible from the CLI or the config file.
- The Electron renderer loads the UI **from the daemon** (`http://127.0.0.1:<port>/`). There is one UI build and one origin, not a second `file://` copy.
- `cargo build` works without Node. If the web UI has not been built, the binary serves a placeholder page that says how to build it.
- The shell never talks to SQLite, never reads the config file, and never spawns agents.
