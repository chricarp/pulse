// How this window gets the daemon token. The whole handshake is specified in
// `pulse_contracts::auth`; the constants come from the generated contracts.
//
// 1. `pulse open` (or the desktop shell) opens `/#login=<code>`: a single-use code that expires
//    in two minutes. The fragment never reaches a server; it is removed from the address bar
//    before anything else runs, so it is not in history either.
// 2. The code is exchanged at `POST /auth/login` for the token, which is kept in this tab's
//    sessionStorage: a reload keeps working, a new tab needs `pulse open` again. The token is the
//    daemon secret, so it survives daemon restarts.
// 3. A WebSocket closed with 4401 means the stored token is no longer accepted (the secret was
//    rotated, or another Pulse home now owns the port): it is forgotten and the window asks for
//    `pulse open` instead of retrying.
//
// The token and the code are never logged and never put in a URL.

import { LOGIN_FRAGMENT_PARAM, LOGIN_PATH, TOKEN_STORAGE_KEY, type UiLoginResponse } from "../contracts";

/** Why this window has no token. Each has its own explanation in the UI. */
export type NoAccessReason =
  /** Opened without a login code and nothing stored for this tab. */
  | "no-code"
  /** The code was refused: expired, already used, or from another Pulse home. */
  | "code-refused"
  /** The daemon could not be reached to exchange the code. */
  | "unreachable"
  /** The daemon closed the connection with 4401: the stored token is no longer valid. */
  | "token-rejected";

export type Access = { kind: "token"; token: string } | { kind: "none"; reason: NoAccessReason };

export function parseLoginFragment(hash: string): { code: string | null; rest: string } {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!raw) return { code: null, rest: "" };
  const params = new URLSearchParams(raw);
  const code = params.get(LOGIN_FRAGMENT_PARAM);
  params.delete(LOGIN_FRAGMENT_PARAM);
  const rest = params.toString();
  return { code: code ? code : null, rest: rest ? `#${rest}` : "" };
}

/** Removes a login code from the address bar and returns it. Call before anything else runs. */
export function takeLoginCode(): string | null {
  const { code, rest } = parseLoginFragment(window.location.hash);
  if (code !== null) {
    window.history.replaceState(window.history.state, "", window.location.pathname + window.location.search + rest);
  }
  return code;
}

export function storedToken(): string | null {
  try {
    return window.sessionStorage.getItem(TOKEN_STORAGE_KEY);
  } catch {
    return null;
  }
}

function storeToken(token: string): void {
  try {
    window.sessionStorage.setItem(TOKEN_STORAGE_KEY, token);
  } catch {
    // Storage disabled: the token lives for this page load only.
  }
}

export function forgetToken(): void {
  try {
    window.sessionStorage.removeItem(TOKEN_STORAGE_KEY);
  } catch {
    // Nothing stored.
  }
}

/**
 * Exchanges a login code for the token. A refused code falls back to a token this tab already
 * holds (the same link opened twice in one tab); otherwise the window explains what to do.
 */
export async function login(code: string | null, fetchImpl: typeof fetch = fetch): Promise<Access> {
  const stored = storedToken();
  if (code === null) return stored ? { kind: "token", token: stored } : { kind: "none", reason: "no-code" };
  let res: Response;
  try {
    res = await fetchImpl(LOGIN_PATH, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code }),
      cache: "no-store",
      credentials: "omit",
    });
  } catch {
    return stored ? { kind: "token", token: stored } : { kind: "none", reason: "unreachable" };
  }
  if (!res.ok) return stored ? { kind: "token", token: stored } : { kind: "none", reason: "code-refused" };
  const body = (await res.json()) as UiLoginResponse;
  storeToken(body.token);
  return { kind: "token", token: body.token };
}
