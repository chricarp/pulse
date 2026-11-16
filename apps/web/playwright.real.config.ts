import { defineConfig, devices } from "@playwright/test";

// The production build served by the real daemon binary. Each test starts its own daemon on a
// temp home and port 0 (e2e-real/daemon.ts), so there is no web server here. Build first:
//   pnpm build && cargo build -p pulse-cli    (the daemon embeds apps/web/dist)
export default defineConfig({
  testDir: "e2e-real",
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  expect: { timeout: 15_000 },
  reporter: [["list"]],
  use: { viewport: { width: 1440, height: 900 }, trace: "off" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } } }],
});
