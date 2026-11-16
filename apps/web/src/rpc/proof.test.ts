import { describe, expect, it } from "vitest";

import { proofFor, requestProof, resumeCodeFor } from "./proof";

// The vector `pulse-daemon/src/auth.rs` and `apps/desktop/src/proof.test.ts` check too.
const TOKEN = "0123456789abcdef0123456789abcdef";
const INSTANCE = "00112233445566778899aabbccddeeff";
const NONCE = "ab".repeat(32);

/** A fetch that answers the proof request with what `answer` computes from the nonce. */
function server(answer: (nonce: string) => Promise<unknown> | unknown, status = 200): typeof fetch {
  return (async (input: RequestInfo | URL) => {
    const nonce = new URL(String(input), "http://127.0.0.1").searchParams.get("nonce") ?? "";
    return new Response(JSON.stringify(await answer(nonce)), { status });
  }) as typeof fetch;
}

describe("daemon proof", () => {
  it("matches the daemon's computation", async () => {
    expect(await proofFor(TOKEN, 7477, INSTANCE, NONCE)).toBe("53a4504b2c9efa0f0b23a3209f384d908c85147e725d1244d827c5ec32fb4a4a");
    expect(await resumeCodeFor(TOKEN, INSTANCE)).toBe("r.f07e6833dc77755c78d65044a0a50a984a8623d97c7d3c813796a0c5d1816bdf");
  });

  it("accepts only a proof for this credential, port and nonce", async () => {
    const real = server(async (n) => ({ instance: INSTANCE, proof: await proofFor(TOKEN, 7477, INSTANCE, n) }));
    expect(await requestProof(TOKEN, { port: 7477, fetch: real })).toEqual({ kind: "proven", instance: INSTANCE });
    // A real daemon's answer relayed from another port.
    const relayed = server(async (n) => ({ instance: INSTANCE, proof: await proofFor(TOKEN, 7478, INSTANCE, n) }));
    expect((await requestProof(TOKEN, { port: 7477, fetch: relayed })).kind).toBe("unproven");
    // A replayed answer for another nonce.
    const replayed = server(async () => ({ instance: INSTANCE, proof: await proofFor(TOKEN, 7477, INSTANCE, NONCE) }));
    expect((await requestProof(TOKEN, { port: 7477, fetch: replayed })).kind).toBe("unproven");
    const other = server(async (n) => ({ instance: INSTANCE, proof: await proofFor("another token", 7477, INSTANCE, n) }));
    expect((await requestProof(TOKEN, { port: 7477, fetch: other })).kind).toBe("unproven");
    expect((await requestProof(TOKEN, { port: 7477, fetch: server(() => "<html>", 404) })).kind).toBe("unproven");
    expect((await requestProof(TOKEN, { port: 7477, fetch: server(() => ({})) })).kind).toBe("unproven");
    const down = (async () => {
      throw new TypeError("connection refused");
    }) as typeof fetch;
    expect(await requestProof(TOKEN, { port: 7477, fetch: down })).toEqual({ kind: "unreachable" });
  });

  it("sends nothing but a fresh long nonce", async () => {
    const asked: string[] = [];
    const record = (async (input: RequestInfo | URL, init?: RequestInit) => {
      asked.push(String(input) + JSON.stringify(init ?? {}));
      return new Response("{}", { status: 404 });
    }) as typeof fetch;
    await requestProof(TOKEN, { port: 7477, fetch: record });
    await requestProof(TOKEN, { port: 7477, fetch: record }, { resume: true });
    expect(asked.join("\n")).not.toContain(TOKEN);
    const nonces = asked.map((a) => /nonce=([0-9a-f]+)/.exec(a)?.[1]);
    expect(nonces[0]).toHaveLength(64);
    expect(nonces[0]).not.toBe(nonces[1]);
    expect(asked[1]).toContain("resume=true");
  });
});
