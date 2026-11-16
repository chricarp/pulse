import type { Page } from "@playwright/test";

export interface Perf {
  longTasks: number[];
  frames: number[];
}

/** Starts recording long tasks and frame intervals in the page. */
export async function startPerf(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as { __perf: { longTasks: number[]; frames: number[]; stop: boolean } };
    w.__perf = { longTasks: [], frames: [], stop: false };
    new PerformanceObserver((list) => {
      for (const e of list.getEntries()) w.__perf.longTasks.push(e.duration);
    }).observe({ type: "longtask", buffered: false });
    let last = performance.now();
    const tick = (t: number) => {
      w.__perf.frames.push(t - last);
      last = t;
      if (!w.__perf.stop) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

export async function stopPerf(page: Page): Promise<Perf> {
  return page.evaluate(() => {
    const w = window as unknown as { __perf: { longTasks: number[]; frames: number[]; stop: boolean } };
    w.__perf.stop = true;
    return { longTasks: w.__perf.longTasks, frames: w.__perf.frames };
  });
}

export function summarize(p: Perf): string {
  const sorted = [...p.frames].sort((a, b) => a - b);
  const q = (x: number) => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * x))] ?? 0;
  const lt = p.longTasks;
  return [
    `frames=${p.frames.length}`,
    `frame p50=${q(0.5).toFixed(1)}ms p95=${q(0.95).toFixed(1)}ms p99=${q(0.99).toFixed(1)}ms max=${(sorted.at(-1) ?? 0).toFixed(1)}ms`,
    `longtasks=${lt.length} total=${lt.reduce((a, b) => a + b, 0).toFixed(0)}ms max=${Math.max(0, ...lt).toFixed(0)}ms`,
  ].join(" | ");
}

export function mock(page: Page) {
  return {
    storm: (total: number, seconds: number) => page.evaluate(([t, s]) => (window as unknown as { __pulseMock: { storm(t: number, s: number): void } }).__pulseMock.storm(t, s), [total, seconds] as const),
    bigRun: (bytes?: number) => page.evaluate((b) => (window as unknown as { __pulseMock: { bigRun(b?: number): string } }).__pulseMock.bigRun(b), bytes),
    dropConnection: () => page.evaluate(() => (window as unknown as { __pulseMock: { dropConnection(): void } }).__pulseMock.dropConnection()),
  };
}
