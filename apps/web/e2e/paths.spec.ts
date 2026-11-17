import { expect, test, type Page } from "@playwright/test";

// Every path the UI shows under the user's home reads as `~/…` (the absolute path stays in
// titles and copy buttons). The mock's home is /home/ada. Event data, file text and run output
// are shown as they are and are left out, as are field values the user edits.
const HOME = "/home/ada/";

async function absolutePaths(page: Page): Promise<string[]> {
  return page.evaluate((home) => {
    const skip = "pre, .output-scroll, input, textarea, select";
    const found: string[] = [];
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const el = n.parentElement;
      if (!el || el.closest(skip) || !el.checkVisibility()) continue;
      const t = n.textContent ?? "";
      if (t.includes(home)) found.push(t.trim().slice(0, 160));
    }
    for (const o of document.querySelectorAll("option")) if (o.textContent?.includes(home)) found.push(o.textContent);
    return found;
  }, HOME);
}

test("paths under the home read as ~/… in every view", async ({ page }) => {
  const check = async (where: string) => expect(await absolutePaths(page), where).toEqual([]);

  await page.goto("/feed?trickle=0");
  await page.locator(".event-row").first().click();
  await expect(page.locator(".detail")).toBeVisible();
  await check("feed and event detail");
  await page.getByRole("button", { name: "this directory" }).click();
  await check("feed filtered by a directory");

  await page.goto("/runs?trickle=0");
  await page.getByRole("button", { name: "Review" }).click();
  await expect(page.locator(".approval")).toBeVisible();
  await check("approval card");
  await page.getByRole("tab", { name: "Invocation" }).click();
  await check("run invocation");

  const views: [string, string][] = [
    ["/rules/new-lecture", "Owned by"],
    ["/rules/new-lecture/edit", "Saved in"],
    ["/rules/new", "Saved in"],
    ["/agents/fixer", "Defined in"],
    ["/agents/fixer/edit", "Program"],
    [`/agents/${encodeURIComponent("thesis/builder")}`, "Defined in"],
    ["/sources/university", "Defined in"],
    ["/sources/university/edit", "Directory"],
    ["/system", "Pulse home"],
  ];
  for (const [path, text] of views) {
    await page.goto(`${path}?trickle=0`);
    await expect(page.getByText(text, { exact: true }).first()).toBeVisible();
    await check(path);
  }
  await page.goto("/rules/new-lecture?trickle=0");
  await page.getByRole("button", { name: /^Delete/ }).click();
  await check("removing a rule");

  await page.goto("/projects?trickle=0");
  await check("projects");
  await page.locator(".project", { hasText: "dotfiles" }).first().getByRole("button", { name: "Review and trust again" }).click();
  await expect(page.getByRole("dialog")).toContainText("Trusting loads what");
  await check("trust review");
});
