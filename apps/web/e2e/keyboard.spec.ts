import { expect, test } from "@playwright/test";

const mod = process.platform === "darwin" ? "Meta" : "Control";

test("navigate, pause and resume, and approve from the keyboard", async ({ page }) => {
  await page.goto("/feed?trickle=0");
  await expect(page.getByRole("heading", { name: "Feed" })).toBeVisible();

  await page.keyboard.press("g");
  await page.keyboard.press("r");
  await expect(page.getByRole("heading", { name: "Rules" })).toBeVisible();
  await page.keyboard.press("j");
  await expect(page.locator(".rule-row.is-selected")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await expect(page.locator(".rule-row.is-selected")).toHaveCount(0);

  // Pause everything, from anywhere; its state is visible.
  await page.keyboard.press("Shift+P");
  await expect(page.locator(".banner").filter({ hasText: "Paused." })).toBeVisible();
  await expect(page.locator(".pause-control")).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Shift+P");
  await expect(page.locator(".banner").filter({ hasText: "Paused." })).toHaveCount(0);

  // The pending approval is one shortcut away, and approved with another.
  await expect(page.locator(".banner-approval")).toBeVisible();
  await page.keyboard.press("g");
  await page.keyboard.press("i");
  await expect(page.locator(".approval")).toBeVisible();
  await page.keyboard.press("Shift+A");
  await expect(page.locator(".toast").filter({ hasText: "Approved" })).toBeVisible();
  await expect(page.locator(".banner-approval")).toHaveCount(0);
});

test("the command palette finds views, rules and actions", async ({ page }) => {
  await page.goto("/feed?trickle=0");
  await expect(page.locator(".event-row").first()).toBeVisible();
  await page.keyboard.press(`${mod}+k`);
  const input = page.getByRole("combobox", { name: "Search commands" });
  await expect(input).toBeFocused();
  await input.fill("sync-n");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/rules\/sync-notes/);
  await expect(page.locator(".detail-title")).toHaveText("sync-notes");

  await page.keyboard.press(`${mod}+k`);
  await input.fill("new rule");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: "New rule" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: "Rules" })).toBeVisible();
});

test("the filter is one key away and its query syntax becomes chips", async ({ page }) => {
  await page.goto("/feed?trickle=0");
  await expect(page.locator(".event-row").first()).toBeVisible();
  await page.keyboard.press("/");
  const q = page.locator("#feed-query");
  await expect(q).toBeFocused();
  await q.fill("type:git.* source:pulse-repo");
  await page.keyboard.press("Enter");
  await expect(page.locator(".filter-chips")).toContainText("type: git.*");
  await expect(page).toHaveURL(/type=git\.\*/);
  await expect(page.locator(".event-row .c-type").first()).toHaveText(/^git\./);
});

test("keyboard shortcuts are discoverable", async ({ page }) => {
  await page.goto("/runs?trickle=0");
  await expect(page.locator(".run-row").first()).toBeVisible();
  await page.keyboard.press("?");
  const dialog = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("Pause or resume everything");
  await expect(dialog).toContainText("Next run");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
});

test("a rule draft is tested against a past event before it is saved", async ({ page }) => {
  await page.goto("/rules/new?trickle=0");
  await page.locator("#rule-name").fill("draft-check");
  const on = page.locator("#rule-on");
  await on.fill("fs.file.created");
  await on.press("Enter");
  await page.locator("#rule-agent").selectOption("study");
  await page.locator("#rule-prompt").fill("Look at {{subject}}");
  await page.locator(".pick-event").first().click();
  await page.keyboard.press(`${mod}+Enter`);
  await expect(page.locator(".decision")).toContainText("fired");
  await expect(page.locator(".prompt-instruction")).toContainText("Look at /home/ada/");
  await expect(page.locator(".prompt-data")).toContainText("Untrusted event data");
});
