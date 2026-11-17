import { expect, test, type Page } from "@playwright/test";

// A placeholder must not be mistakable for content: an empty Cooldown field showing "30s" read
// as a cooldown of 30 s, most of all in the dark theme. Each placeholder in the rule, agent and
// source editors says what it is ("e.g. 30s", "default 15m", or a description), is dimmer than
// a value, and is still as legible as the hint text under the fields.

interface Seen {
  text: string;
  placeholder: number;
  value: number;
  hint: number;
}

async function placeholders(page: Page): Promise<Seen[]> {
  return page.evaluate(() => {
    const rgb = (c: string) => (c.match(/[\d.]+/g) ?? []).slice(0, 4).map(Number);
    const lum = ([r = 0, g = 0, b = 0]: number[]) => {
      const f = (v: number) => ((v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
      return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
    };
    // The colour actually painted behind an element: the nearest opaque background.
    const behind = (el: Element): number[] => {
      for (let e: Element | null = el; e; e = e.parentElement) {
        const c = rgb(getComputedStyle(e).backgroundColor);
        if (c.length === 3 || (c[3] ?? 0) > 0.99) return c;
      }
      return [255, 255, 255];
    };
    const contrast = (fg: number[], bg: number[]) => {
      const [a, b] = [lum(fg), lum(bg)].sort((x, y) => y - x) as [number, number];
      return (a + 0.05) / (b + 0.05);
    };
    const hintEl = document.querySelector(".editor .field-hint");
    const hint = hintEl ? contrast(rgb(getComputedStyle(hintEl).color), behind(hintEl)) : 0;
    return [...document.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>(".editor input[placeholder], .editor textarea[placeholder]")]
      .filter((el) => el.checkVisibility() && el.placeholder !== "")
      .map((el) => {
        const bg = behind(el);
        return {
          text: el.placeholder,
          placeholder: contrast(rgb(getComputedStyle(el, "::placeholder").color), bg),
          value: contrast(rgb(getComputedStyle(el).color), bg),
          hint,
        };
      });
  });
}

/** An example, a default, or a description; never a bare value. */
const WORDED = /^(e\.g\. |default[: ])|\(optional\)$|, then Enter$/;

test("editor placeholders read as examples, never as values, in both themes", async ({ page }) => {
  const problems: string[] = [];
  const check = async (where: string) => {
    for (const p of await placeholders(page)) {
      if (!WORDED.test(p.text)) problems.push(`${where}: "${p.text}" reads like a value`);
      if (p.placeholder > p.value / 2) problems.push(`${where}: "${p.text}" is nearly as strong as a value (${p.placeholder.toFixed(2)} vs ${p.value.toFixed(2)})`);
      if (p.placeholder + 0.005 < p.hint) problems.push(`${where}: "${p.text}" is fainter than hint text (${p.placeholder.toFixed(2)} vs ${p.hint.toFixed(2)})`);
    }
  };
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/rules/new?trickle=0");
    await page.getByRole("button", { name: "Add condition" }).click();
    await check(`${scheme} rule`);
    await page.getByLabel("Operator").selectOption({ index: 1 });
    await check(`${scheme} rule, list condition`);

    await page.goto("/agents/new?trickle=0");
    for (const adapter of ["claude-code", "codex", "command"]) {
      await page.getByLabel("Adapter").selectOption(adapter);
      await check(`${scheme} agent ${adapter}`);
    }
    await page.getByRole("button", { name: "Add variable" }).click();
    await check(`${scheme} agent env`);

    await page.goto("/sources/new?trickle=0");
    for (const kind of ["fs", "git", "schedule", "calendar"]) {
      await page.getByLabel("Kind").selectOption(kind);
      await check(`${scheme} source ${kind}`);
    }
    await page.getByLabel("From").selectOption("path");
    await check(`${scheme} source calendar file`);
  }
  expect(problems).toEqual([]);
});
