import { createServer } from "node:net";

import { defineConfig, devices } from "@playwright/test";

// A free port on loopback, chosen once by the runner (other builds share this machine). The
// config is loaded again in each worker; they inherit the runner's environment.
const port = process.env.PULSE_E2E_PORT ? Number(process.env.PULSE_E2E_PORT) : await new Promise<number>((resolve, reject) => {
  const s = createServer();
  s.once("error", reject);
  s.listen(0, "127.0.0.1", () => {
    const addr = s.address();
    s.close(() => resolve(typeof addr === "object" && addr ? addr.port : 0));
  });
});

process.env.PULSE_E2E_PORT = String(port);

const vite = "node node_modules/vite/bin/vite.js";

export default defineConfig({
  testDir: "e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  expect: { timeout: 15_000 },
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    viewport: { width: 1440, height: 900 },
    trace: "off",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } } }],
  webServer: {
    // The production build of the app with the mock daemon compiled in (`--mode mock`).
    command: `${vite} build --mode mock --outDir dist-mock --logLevel warn && ${vite} preview --outDir dist-mock --host 127.0.0.1 --port ${port} --strictPort`,
    url: `http://127.0.0.1:${port}/`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
