// Storms against the real daemon: the UI must not freeze. These produce the measurements in
// docs/internals/0008-web-ui.md ("Against the real daemon"). The bounds asserted here are the
// definition of done: no main-thread task of LONG_TASK_BOUND or more while the storm arrives,
// and the feed answers a key within INTERACTIVE_BOUND, during and after the storm.

import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { expect, test as base, type Page } from "@playwright/test";

import { startPerf, stopPerf, summarize } from "../e2e/helpers";
import { pulseBin, startDaemon, type Daemon } from "./daemon";

/** A task this long is a visible stall. The UI has none at all on the development machine. */
const LONG_TASK_BOUND = 200;
/** From a key press to the selected event's detail on screen. */
const INTERACTIVE_BOUND = 1000;

const test = base.extend<{ daemon: Daemon }>({
  // oxlint-disable-next-line no-empty-pattern -- Playwright fixtures take their dependencies by destructuring.
  daemon: async ({}, use) => {
    const d = await startDaemon();
    await use(d);
    await d.dispose();
  },
});

async function openFeed(page: Page, daemon: Daemon): Promise<void> {
  await page.goto(daemon.loginUrl());
  await expect(page.locator(".conn-line")).toContainText("Connected");
  await page.goto(`http://127.0.0.1:${daemon.port}/feed`);
  await expect(page.locator(".live-dot.is-live")).toBeVisible();
}

/** The events the feed has accounted for: shown plus summarized. */
async function accounted(page: Page): Promise<number> {
  const text = (await page.locator(".feed-status").innerText()).replace(/,/g, "");
  return Number(/(\d+) events? shown/.exec(text)?.[1] ?? 0) + Number(/(\d+) summarized/.exec(text)?.[1] ?? 0);
}

/** Presses `j` in the feed and returns how long the detail took to show the next event. */
async function respondsToKeys(page: Page): Promise<number> {
  await page.locator(".feed-list").focus();
  const before = page.url();
  const t0 = Date.now();
  await page.keyboard.press("j");
  await expect(page.locator(".detail")).toBeVisible();
  await expect.poll(() => page.url()).not.toBe(before);
  return Date.now() - t0;
}

test("10,000 files unzipped into a watched folder", async ({ page, daemon }) => {
  await openFeed(page, daemon);
  await startPerf(page);
  const dir = join(daemon.home, "Inbox", "unzipped");
  mkdirSync(dir);
  for (let i = 0; i < 10_000; i++) writeFileSync(join(dir, `file-${String(i).padStart(5, "0")}.txt`), "x");
  // The fs source folds a storm in one directory into one event.
  const burst = page.locator(".event-row", { hasText: "fs.burst" }).first();
  await expect(burst).toBeVisible({ timeout: 30_000 });
  const perf = await stopPerf(page);
  const after = await respondsToKeys(page);
  const line = `real daemon, 10,000 files: ${summarize(perf)} | key after storm ${after}ms`;
  console.log(line);
  test.info().annotations.push({ type: "storm-files", description: line });
  expect(Math.max(0, ...perf.longTasks)).toBeLessThan(LONG_TASK_BOUND);
  expect(after).toBeLessThan(INTERACTIVE_BOUND);
  expect(await page.locator(".event-row").count()).toBeLessThan(80);
});

test("10,000 events through pulse ingest, the feed used while they arrive", async ({ page, daemon }) => {
  await openFeed(page, daemon);
  await startPerf(page);
  const lines = Array.from({ length: 10_000 }, (_, i) => JSON.stringify({ type: "storm.tick", subject: `tick ${i}` })).join("\n");
  const ingest = spawn(pulseBin, ["ingest", "-s", "load"], { env: { ...process.env, HOME: daemon.home, PULSE_HOME: daemon.pulseHome }, stdio: ["pipe", "ignore", "pipe"] });
  const done = new Promise<number | null>((resolve) => ingest.once("exit", resolve));
  ingest.stdin.end(`${lines}\n`);
  // While the storm arrives: the first batch is on screen and the feed answers keys.
  await expect(page.locator(".event-row", { hasText: "ext.load.storm.tick" }).first()).toBeVisible();
  const during = await respondsToKeys(page);
  expect(await done).toBe(0);
  await expect.poll(() => accounted(page), { timeout: 30_000 }).toBeGreaterThanOrEqual(10_000);
  const perf = await stopPerf(page);
  const line = `real daemon, 10,000 ingested: ${summarize(perf)} | key during storm ${during}ms`;
  console.log(line);
  test.info().annotations.push({ type: "storm-ingest", description: line });
  expect(Math.max(0, ...perf.longTasks)).toBeLessThan(LONG_TASK_BOUND);
  expect(during).toBeLessThan(INTERACTIVE_BOUND);
  await page.keyboard.press("Escape");
  expect(await respondsToKeys(page)).toBeLessThan(INTERACTIVE_BOUND);
  expect(await page.locator(".event-row").count()).toBeLessThan(80);
});
