// Runs sandboxed, before the page, in an isolated world. It exposes one frozen object and keeps
// `ipcRenderer` to itself: the page can only send the one message below.

import { contextBridge, ipcRenderer } from "electron";

import { type PulseDesktopBridge, REVEAL_PATH_CHANNEL } from "./bridge.ts";

const bridge: PulseDesktopBridge = Object.freeze({
  version: 1,
  platform: process.platform,
  revealPath: (path: string): Promise<boolean> =>
    typeof path === "string" ? ipcRenderer.invoke(REVEAL_PATH_CHANNEL, path) : Promise.resolve(false),
});

contextBridge.exposeInMainWorld("pulseDesktop", bridge);
