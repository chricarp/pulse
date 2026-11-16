// Every string the app renders into the DOM as text passes through `visible()` here, so text
// from events, runs, file names, config files and sources cannot reorder or hide what is around
// it, in any view, without each view remembering to ask. The app's JSX compiles to this module
// (`jsxImportSource` in vite.config.ts and tsconfig.json), which hands React the same element
// with string children and text attributes of DOM elements made visible. Values the user edits
// (`value`, `defaultValue`) and everything given to components are left as they are; what a
// component finally renders into a DOM element is cleaned there. Not covered: text set outside
// JSX (`document.title`, which `Shell.tsx` builds from numbers only) and what is copied to the
// clipboard (the real value, on purpose).

import { visible } from "../lib/format";

const TEXT_ATTRIBUTES = ["title", "aria-label", "aria-description", "alt", "placeholder"] as const;

function children(c: unknown): unknown {
  if (typeof c === "string") return visible(c);
  if (!Array.isArray(c)) return c;
  let out: unknown[] | null = null;
  for (let i = 0; i < c.length; i++) {
    const v: unknown = c[i];
    const n = children(v);
    if (n !== v) {
      out ??= c.slice();
      out[i] = n;
    }
  }
  return out ?? c;
}

/** `props` with its text made visible, for a DOM element; anything else is left alone. */
export function cleanProps<P>(type: unknown, props: P): P {
  if (typeof type !== "string" || props === null || typeof props !== "object") return props;
  const p = props as Record<string, unknown>;
  let out: Record<string, unknown> | null = null;
  const c = children(p.children);
  if (c !== p.children) out = { ...p, children: c };
  for (const name of TEXT_ATTRIBUTES) {
    const v = p[name];
    if (typeof v === "string") {
      const n = visible(v);
      if (n !== v) {
        out ??= { ...p };
        out[name] = n;
      }
    }
  }
  return (out ?? p) as P;
}
