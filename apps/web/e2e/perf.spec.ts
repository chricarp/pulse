// Measurements, with generous bounds so they catch regressions without flaking. The numbers
// are printed; docs/internals/0008-web-ui.md records a run.
//
// The mock daemon runs in the page, so its own work (routing every event, serializing frames)
// lands on the same main thread as the UI. The real daemon is another process; these numbers
// overstate the UI's cost.

import { expect, test } from "@playwright/test";

import { mock, startPerf, stopPerf, summarize } from "./helpers";

test("a 30,000-event storm, then scrolling 20,000 rows from the log", async ({ page }) => {
  test.setTimeout(180_000);
  await page.goto("/feed?trickle=0");
  await expect(page.locator(".event-row").first()).toBeVisible();

  await startPerf(page);
  await mock(page).storm(30_000, 3);
  await expect
    .poll(async () => {
      const text = (await page.locator(".feed-status").innerText()).replace(/,/g, "");
      return Number(/(\d+) events shown/.exec(text)?.[1] ?? 0) + Number(/(\d+) summarized/.exec(text)?.[1] ?? 0);
    }, { timeout: 30_000 })
    .toBeGreaterThanOrEqual(30_000);
  const storm = await stopPerf(page);
  console.log(`storm 30k: ${summarize(storm)}`);

  // The summarized events are behind gap rows; "Show from the log" pages them in. Open the
  // whole range from the log and scroll to the end until the view's bound (20,000) is reached.
  const until = new Date(Date.now() + 3_600_000).toISOString();
  await page.evaluate((u) => {
    window.history.pushState(null, "", `/feed?until=${encodeURIComponent(u)}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
  }, until);
  await expect(page.getByText("From the log")).toBeVisible();
  const list = page.locator(".feed-list");
  await expect
    .poll(async () => {
      await list.evaluate((el) => (el.scrollTop = el.scrollHeight));
      const text = (await page.locator(".feed-status").innerText()).replace(/,/g, "");
      return Number(/(\d+) events shown/.exec(text)?.[1] ?? 0);
    }, { timeout: 120_000, intervals: [50] })
    .toBeGreaterThanOrEqual(20_000);
  const rows = await list.evaluate((el) => Math.round(el.scrollHeight / 30));
  console.log(`feed rows: ${rows}`);

  await startPerf(page);
  await list.evaluate(async (el) => {
    for (let y = el.scrollHeight; y > 0; y -= 3000) {
      el.scrollTop = y;
      await new Promise(requestAnimationFrame);
    }
  });
  const scroll = await stopPerf(page);
  console.log(`feed scroll over ${rows} rows: ${summarize(scroll)}`);
  expect(await page.locator(".event-row").count()).toBeLessThan(80);
  expect(Math.max(0, ...scroll.longTasks)).toBeLessThan(200);
});
