// Light and dark follow the system; a manual override is a per-browser preference.

import { createStore, useStore } from "../store/store";

export type ThemeChoice = "system" | "light" | "dark";
const KEY = "pulse.theme";

function read(): ThemeChoice {
  try {
    const v = window.localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

const theme = createStore<ThemeChoice>(read());

function apply(choice: ThemeChoice): void {
  const root = document.documentElement;
  if (choice === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", choice);
}

apply(theme.get());

export function setTheme(choice: ThemeChoice): void {
  theme.set(choice);
  apply(choice);
  try {
    if (choice === "system") window.localStorage.removeItem(KEY);
    else window.localStorage.setItem(KEY, choice);
  } catch {
    // Not persisted.
  }
}

export function cycleTheme(): ThemeChoice {
  const order: ThemeChoice[] = ["system", "light", "dark"];
  const next = order[(order.indexOf(theme.get()) + 1) % order.length] ?? "system";
  setTheme(next);
  return next;
}

export function useTheme(): ThemeChoice {
  return useStore(theme, (t) => t);
}
