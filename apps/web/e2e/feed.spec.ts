import { expect, test } from "@playwright/test";

import { mock, scrollToGap, startPerf, stopPerf, summarize } from "./helpers";

test("the feed stays virtualized and responsive through a storm of 10,000 events", async ({ page }) => {
  await page.goto("/feed?trickle=0");
  await expect(page.locator(".event-row").first()).toBeVisible();
  await startPerf(page);
  await mock(page).storm(10_000, 3);
  // The daemon summarizes: batches of at most 200 plus a count of what was left out.
  await expect(page.getByText(/summarized/)).toBeVisible();
  // Wait until every storm event is accounted for (shown or summarized).
  await expect
    .poll(async () => {
      const text = (await page.locator(".feed-status").innerText()).replace(/,/g, "");
      const shown = Number(/(\d+) events shown/.exec(text)?.[1] ?? 0);
      const summarized = Number(/(\d+) summarized/.exec(text)?.[1] ?? 0);
      return shown + summarized;
    })
    .toBeGreaterThanOrEqual(10_000);
  const perf = await stopPerf(page);
  test.info().annotations.push({ type: "storm", description: summarize(perf) });
  console.log(`storm: ${summarize(perf)}`);

  // Virtualized: only the visible rows exist in the DOM.
  expect(await page.locator(".event-row").count()).toBeLessThan(80);
  // Below a batch that dropped events, a gap row says what was left out of the live view.
  await scrollToGap(page);
  await expect(page.locator(".gap-row").first()).toContainText("not shown live");
  await page.locator(".feed-list").evaluate((el) => el.scrollTo(0, 0));
  // Still responsive: keyboard selection opens the detail.
  await page.locator(".feed-list").focus();
  await page.keyboard.press("j");
  await expect(page.locator(".detail")).toBeVisible();
  // No single task blocked the page for long.
  expect(Math.max(0, ...perf.longTasks)).toBeLessThan(400);
});

test("a gap row opens the summarized range from the log", async ({ page }) => {
  await page.goto("/feed?trickle=0");
  await expect(page.locator(".event-row").first()).toBeVisible();
  await mock(page).storm(3000, 1);
  await expect(page.getByText(/summarized/)).toBeVisible();
  await scrollToGap(page);
  const gap = page.locator(".gap-row").first();
  await expect(gap).toBeVisible();
  await gap.getByRole("button", { name: "Show from the log" }).click();
  await expect(page.getByText("From the log")).toBeVisible();
  await expect(page).toHaveURL(/until=/);
  await page.getByRole("button", { name: "Back to live" }).click();
  await expect(page.locator(".live-dot.is-live")).toBeVisible();
});

test("scrolling away holds the live tail and says how much arrived", async ({ page }) => {
  await page.goto("/feed?trickle=0");
  const list = page.locator(".feed-list");
  await expect(page.locator(".event-row").first()).toBeVisible();
  await list.evaluate((el) => el.scrollTo(0, 600));
  await expect(page.getByText("Scrolled away from the newest")).toBeVisible();
  await mock(page).storm(500, 0.5);
  await expect(page.locator(".feed-frozen")).toContainText("new");
  await page.locator(".feed-frozen").click();
  await expect(page.locator(".feed-frozen")).toHaveCount(0);
  await expect.poll(() => list.evaluate((el) => el.scrollTop)).toBe(0);
});

test("a dropped connection is marked in the feed and recovered", async ({ page }) => {
  await page.goto("/feed?trickle=0");
  await expect(page.locator(".live-dot.is-live")).toBeVisible();
  await mock(page).dropConnection();
  await expect(page.locator(".live-dot.is-live")).toBeVisible();
  // History reached back past the gap, so nothing is marked missing.
  await expect(page.locator(".gap-row.is-lost")).toHaveCount(0);
});

test("a path cut to fit keeps its file name's end and shows where it was cut, at every width", async ({ page }) => {
  // In the desktop window and beside the detail pane, names were clipped at the cell's edge
  // with no ellipsis (`~/week-07-shortest-paths.p`): the file name never shrank.
  const clipped = () =>
    page.evaluate(() =>
      [...document.querySelectorAll<HTMLElement>(".event-row .trunc")].flatMap((t) => {
        const box = t.getBoundingClientRect();
        const cell = (t.closest("[role=cell]") ?? t).getBoundingClientRect();
        const end = t.querySelector<HTMLElement>(".trunc-end")?.getBoundingClientRect();
        const fits = t.scrollWidth <= t.clientWidth + 1 && box.right <= cell.right + 1 && (!end || end.right <= box.right + 1);
        return fits ? [] : [t.title];
      }),
    );
  // 1100 is the desktop window's default width.
  for (const width of [1440, 1100, 900]) {
    await page.setViewportSize({ width, height: 800 });
    await page.goto("/feed?trickle=0");
    await expect(page.locator(".event-row").first()).toBeVisible();
    expect(await clipped(), `at ${width}px`).toEqual([]);
    await page.locator(".event-row").first().click();
    await expect(page.locator(".detail")).toBeVisible();
    expect(await clipped(), `at ${width}px beside the detail`).toEqual([]);
  }
});
