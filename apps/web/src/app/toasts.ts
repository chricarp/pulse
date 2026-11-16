// Short confirmations and failures of actions. Each disappears after a while on a timer.

import { createStore } from "../store/store";

export interface Toast {
  id: number;
  tone: "ok" | "error" | "info";
  message: string;
  detail?: string;
}

export const toasts = createStore<Toast[]>([]);
let next = 1;

export function toast(message: string, tone: Toast["tone"] = "ok", detail?: string): void {
  const id = next++;
  const t: Toast = detail === undefined ? { id, tone, message } : { id, tone, message, detail };
  toasts.set((list) => [...list.slice(-3), t]);
  setTimeout(() => dismiss(id), tone === "error" ? 9000 : 4000);
}

export function dismiss(id: number): void {
  toasts.set((list) => list.filter((t) => t.id !== id));
}

/** Runs an action and reports its failure; returns the result or `undefined`. */
export async function attempt<T>(what: string, action: () => Promise<T>, success?: string): Promise<T | undefined> {
  try {
    const r = await action();
    if (success) toast(success, "ok");
    return r;
  } catch (err) {
    toast(`${what} failed`, "error", err instanceof Error ? err.message : String(err));
    return undefined;
  }
}
