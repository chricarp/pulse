import { expect, test } from "@playwright/test";

import { mock, startPerf, stopPerf, summarize } from "./helpers";

test("a run with megabytes of output streams, follows the tail, and scrolls back", async ({ page }) => {
  await page.goto("/runs?trickle=0");
  await expect(page.locator(".run-row").first()).toBeVisible();
  const id = await mock(page).bigRun(6 * 1024 * 1024);
  await startPerf(page);
  const t0 = Date.now();
  // In-app navigation, as a click would do (a reload would start a fresh mock).
  await page.evaluate((rid) => {
    window.history.pushState(null, "", `/runs/${rid}`);
    window.dispatchEvent(new PopStateEvent("popstate"));
  }, id);
  const scroll = page.locator(".output-scroll");
  await expect(page.locator(".output-bar")).toContainText(/[3-9]\d,\d{3} chunks/);
  const loaded = Date.now() - t0;
  // Following: pinned to the bottom while lines arrive.
  await expect.poll(() => scroll.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThan(40);
  const perf = await stopPerf(page);
  console.log(`big output load (${loaded}ms to all chunks): ${summarize(perf)}`);
  test.info().annotations.push({ type: "output-load", description: `${loaded}ms; ${summarize(perf)}` });
  expect(await page.locator(".vout").count()).toBeLessThan(200);

  // Scroll back: the tail stops following and offers to come back.
  await startPerf(page);
  await scroll.evaluate(async (el) => {
    for (let y = el.scrollHeight; y > 0; y -= 4000) {
      el.scrollTop = y;
      await new Promise(requestAnimationFrame);
    }
  });
  const scrollPerf = await stopPerf(page);
  console.log(`big output scroll: ${summarize(scrollPerf)}`);
  test.info().annotations.push({ type: "output-scroll", description: summarize(scrollPerf) });
  await expect(page.getByRole("button", { name: /Follow live output|Jump to the end/ })).toBeVisible();
  // The dropped middle is marked where it was: between the head and the tail of the output.
  const found = await scroll.evaluate(async (el) => {
    for (let y = el.scrollHeight * 0.3; y < el.scrollHeight * 0.7; y += el.clientHeight) {
      el.scrollTop = y;
      await new Promise(requestAnimationFrame);
      await new Promise(requestAnimationFrame);
      if (el.querySelector(".ch-marker")) return true;
    }
    return false;
  });
  expect(found).toBe(true);
  await expect(page.locator(".ch-marker").first()).toContainText("dropped");
  await page.keyboard.press("End");
  await expect.poll(() => scroll.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThan(40);
});

test("structured output reads differently from plain text", async ({ page }) => {
  await page.goto("/runs?show=done&agent=study&trickle=0");
  await page.locator(".run-row").first().click();
  await expect(page.locator(".ch-tool").first()).toBeVisible();
  await expect(page.locator(".ch-tool_result").first()).toBeVisible();
  await expect(page.locator(".ch-result").first()).toContainText("Result");
  await page.getByLabel("Tool calls").uncheck();
  await expect(page.locator(".ch-tool")).toHaveCount(0);
});
