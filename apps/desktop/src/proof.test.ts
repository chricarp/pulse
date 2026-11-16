import { describe, expect, it } from "vitest";

import { proofFor, requestProof } from "./proof.ts";

// The vector `pulse-daemon/src/auth.rs` and `apps/web/src/rpc/proof.test.ts` check too.
const TOKEN = "0123456789abcdef0123456789abcdef";
const INSTANCE = "00112233445566778899aabbccddeeff";

function server(answer: (nonce: string) => unknown, status = 200, seen: string[] = []): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit) => {
    seen.push(String(input) + JSON.stringify(init?.headers ?? {}));
    const nonce = new URL(String(input)).searchParams.get("nonce") ?? "";
    return new Response(JSON.stringify(answer(nonce)), { status });
  }) as typeof fetch;
}

describe("daemon proof", () => {
  it("matches the daemon's computation", () => {
    expect(proofFor(TOKEN, 7477, INSTANCE, "ab".repeat(32))).toBe("53a4504b2c9efa0f0b23a3209f384d908c85147e725d1244d827c5ec32fb4a4a");
  });

  it("accepts only a proof for this token, port and nonce, and sends neither", async () => {
    const seen: string[] = [];
    const real = server((n) => ({ instance: INSTANCE, proof: proofFor(TOKEN, 7477, INSTANCE, n) }), 200, seen);
    expect(await requestProof(7477, TOKEN, real)).toEqual({ kind: "proven" });
    const relayed = server((n) => ({ instance: INSTANCE, proof: proofFor(TOKEN, 7478, INSTANCE, n) }), 200, seen);
    expect((await requestProof(7477, TOKEN, relayed)).kind).toBe("unproven");
    const replayed = server(() => ({ instance: INSTANCE, proof: proofFor(TOKEN, 7477, INSTANCE, "ab".repeat(32)) }), 200, seen);
    expect((await requestProof(7477, TOKEN, replayed)).kind).toBe("unproven");
    expect((await requestProof(7477, TOKEN, server(() => "<html>", 404, seen))).kind).toBe("unproven");
    expect((await requestProof(7477, TOKEN, server(() => ({ proof: 1 }), 200, seen))).kind).toBe("unproven");
    expect(seen.join("\n")).not.toContain(TOKEN);
  });
});
