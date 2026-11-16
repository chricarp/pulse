// URL policy for the window: what it may navigate to, what may leave for the system browser,
// and the paths of the views the shell opens.

/** The daemon's origin. The shell always uses 127.0.0.1, never `localhost`, so there is one. */
export function daemonOrigin(port: number): string {
  return `http://127.0.0.1:${port}`;
}

function parse(url: string): URL | null {
  if (typeof url !== "string" || url.length > 8192) return null;
  try {
    return new URL(url);
  } catch {
    return null;
  }
}

/** Whether `url` belongs to the daemon's UI. Anything else must not load in the window. */
export function isDaemonUrl(url: string, origin: string | null): boolean {
  if (origin === null) return false;
  const u = parse(url);
  return u !== null && u.protocol === "http:" && u.origin === origin;
}

/**
 * A link that may be handed to the system browser, or `null`. Only http and https: other schemes
 * (`file:`, `javascript:`, custom app handlers) would let text in an event start a local program.
 * Links with credentials are refused because they are a classic way to disguise the real host.
 */
export function externalUrl(url: string): string | null {
  const u = parse(url);
  if (u === null || (u.protocol !== "http:" && u.protocol !== "https:")) return null;
  if (u.username !== "" || u.password !== "" || u.hostname === "") return null;
  const s = u.toString();
  return s.length <= 2048 ? s : null;
}

/** Views the shell opens from the tray and from notifications. */
export type Route =
  | { kind: "home" }
  | { kind: "run"; id: string }
  | { kind: "approvals" }
  | { kind: "rule"; name: string };

/**
 * The web UI's path for a view. These paths are the contract between the shell and the UI
 * (see docs/internals/0010-desktop-shell.md); the daemon serves `index.html` for all of them.
 */
export function routePath(route: Route): string {
  switch (route.kind) {
    case "home":
      return "/";
    case "run":
      return `/runs/${encodeURIComponent(route.id)}`;
    case "approvals":
      return "/runs?show=approval";
    case "rule":
      return `/rules/${encodeURIComponent(route.name)}`;
  }
}

/** The URL that opens `route` and signs the page in with a single-use login code. */
export function loginUrl(origin: string, route: Route, code: string): string {
  return `${origin}${routePath(route)}#login=${encodeURIComponent(code)}`;
}

/** The login code from `pulse open --print --json`'s URL, if it is for `origin`. */
export function loginCodeFrom(url: string, origin: string): string | null {
  const u = parse(url);
  if (u === null || u.origin !== origin) return null;
  const code = new URLSearchParams(u.hash.replace(/^#/, "")).get("login");
  return code !== null && /^[0-9]+\.[0-9a-f]+$/.test(code) ? code : null;
}

/** A URL with its fragment removed, for logs: the fragment carries the login code. */
export function redact(url: string): string {
  const u = parse(url);
  if (u === null) return "<unparseable url>";
  if (u.protocol === "http:" || u.protocol === "https:") return `${u.origin}${u.pathname}`;
  return `${u.protocol}…`;
}
