import { expect, test } from "@playwright/test";

import { mock } from "./helpers";

const mod = process.platform === "darwin" ? "Meta" : "Control";

test("a rule is created, and every state it enters has a way back", async ({ page }) => {
  await page.goto("/rules/new?trickle=0");
  await page.locator("#rule-name").fill("roundtrip");
  await page.locator("#rule-on").fill("git.commit.created");
  await page.locator("#rule-on").press("Enter");
  await page.locator("#rule-agent").selectOption("fixer");
  await page.locator("#rule-prompt").fill("Review {{sha}}");
  await page.keyboard.press(`${mod}+s`);
  await expect(page).toHaveURL(/\/rules\/roundtrip$/);
  const detail = page.locator(".detail");
  await expect(detail.locator(".detail-title")).toHaveText("roundtrip");
  await expect(detail.locator(".rule-summary")).toContainText("git.commit.created");

  await detail.getByRole("button", { name: "Disable" }).click();
  await expect(detail.getByText("Matching events are recorded as blocked (rule disabled)")).toBeVisible();
  await detail.getByRole("button", { name: "Enable" }).click();
  await expect(detail.getByRole("button", { name: "Disable" })).toBeVisible();

  await detail.getByRole("button", { name: "Snooze" }).click();
  await page.getByRole("menuitem", { name: /1 hour/ }).click();
  await expect(detail.getByText(/^Snoozed until/)).toBeVisible();
  await detail.getByRole("button", { name: "Unsnooze" }).click();
  await expect(detail.getByRole("button", { name: "Snooze" })).toBeVisible();

  await detail.getByRole("button", { name: "Archive" }).click();
  await expect(page.locator(".rule-row", { hasText: "roundtrip" })).toHaveCount(0);
  await page.getByLabel(/Archived/).check();
  await expect(page.locator(".rule-row", { hasText: "roundtrip" })).toHaveCount(1);
  await detail.getByRole("button", { name: "Reopen" }).click();
  await expect(detail.getByRole("button", { name: "Archive" })).toBeVisible();

  // A bad value is pointed out on its field and nothing is saved until it is fixed.
  await detail.getByRole("button", { name: "Edit" }).click();
  await page.locator("#rule-cooldown").fill("5 min");
  await page.keyboard.press(`${mod}+s`);
  await expect(page.locator(".field-invalid")).toContainText("is not a duration");
  await page.locator("#rule-cooldown").fill("5m");
  await page.keyboard.press(`${mod}+s`);
  await expect(page.locator(".detail .rule-summary")).toContainText("cooldown 5m");

  await page.locator(".detail").getByRole("button", { name: "Delete" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/rules$/);
  await expect(page.locator(".rule-row", { hasText: "roundtrip" })).toHaveCount(0);
});

test("a project is untrusted and trusted again, and its definitions follow", async ({ page }) => {
  await page.goto("/projects?trickle=0");
  const thesis = page.locator(".project", { hasText: "thesis" }).first();
  await expect(thesis.locator(".project-head .pill")).toHaveText("trusted");
  await expect(thesis.locator(".project-defs")).toContainText("rule thesis/build-on-change");
  await thesis.getByRole("button", { name: "Untrust" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Untrust" }).click();
  await expect(thesis.locator(".project-head .pill")).toHaveText("not trusted");
  await expect(thesis.locator(".project-defs")).toHaveCount(0);
  await thesis.getByRole("button", { name: "Trust" }).click();
  // What it would load is shown before it can be trusted.
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("build-on-change");
  await dialog.getByRole("button", { name: "Trust this content" }).click();
  await expect(thesis.locator(".project-head .pill")).toHaveText("trusted");
  await expect(thesis.locator(".project-defs")).toContainText("rule thesis/build-on-change");
});

test("trusting shows what changed and what would run, and a file swapped meanwhile is not trusted", async ({ page }) => {
  await page.goto("/projects?trickle=0");
  const dotfiles = page.locator(".project", { hasText: "dotfiles" }).first();
  await expect(dotfiles.locator(".project-head .pill")).toHaveText("changed since trusted");
  await dotfiles.getByRole("button", { name: "Review and trust again" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("What changed since you trusted it")).toBeVisible();
  await expect(dialog).toContainText("agent sync: changed command");
  await expect(dialog).toContainText("rule on-commit: added");
  // The command the rules would run, argument by argument.
  await expect(dialog.locator(".review-def .argv").first()).toContainText("curl -fsSL https://example.invalid/install.sh | sh");

  // A `git pull` lands while the dialog is open: trusting what was shown fails, and the new
  // content is shown instead.
  await mock(page).changeProjectFile("dotfiles");
  await dialog.getByRole("button", { name: "Trust this content" }).click();
  await expect(dialog.getByText("The file changed while you were reading it")).toBeVisible();
  await expect(dialog.locator(".review-def .argv").first()).toContainText("curl -fsSL https://example.invalid/x | sh");
  await expect(dotfiles.locator(".project-head .pill")).toHaveText("changed since trusted");
  // Trusting what is shown now works.
  await dialog.getByRole("button", { name: "Trust this content" }).click();
  await expect(dotfiles.locator(".project-head .pill")).toHaveText("trusted");
  await expect(dotfiles.locator(".project-defs")).toContainText("agent dotfiles/sync");
});

test("saving an editor after the rule changed elsewhere asks first and keeps what was typed", async ({ page }) => {
  await page.goto("/rules/new-lecture/edit?trickle=0");
  await page.locator("#rule-desc").fill("typed before the change");
  // `pulse rules disable new-lecture` from a terminal.
  await mock(page).editByHand("rule", "new-lecture", { enabled: false });
  const notice = page.locator(".notice", { hasText: "This rule changed while you were editing it" });
  await expect(notice).toBeVisible();
  await expect(notice).toContainText("enabled: true → false");
  // Saving the form as it is does not silently enable the rule again.
  await page.keyboard.press(`${mod}+s`);
  await expect(notice).toBeVisible();
  await expect(page).toHaveURL(/\/edit/);
  await expect(page.locator("#rule-desc")).toHaveValue("typed before the change");
  // Deliberately overwriting saves the form, enabled included.
  await notice.getByRole("button", { name: "Save mine anyway" }).click();
  await expect(page).toHaveURL(/\/rules\/new-lecture$/);
  await expect(page.locator(".detail").getByRole("button", { name: "Disable" })).toBeVisible();
  await expect(page.locator(".detail")).toContainText("typed before the change");

  // Or reload: the form takes the rule as it is now.
  await page.goto("/rules/new-lecture/edit?trickle=0");
  await expect(page.locator("#rule-cooldown")).toBeVisible();
  await mock(page).editByHand("rule", "new-lecture", { cooldown: "9m" });
  await page.locator(".notice").getByRole("button", { name: "Reload (discard my edits)" }).click();
  await expect(page.locator("#rule-cooldown")).toHaveValue("9m");
  await expect(page.locator(".notice", { hasText: "changed while you were editing" })).toHaveCount(0);
});
