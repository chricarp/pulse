import { defineConfig } from "@playwright/test";

// The documentation images (docs/images/), made by e2e-real/docs-images.ts against the real
// daemon. Not a test: `pnpm docs:images`. Build first, as for `test:real`:
//   pnpm build && cargo clean -p pulse-daemon && cargo build -p pulse-cli
export default defineConfig({
  testDir: "e2e-real",
  testMatch: "docs-images.ts",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 300_000,
  expect: { timeout: 30_000 },
  reporter: [["list"]],
  use: { trace: "off" },
});
