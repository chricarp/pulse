// The daemon proves that it knows a credential before this page sends it anywhere. The port is
// fixed and anyone on the machine can listen on it while the daemon is stopped; without this,
// a reconnecting tab would hand the token to whatever answers. The computation is specified in
// `pulse_contracts::auth`; the daemon's (`pulse-daemon/src/auth.rs`) and the desktop shell's
// (`apps/desktop/src/proof.ts`) must agree with this one, and all three test the same vector.

import { type DaemonProof, PROOF_KEY_LABEL, PROOF_PATH, RESUME_CODE_PREFIX, RESUME_KEY_LABEL } from "../contracts";

const encoder = new TextEncoder();

async function hmac(key: Uint8Array, message: string): Promise<Uint8Array> {
  const k = await crypto.subtle.importKey("raw", key as BufferSource, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", k, encoder.encode(message)));
}

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** What the daemon on `port`, started as `instance`, answers for `credential` and `nonce`. */
export async function proofFor(credential: string, port: number, instance: string, nonce: string): Promise<string> {
  const key = await hmac(encoder.encode(credential), PROOF_KEY_LABEL);
  return hex(await hmac(key, `${port}\0${instance}\0${nonce}`));
}

/** The code that lets a reload of this tab sign in again, on daemon instance `instance` only. */
export async function resumeCodeFor(token: string, instance: string): Promise<string> {
  const key = await hmac(encoder.encode(token), RESUME_KEY_LABEL);
  return RESUME_CODE_PREFIX + hex(await hmac(key, instance));
}

/** Compares without stopping at the first difference. */
function sameText(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

export type ProofResult =
  | { kind: "proven"; instance: string }
  /**
   * Something answered but did not prove it knows the credential. Nothing was sent. `instance`
   * is what it claimed to be, if it answered in the shape of a proof.
   */
  | { kind: "unproven"; why: string; instance?: string }
  /** Nothing answered. */
  | { kind: "unreachable" };

export interface ProofDeps {
  /** The daemon's port: the page's own, or in development the one Vite proxies to. */
  port: number;
  fetch?: typeof fetch;
}

/**
 * Asks whatever serves this origin to prove it knows `credential` (the token, or with `resume`
 * a resume code). Sends only a fresh random nonce.
 */
export async function requestProof(credential: string, deps: ProofDeps, opts: { resume?: boolean } = {}): Promise<ProofResult> {
  const nonce = hex(crypto.getRandomValues(new Uint8Array(32)));
  const query = new URLSearchParams({ nonce });
  if (opts.resume) query.set("resume", "true");
  let res: Response;
  try {
    res = await (deps.fetch ?? fetch)(`${PROOF_PATH}?${query.toString()}`, { cache: "no-store", credentials: "omit" });
  } catch {
    return { kind: "unreachable" };
  }
  if (!res.ok) return { kind: "unproven", why: `it answered HTTP ${res.status}` };
  let answer: DaemonProof;
  try {
    answer = (await res.json()) as DaemonProof;
  } catch {
    return { kind: "unproven", why: "its answer is not a proof" };
  }
  if (typeof answer?.instance !== "string" || typeof answer.proof !== "string") {
    return { kind: "unproven", why: "its answer is not a proof" };
  }
  const want = await proofFor(credential, deps.port, answer.instance, nonce);
  return sameText(want, answer.proof)
    ? { kind: "proven", instance: answer.instance }
    : { kind: "unproven", why: "its proof is wrong", instance: answer.instance };
}

/** The daemon's port as this page should expect it. */
export function daemonPort(loc: Pick<Location, "port" | "protocol"> = window.location): number {
  // In development the page comes from Vite, which proxies `/ws` and `/auth` to the daemon.
  if (import.meta.env.DEV && typeof __PULSE_DEV_DAEMON_PORT__ === "number") return __PULSE_DEV_DAEMON_PORT__;
  if (loc.port) return Number(loc.port);
  return loc.protocol === "https:" ? 443 : 80;
}
