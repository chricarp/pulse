import { describe, expect, it } from "vitest";

import { RESUME_STORAGE_KEY } from "../contracts";
import { login, parseLoginFragment, ReloadKeeper, takeResume } from "./login";
import { proofFor, resumeCodeFor } from "./proof";
import { webSocketFactory } from "./socket";

const TOKEN = "0123456789abcdef0123456789abcdef";
const INSTANCE = "00112233445566778899aabbccddeeff";

describe("login fragment", () => {
  it("takes the code and keeps the rest of the fragment", () => {
    expect(parseLoginFragment("#login=123.abc")).toEqual({ code: "123.abc", rest: "" });
    expect(parseLoginFragment("#login=123.abc&x=1")).toEqual({ code: "123.abc", rest: "#x=1" });
    expect(parseLoginFragment("#x=1")).toEqual({ code: null, rest: "#x=1" });
    expect(parseLoginFragment("")).toEqual({ code: null, rest: "" });
    expect(parseLoginFragment("#login=")).toEqual({ code: null, rest: "" });
  });
});

class MemoryStorage {
  private readonly m = new Map<string, string>();
  get length() {
    return this.m.size;
  }
  getItem(k: string) {
    return this.m.get(k) ?? null;
  }
  setItem(k: string, v: string) {
    this.m.set(k, v);
  }
  removeItem(k: string) {
    this.m.delete(k);
  }
  clear() {
    this.m.clear();
  }
  key() {
    return null;
  }
}

/** The daemon (or an impostor) as `fetch` sees it; records every request body. */
function daemon(opts: { instance?: string; secret?: string; codes?: string[] } = {}) {
  const instance = opts.instance ?? INSTANCE;
  const secret = opts.secret ?? TOKEN;
  const sent: string[] = [];
  const f = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), "http://127.0.0.1:7477");
    sent.push(`${url.pathname}${url.search} ${String(init?.body ?? "")}`);
    if (url.pathname === "/auth/proof") {
      const nonce = url.searchParams.get("nonce") ?? "";
      const credential = url.searchParams.get("resume") ? await resumeCodeFor(secret, instance) : secret;
      return new Response(JSON.stringify({ instance, proof: await proofFor(credential, 7477, instance, nonce) }));
    }
    const { code } = JSON.parse(String(init?.body)) as { code: string };
    const ok = (opts.codes ?? []).includes(code) || code === (await resumeCodeFor(secret, instance));
    return ok ? new Response(JSON.stringify({ token: secret })) : new Response("{}", { status: 401 });
  }) as typeof fetch;
  return { fetch: f, sent };
}

describe("login", () => {
  it("exchanges a login code, or a resume code the daemon proved it knows", async () => {
    const d = daemon({ codes: ["1.ab"] });
    expect(await login("1.ab", null, { port: 7477, fetch: d.fetch })).toEqual({ kind: "token", token: TOKEN });
    const resume = { instance: INSTANCE, code: await resumeCodeFor(TOKEN, INSTANCE) };
    expect(await login(null, resume, { port: 7477, fetch: d.fetch })).toEqual({ kind: "token", token: TOKEN });
    // The same link again in this tab: the code is spent, the resume code still works.
    expect(await login("2.cd", resume, { port: 7477, fetch: d.fetch })).toEqual({ kind: "token", token: TOKEN });
    expect(await login("2.cd", null, { port: 7477, fetch: d.fetch })).toEqual({ kind: "none", reason: "code-refused" });
    expect(await login(null, null, { port: 7477, fetch: d.fetch })).toEqual({ kind: "none", reason: "no-code" });
  });

  it("never sends a resume code to a port that cannot prove it, and says why", async () => {
    const resume = { instance: INSTANCE, code: await resumeCodeFor(TOKEN, INSTANCE) };
    // The daemon restarted: a new instance, which cannot know the old code.
    const restarted = daemon({ instance: "ff".repeat(16) });
    expect(await login(null, resume, { port: 7477, fetch: restarted.fetch })).toEqual({ kind: "none", reason: "resume-stale" });
    // A program that took the port and claims the old instance.
    const impostor = daemon({ secret: "not the token, but just as long" });
    const got = await login(null, resume, { port: 7477, fetch: impostor.fetch });
    expect(got.kind === "none" && got.reason).toBe("unproven");
    for (const sent of [...restarted.sent, ...impostor.sent]) expect(sent).not.toContain(resume.code);
  });

  it("stores only an instance-bound resume code, only when the page unloads", async () => {
    const store = new MemoryStorage() as unknown as Storage;
    const handlers = new Map<string, (e: { persisted?: boolean }) => void>();
    const page = { addEventListener: (type: string, h: (e: { persisted?: boolean }) => void) => handlers.set(type, h) };
    const keeper = new ReloadKeeper(TOKEN, store, page as unknown as Window);
    keeper.connected(INSTANCE);
    await keeper.settled();
    expect(store.length).toBe(0);
    handlers.get("pagehide")?.({});
    const stored = store.getItem(RESUME_STORAGE_KEY) ?? "";
    expect(stored).not.toContain(TOKEN);
    expect(JSON.parse(stored)).toEqual({ instance: INSTANCE, code: await resumeCodeFor(TOKEN, INSTANCE) });
    // Back from the back/forward cache: running again, so it leaves storage.
    handlers.get("pageshow")?.({ persisted: true });
    expect(store.length).toBe(0);
    handlers.get("pagehide")?.({});
    expect(takeResume(store)?.instance).toBe(INSTANCE);
    expect(store.length).toBe(0); // taken means removed
    // A rejected token leaves nothing for a reload.
    keeper.forget();
    handlers.get("pagehide")?.({});
    expect(store.length).toBe(0);
  });
});

describe("socket", () => {
  it("opens the WebSocket that carries the token only after the proof", async () => {
    const opened: string[][] = [];
    class FakeWs {
      onopen: (() => void) | null = null;
      onmessage: ((e: { data: unknown }) => void) | null = null;
      onclose: ((e: { code: number; reason: string }) => void) | null = null;
      constructor(_url: string, protocols: string[]) {
        opened.push(protocols);
      }
      send() {}
      close() {}
    }
    const closes: [number, string][] = [];
    const connect = (proof: { kind: "proven"; instance: string } | { kind: "unproven"; why: string } | { kind: "unreachable" }) => {
      const answered = Promise.resolve(proof);
      const s = webSocketFactory("ws://127.0.0.1:7477/ws", TOKEN, {
        prove: () => answered,
        WebSocket: FakeWs as unknown as new (url: string, protocols: string[]) => WebSocket,
      })();
      s.onclose = (code, reason) => closes.push([code, reason]);
      // The socket acts in the reaction to the proof, queued before this one.
      return answered.then(() => undefined);
    };
    await connect({ kind: "unproven", why: "its proof is wrong" });
    await connect({ kind: "unreachable" });
    expect(opened).toEqual([]);
    expect(closes).toEqual([
      [4900, "its proof is wrong"],
      [1006, "the daemon did not answer"],
    ]);
    await connect({ kind: "proven", instance: INSTANCE });
    expect(opened).toEqual([["pulse", `pulse.token.${TOKEN}`]]);
  });
});
