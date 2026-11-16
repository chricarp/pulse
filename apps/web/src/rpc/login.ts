// How this window gets the daemon token. The whole handshake is specified in
// `pulse_contracts::auth`; the constants come from the generated contracts.
//
// 1. `pulse open` (or the desktop shell) opens `/#login=<code>`: a single-use code that expires
//    in two minutes. The fragment never reaches a server; it is removed from the address bar
//    before anything else runs, so it is not in history either.
// 2. The code is exchanged at `POST /auth/login` for the token, which this page keeps in memory
//    only. Before every WebSocket attempt, the daemon must prove it knows the token
//    (`proof.ts`); something else listening on the port is never sent it.
// 3. A reload must not depend on anything a program that took the port could use: such a
//    program serves its own page on this origin, which can read everything this page stored.
//    So nothing is stored while the page runs. When it unloads, it stores a resume code that
//    only the daemon instance this tab was connected to accepts; the next load takes it out of
//    storage at once, checks the daemon's proof for it, and exchanges it for the token. After
//    the daemon restarts, or on another port, it is worthless.
// 4. A WebSocket closed with 4401 means the token is no longer accepted (the secret was
//    rotated): nothing is kept, and the window asks for `pulse open` instead of retrying.
//
// The token and the codes are never logged and never put in a URL.

import { LOGIN_FRAGMENT_PARAM, LOGIN_PATH, RESUME_STORAGE_KEY, type UiLoginResponse } from "../contracts";
import { type ProofDeps, requestProof, resumeCodeFor } from "./proof";

/** Why this window has no token. Each has its own explanation in the UI. */
export type NoAccessReason =
  /** Opened without a login code and nothing to resume. */
  | "no-code"
  /** The code was refused: expired, already used, or from another Pulse home. */
  | "code-refused"
  /** A reload, but the daemon has restarted since (or another one answers): resume codes die with it. */
  | "resume-stale"
  /** Something answered on the port without proving it knows this tab's credential. Nothing was sent. */
  | "unproven"
  /** The daemon could not be reached to exchange the code. */
  | "unreachable"
  /** The daemon closed the connection with 4401: the token is no longer valid. */
  | "token-rejected";

export type Access = { kind: "token"; token: string } | { kind: "none"; reason: NoAccessReason; detail?: string };

/** What a reload carries over: a resume code and the daemon instance it is for. */
export interface Resume {
  instance: string;
  code: string;
}

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

function storage(): Storage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

/** Takes the resume code the previous page of this tab left, and removes it from storage. */
export function takeResume(store: Storage | null = storage()): Resume | null {
  let raw: string | null = null;
  try {
    raw = store?.getItem(RESUME_STORAGE_KEY) ?? null;
    store?.removeItem(RESUME_STORAGE_KEY);
    // Earlier builds kept the token itself here; a tab reloaded since must not keep it.
    store?.removeItem("pulse.token");
  } catch {
    return null;
  }
  if (raw === null) return null;
  try {
    const v = JSON.parse(raw) as Partial<Resume>;
    return typeof v.instance === "string" && typeof v.code === "string" ? { instance: v.instance, code: v.code } : null;
  } catch {
    return null;
  }
}

async function exchange(code: string, fetchImpl: typeof fetch): Promise<Access | "refused"> {
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
    return { kind: "none", reason: "unreachable" };
  }
  if (!res.ok) return "refused";
  const body = (await res.json()) as UiLoginResponse;
  return { kind: "token", token: body.token };
}

/**
 * Gets the token: from a login code (a fresh `pulse open`), else from a resume code (a reload),
 * after the daemon has proven it knows the resume code. A login code needs no such proof: the
 * page that holds it was served by that same port a moment ago, and `pulse open` checked the
 * daemon's proof before opening it.
 */
export async function login(code: string | null, resume: Resume | null, deps: ProofDeps): Promise<Access> {
  const fetchImpl = deps.fetch ?? fetch;
  if (code !== null) {
    const got = await exchange(code, fetchImpl);
    // The same link opened again in this tab: its code is spent, but the tab can resume.
    if (got !== "refused" && (got.kind === "token" || resume === null)) return got;
    if (resume === null) return { kind: "none", reason: "code-refused" };
  }
  if (resume === null) return { kind: "none", reason: "no-code" };
  const proof = await requestProof(resume.code, deps, { resume: true });
  if (proof.kind === "unreachable") return { kind: "none", reason: "unreachable" };
  if (proof.kind === "unproven") {
    // A restarted daemon names a new instance and cannot prove the old one's code. Something
    // that names the old instance and still cannot prove it is not that daemon.
    const restarted = proof.instance !== undefined && proof.instance !== resume.instance;
    return restarted ? { kind: "none", reason: "resume-stale" } : { kind: "none", reason: "unproven", detail: proof.why };
  }
  const got = await exchange(resume.code, fetchImpl);
  return got === "refused" ? { kind: "none", reason: "resume-stale" } : got;
}

/**
 * Keeps what a reload of this tab needs, in memory, and writes it to `sessionStorage` only when
 * the page unloads. Only on 127.0.0.1: the daemon listens there and nowhere else, and on
 * `localhost` a browser may reach a program listening on `[::1]` with the same port instead.
 */
export class ReloadKeeper {
  private resume: Resume | null = null;
  private pending: Promise<void> = Promise.resolve();

  constructor(
    private readonly token: string,
    private readonly store: Storage | null = storage(),
    page: Pick<Window, "addEventListener"> | null = window.location.hostname === "127.0.0.1" ? window : null,
  ) {
    page?.addEventListener("pagehide", () => this.write());
    // Back from the back/forward cache: the page is running again, so nothing stays stored.
    page?.addEventListener("pageshow", (e) => {
      if (e.persisted) takeResume(this.store);
    });
  }

  /** A daemon instance proved it knows the token. */
  connected(instance: string): void {
    if (this.resume?.instance === instance) return;
    this.pending = resumeCodeFor(this.token, instance).then((code) => {
      this.resume = { instance, code };
    });
  }

  /** The token is no longer accepted: a reload must not try it. */
  forget(): void {
    this.resume = null;
    this.pending = Promise.resolve();
  }

  /** For tests: resolves when the latest resume code is computed. */
  settled(): Promise<void> {
    return this.pending;
  }

  write(): void {
    if (this.resume === null) return;
    try {
      this.store?.setItem(RESUME_STORAGE_KEY, JSON.stringify(this.resume));
    } catch {
      // Storage disabled: a reload needs `pulse open`.
    }
  }
}
