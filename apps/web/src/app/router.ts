// A small router over the History API. Paths are the app's navigation state; everything after
// `?` is view state (filters). The daemon serves index.html for every non-asset path.

import { useSyncExternalStore } from "react";

export type Route =
  | { view: "feed"; eventId: string | null }
  | { view: "runs"; runId: string | null }
  | { view: "rules"; name: string | null; mode: "show" | "edit" | "new" }
  | { view: "agents"; name: string | null; mode: "show" | "edit" | "new" }
  | { view: "sources"; name: string | null; mode: "show" | "edit" | "new" }
  | { view: "projects" }
  | { view: "system" };

export type ViewName = Route["view"];

const listeners = new Set<() => void>();
let current = { path: window.location.pathname, search: window.location.search };

function emit(): void {
  current = { path: window.location.pathname, search: window.location.search };
  for (const l of listeners) l();
}

window.addEventListener("popstate", emit);

export function parseRoute(path: string): Route {
  const parts = path.split("/").filter(Boolean).map((p) => {
    try {
      return decodeURIComponent(p);
    } catch {
      return p;
    }
  });
  const [head, a, b] = parts;
  switch (head) {
    case "runs":
      return { view: "runs", runId: a ?? null };
    case "rules":
    case "agents":
    case "sources": {
      if (a === "new" && b === undefined) return { view: head, name: null, mode: "new" };
      // Names may hold a slash (`thesis/build-on-change`), encoded as %2F in one segment.
      return { view: head, name: a ?? null, mode: b === "edit" ? "edit" : "show" };
    }
    case "projects":
      return { view: "projects" };
    case "system":
      return { view: "system" };
    case "feed":
    default:
      return { view: "feed", eventId: head === "feed" ? (a ?? null) : null };
  }
}

export function href(route: Route, search = ""): string {
  const enc = encodeURIComponent;
  let path: string;
  switch (route.view) {
    case "feed":
      path = route.eventId ? `/feed/${enc(route.eventId)}` : "/feed";
      break;
    case "runs":
      path = route.runId ? `/runs/${enc(route.runId)}` : "/runs";
      break;
    case "rules":
    case "agents":
    case "sources":
      path =
        route.mode === "new"
          ? `/${route.view}/new`
          : route.name
            ? `/${route.view}/${enc(route.name)}${route.mode === "edit" ? "/edit" : ""}`
            : `/${route.view}`;
      break;
    default:
      path = `/${route.view}`;
  }
  return path + search;
}

export function navigate(to: Route | string, opts: { replace?: boolean; keepSearch?: boolean } = {}): void {
  let url = typeof to === "string" ? to : href(to);
  if (opts.keepSearch && !url.includes("?")) url += window.location.search;
  if (url === window.location.pathname + window.location.search) return;
  if (opts.replace) window.history.replaceState(null, "", url);
  else window.history.pushState(null, "", url);
  emit();
}

/** Replaces the query string without a history entry (filters as you type). */
export function setSearch(search: URLSearchParams): void {
  const s = search.toString();
  const url = window.location.pathname + (s ? `?${s}` : "");
  if (url === window.location.pathname + window.location.search) return;
  window.history.replaceState(null, "", url);
  emit();
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function useLocation(): { path: string; search: string } {
  return useSyncExternalStore(subscribe, () => current);
}

let lastPath = "";
let lastRoute: Route = { view: "feed", eventId: null };

/** The same object while the path is the same, so it can sit in dependency lists. */
export function useRoute(): Route {
  const { path } = useLocation();
  if (path !== lastPath) {
    lastPath = path;
    lastRoute = parseRoute(path);
  }
  return lastRoute;
}
