// Before the shell sends the token anywhere, whatever listens on the daemon's port must prove
// that it knows it: a stale `daemon.json` can name a port another program now holds. The
// computation is specified in `pulse_contracts::auth`; the daemon's (`pulse-daemon/src/auth.rs`)
// and the web UI's (`apps/web/src/rpc/proof.ts`) must agree with this one, and all three test
// the same vector (`proof.test.ts`), which also pins the labels written out here.

import { createHmac, randomBytes, timingSafeEqual } from "node:crypto";

import type { DaemonProof } from "./contracts.ts";

const PROOF_KEY_LABEL = "pulse/daemon-proof/v1";

/** What the daemon on `port`, started as `instance`, answers for `credential` and `nonce`. */
export function proofFor(credential: string, port: number, instance: string, nonce: string): string {
  const key = createHmac("sha256", credential).update(PROOF_KEY_LABEL).digest();
  return createHmac("sha256", key).update(`${port}\0${instance}\0${nonce}`).digest("hex");
}

export type ProofResult = { kind: "proven" } | { kind: "unproven"; why: string } | { kind: "unreachable"; why: string };

/** Asks `127.0.0.1:<port>` to prove it knows `token`. Sends only a fresh random nonce. */
export async function requestProof(port: number, token: string, fetchImpl: typeof fetch = fetch, timeoutMs = 3000): Promise<ProofResult> {
  const nonce = randomBytes(32).toString("hex");
  let res: Response;
  try {
    res = await fetchImpl(`http://127.0.0.1:${port}/auth/proof?nonce=${nonce}`, { signal: AbortSignal.timeout(timeoutMs) });
  } catch (e) {
    return { kind: "unreachable", why: (e as Error).message };
  }
  if (!res.ok) return { kind: "unproven", why: `it answered HTTP ${res.status}` };
  let answer: Partial<DaemonProof>;
  try {
    answer = (await res.json()) as Partial<DaemonProof>;
  } catch {
    return { kind: "unproven", why: "its answer is not a proof" };
  }
  if (typeof answer.instance !== "string" || typeof answer.proof !== "string") return { kind: "unproven", why: "its answer is not a proof" };
  const want = Buffer.from(proofFor(token, port, answer.instance, nonce));
  const got = Buffer.from(answer.proof);
  return want.length === got.length && timingSafeEqual(want, got) ? { kind: "proven" } : { kind: "unproven", why: "its proof is wrong" };
}
