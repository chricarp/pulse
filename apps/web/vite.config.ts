import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// In development the app talks to a real daemon through this proxy, so the bundle never
// contains an origin: `PULSE_DAEMON=http://127.0.0.1:7477 pnpm dev`. `pnpm dev:mock` uses the
// in-browser mock daemon instead and needs no daemon at all.
const daemon = process.env.PULSE_DAEMON ?? "http://127.0.0.1:7477";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    proxy: {
      // The login exchange (`pulse open` codes); the daemon checks `Origin` here too.
      "/auth": {
        target: daemon,
        changeOrigin: true,
        configure: (proxy) => {
          proxy.on("proxyReq", (req) => req.setHeader("origin", daemon));
        },
      },
      "/ws": {
        target: daemon,
        ws: true,
        changeOrigin: true,
        // The daemon checks `Origin` against its own address (DNS rebinding, threat model §4).
        // Through the proxy the browser's origin is Vite's, so present the daemon's own.
        configure: (proxy) => {
          proxy.on("proxyReqWs", (req) => req.setHeader("origin", daemon));
        },
      },
    },
  },
  preview: { host: "127.0.0.1" },
  build: {
    outDir: "dist",
    target: "es2022",
    sourcemap: false,
    chunkSizeWarningLimit: 600,
  },
  test: {
    include: ["src/**/*.test.ts"],
    environment: "node",
  },
});
