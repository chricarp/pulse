// The daemon token arrives in the URL fragment (`http://127.0.0.1:7477/#token=…`), which the
// browser never sends to a server. It is removed from the address bar before anything else runs,
// then kept in sessionStorage so a reload of this tab keeps working. It is never logged.

const KEY = "pulse.token";

export function parseFragmentToken(hash: string): { token: string | null; rest: string } {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  if (!raw) return { token: null, rest: "" };
  const params = new URLSearchParams(raw);
  const token = params.get("token");
  params.delete("token");
  const rest = params.toString();
  return { token: token && token.length > 0 ? token : null, rest: rest ? `#${rest}` : "" };
}

/** Takes the token from the fragment (scrubbing it) or from this tab's storage. */
export function takeToken(): string | null {
  const { token, rest } = parseFragmentToken(window.location.hash);
  if (token) {
    const url = window.location.pathname + window.location.search + rest;
    window.history.replaceState(window.history.state, "", url);
    try {
      window.sessionStorage.setItem(KEY, token);
    } catch {
      // Storage disabled: the token lives for this page load only.
    }
    return token;
  }
  try {
    return window.sessionStorage.getItem(KEY);
  } catch {
    return null;
  }
}

export function forgetToken(): void {
  try {
    window.sessionStorage.removeItem(KEY);
  } catch {
    // Nothing stored.
  }
}
