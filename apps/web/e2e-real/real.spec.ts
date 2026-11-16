// The production UI against the real daemon binary: login, live feed, live run output,
// approvals, config write-back, and reconnecting after a restart. Each test gets its own daemon
// on a temp home (see daemon.ts). Tests wait for conditions, never for time.

import { writeFileSync } from "node:fs";
import { createServer, type IncomingMessage } from "node:http";
import { join } from "node:path";
import type { Duplex } from "node:stream";

import { expect, test as base, type Page } from "@playwright/test";

import { startDaemon, type Daemon } from "./daemon";

const test = base.extend<{ daemon: Daemon }>({
  // oxlint-disable-next-line no-empty-pattern -- Playwright fixtures take their dependencies by destructuring.
  daemon: async ({}, use) => {
    const d = await startDaemon();
    await use(d);
    await d.dispose();
  },
  // Every page runs under the daemon's Content-Security-Policy; anything it blocks fails the test.
  page: async ({ page }, use) => {
    const blocked: string[] = [];
    page.on("console", (m) => {
      if (/Content Security Policy/i.test(m.text())) blocked.push(m.text());
    });
    await use(page);
    expect(blocked).toEqual([]);
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
  // The token lives in memory only: nothing stored holds it.
  const stored = () => page.evaluate(() => JSON.stringify({ ...sessionStorage }) + JSON.stringify({ ...localStorage }));
  expect(await stored()).not.toContain(daemon.token());
  await page.reload();
  await expect(page.locator(".conn-line")).toContainText("Connected");
  expect(await stored()).not.toContain("pulse.resume");

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

test("a program that takes the port gets no credential, on reconnect or on reload", async ({ page, daemon }) => {
  await login(page, daemon);
  const token = daemon.token();
  const port = daemon.port;
  await daemon.stop();
  // It records everything, fakes proofs, and serves a page that reads this origin's storage.
  const seen: string[] = [];
  const squatter = createServer((req, res) => {
    seen.push(`${req.url ?? ""} ${JSON.stringify(req.headers)}`);
    if (req.url?.startsWith("/auth/proof")) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ instance: "00".repeat(16), proof: "00".repeat(32) }));
      return;
    }
    let body = "";
    req.on("data", (d: Buffer) => (body += d.toString()));
    req.on("end", () => {
      seen.push(body);
      res.writeHead(200, { "content-type": "text/html" });
      res.end("<!doctype html><title>x</title><script>document.title = 'stolen:' + JSON.stringify({ ...sessionStorage })</script>");
    });
  });
  squatter.on("upgrade", (req: IncomingMessage, socket: Duplex) => {
    seen.push(`${req.url ?? ""} ${JSON.stringify(req.headers)}`);
    socket.destroy();
  });
  await new Promise<void>((r) => squatter.listen(port, "127.0.0.1", r));

  // Reconnecting: the proof fails, the token is not sent, and the window says why.
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.locator(".banner[data-unproven]")).toContainText("could not prove it is your daemon");

  // Reloading: the squatter's page runs on this origin and reads what the real page stored.
  await page.reload();
  await expect(page).toHaveTitle(/^stolen:/);
  const stolen = JSON.parse((await page.title()).slice("stolen:".length)) as Record<string, string>;
  expect(JSON.stringify(stolen)).not.toContain(token);
  const resume = JSON.parse(stolen["pulse.resume"] ?? "null") as { code: string } | null;
  expect(resume?.code).toMatch(/^r\.[0-9a-f]{64}$/);
  expect(seen.join("\n")).toContain("/auth/proof?nonce=");
  expect(seen.join("\n")).not.toContain(token);

  // What it read works on no daemon: this one restarted, as any daemon must for it to get the port.
  squatter.closeAllConnections();
  await new Promise((r) => squatter.close(r));
  await daemon.start();
  const res = await fetch(`http://127.0.0.1:${port}/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json", origin: `http://127.0.0.1:${port}` },
    body: JSON.stringify({ code: resume?.code }),
  });
  expect(res.status).toBe(401);
});

test("the UI is served with a strict Content-Security-Policy", async ({ page, daemon }) => {
  const res = await fetch(`http://127.0.0.1:${daemon.port}/runs`);
  const csp = res.headers.get("content-security-policy") ?? "";
  for (const d of ["default-src 'none'", "script-src 'self'", "object-src 'none'", "base-uri 'none'", "frame-ancestors 'none'"]) expect(csp).toContain(d);
  expect(csp).not.toContain("unsafe");
  expect(res.headers.get("cache-control")).toBe("no-store");
  expect(res.headers.get("x-content-type-options")).toBe("nosniff");
  // And the app works under it (the page fixture fails the test on any violation).
  await login(page, daemon);
  writeFileSync(join(daemon.home, "Inbox", "csp.txt"), "x");
  await expect(page.locator(".event-row", { hasText: "csp.txt" })).toBeVisible();
});
