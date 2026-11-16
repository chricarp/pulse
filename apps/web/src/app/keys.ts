// Keybindings: one window listener, a registry of bindings, two-key sequences (`g f`).
// Single keys and sequences are ignored while typing in a field; `mod+…` and Escape are not.

import { useEffect, useRef } from "react";

export interface Binding {
  /** `mod+k`, `shift+p`, `g f`, `/`, `?`, `j`, `escape`, `mod+enter`. */
  keys: string;
  label: string;
  group: "Navigation" | "Actions" | "Lists" | "Feed" | "Runs" | "Rules" | "Editor";
  run: (e: KeyboardEvent) => void;
  /** Also fire while focus is in a text field. Implied for `mod+` combinations. */
  inInputs?: boolean;
  /** Listed in the help and palette. */
  hidden?: boolean;
}

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

const registry = new Set<Binding[]>();
const listeners = new Set<() => void>();
let pendingPrefix: string | null = null;
let prefixTimer: ReturnType<typeof setTimeout> | null = null;

export function allBindings(): Binding[] {
  const seen = new Map<string, Binding>();
  for (const list of registry) for (const b of list) if (!b.hidden) seen.set(`${b.group}:${b.keys}`, b);
  return [...seen.values()];
}

export function onBindingsChanged(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

function keyName(e: KeyboardEvent): string {
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key.toLowerCase();
  const parts: string[] = [];
  if (isMac ? e.metaKey : e.ctrlKey) parts.push("mod");
  if (e.altKey) parts.push("alt");
  // Shift is part of the character for symbols (`?`), but named for letters (`shift+p`).
  if (e.shiftKey && /^[a-z]$/.test(k)) parts.push("shift");
  parts.push(k === " " ? "space" : k);
  return parts.join("+");
}

function inField(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  if (tag === "TEXTAREA" || tag === "SELECT") return true;
  if (tag === "INPUT") {
    const type = (target as HTMLInputElement).type;
    return !["checkbox", "radio", "button", "submit", "range"].includes(type);
  }
  return false;
}

function onKeyDown(e: KeyboardEvent): void {
  if (e.defaultPrevented || e.isComposing) return;
  const name = keyName(e);
  const typing = inField(e.target);
  const lists = [...registry].reverse(); // Latest registered (innermost view) wins.
  const candidates = pendingPrefix ? `${pendingPrefix} ${name}` : name;
  for (const list of lists) {
    for (const b of list) {
      if (b.keys !== candidates) continue;
      if (typing && !b.inInputs && !b.keys.startsWith("mod+") && b.keys !== "escape") continue;
      clearPrefix();
      e.preventDefault();
      b.run(e);
      return;
    }
  }
  if (pendingPrefix) {
    clearPrefix();
    return;
  }
  if (typing || name.includes("+")) return;
  // Start a sequence if some binding begins with this key.
  for (const list of lists) {
    for (const b of list) {
      if (b.keys.startsWith(`${name} `)) {
        pendingPrefix = name;
        prefixTimer = setTimeout(clearPrefix, 1200);
        e.preventDefault();
        return;
      }
    }
  }
}

function clearPrefix(): void {
  pendingPrefix = null;
  if (prefixTimer) clearTimeout(prefixTimer);
  prefixTimer = null;
}

if (typeof window !== "undefined") window.addEventListener("keydown", onKeyDown);

/** Registers bindings for the lifetime of the component. The latest `run` is always called. */
export function useBindings(bindings: Binding[]): void {
  const ref = useRef(bindings);
  ref.current = bindings;
  const signature = bindings.map((b) => `${b.keys}|${b.label}|${b.hidden ? 1 : 0}`).join(";");
  useEffect(() => {
    const list: Binding[] = ref.current.map((b, i) => ({
      ...b,
      run: (e: KeyboardEvent) => ref.current[i]?.run(e),
    }));
    registry.add(list);
    for (const l of listeners) l();
    return () => {
      registry.delete(list);
      for (const l of listeners) l();
    };
  }, [signature]);
}

/** `mod+shift+p` → `Ctrl Shift P` (or `⌘⇧P` on a Mac). */
export function formatKeys(keys: string): string[] {
  return keys.split(" ").map((stroke) =>
    stroke
      .split("+")
      .map((p) => {
        switch (p) {
          case "mod":
            return isMac ? "⌘" : "Ctrl";
          case "shift":
            return isMac ? "⇧" : "Shift";
          case "alt":
            return isMac ? "⌥" : "Alt";
          case "enter":
            return "Enter";
          case "escape":
            return "Esc";
          case "arrowup":
            return "↑";
          case "arrowdown":
            return "↓";
          default:
            return p.length === 1 ? p.toUpperCase() : p;
        }
      })
      .join(isMac ? "" : " "),
  );
}
