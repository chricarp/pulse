// Finding, starting and stopping the daemon. Everything that touches the Pulse home goes through
// the `pulse` binary or the RPC socket, except reading `daemon.json` (the documented way for a
// client to find the port) and the token file (what every local client authenticates with).
// The shell never opens the database or the config file.

import { execFile } from "node:child_process";
import { readFile, realpath } from "node:fs/promises";
import { join } from "node:path";

import type { DaemonInfo } from "./contracts.ts";
import { requestProof } from "./proof.ts";
import { RpcClient, type SocketFactory } from "./rpc.ts";
import { plainText } from "./text.ts";
import { daemonOrigin, loginCodeFrom } from "./urls.ts";

export interface DaemonEnv {
  /** The Pulse home, absolute. */
  home: string;
  /** Where to say why a port was not trusted with the token. */
  log?: (msg: string) => void;
  /** The `pulse` binary to run. */
  binary: string;
  /** Environment for the processes the shell starts. */
  env: NodeJS.ProcessEnv;
  factory?: SocketFactory;
  /** For the proof request (tests). */
  fetch?: typeof fetch;
}

/** `daemon.json` if it exists and looks like one; anything else counts as "no daemon". */
export async function readDaemonInfo(home: string): Promise<DaemonInfo | null> {
  let text: string;
  try {
    text = await readFile(join(home, "daemon.json"), "utf8");
  } catch {
    return null;
  }
  try {
    const v = JSON.parse(text) as Record<string, unknown>;
    if (
      typeof v.pid === "number" &&
      typeof v.port === "number" &&
      Number.isInteger(v.port) &&
      v.port > 0 &&
      v.port < 65536 &&
      typeof v.version === "string" &&
      typeof v.started_at === "string"
    ) {
      return { pid: v.pid, port: v.port, version: v.version, started_at: v.started_at };
    }
  } catch {
    // A half-written or foreign file.
  }
  return null;
}

async function readToken(home: string): Promise<string | null> {
  try {
    const t = (await readFile(join(home, "token"), "utf8")).trim();
    return t.length >= 32 ? t : null;
  } catch {
    return null;
  }
}

async function sameDir(a: string, b: string): Promise<boolean> {
  if (a === b) return true;
  try {
    return (await realpath(a)) === (await realpath(b));
  } catch {
    return false;
  }
}

/**
 * Connects to the daemon `daemon.json` names and checks it is the one for this home: a stale
 * file can name a port that some other program (or another Pulse home's daemon) now uses. The
 * program on the port must prove it knows the token before the token is sent to it.
 * Returns an open client, or `null`.
 */
export async function connectVerified(d: DaemonEnv, info: DaemonInfo, timeoutMs = 3000): Promise<RpcClient | null> {
  const token = await readToken(d.home);
  if (token === null) return null;
  const proof = await requestProof(info.port, token, d.fetch, timeoutMs);
  if (proof.kind !== "proven") {
    if (proof.kind === "unproven") {
      d.log?.(`something else is listening on Pulse's port ${info.port} (${proof.why}); the token was not sent to it`);
    }
    return null;
  }
  let client: RpcClient;
  try {
    client = await RpcClient.connect({
      port: info.port,
      token,
      timeoutMs,
      ...(d.factory ? { factory: d.factory } : {}),
    });
  } catch {
    return null;
  }
  try {
    const status = await client.call("system.status", {});
    if (status.port === info.port && (await sameDir(status.pulse_home, d.home))) return client;
  } catch {
    // Not a Pulse daemon, or not ours (wrong token).
  }
  client.close();
  return null;
}

/** A daemon that answers for this home, with an open connection to it. */
export async function locate(d: DaemonEnv): Promise<{ info: DaemonInfo; client: RpcClient } | null> {
  const info = await readDaemonInfo(d.home);
  if (info === null) return null;
  const client = await connectVerified(d, info);
  return client === null ? null : { info, client };
}

interface Ran {
  code: number | null;
  stdout: string;
  stderr: string;
}

function run(d: DaemonEnv, args: string[], timeoutMs: number): Promise<Ran> {
  return new Promise((resolve) => {
    execFile(
      d.binary,
      ["--home", d.home, ...args],
      { env: d.env, timeout: timeoutMs, maxBuffer: 1024 * 1024, windowsHide: true },
      (err, stdout, stderr) => {
        const code = err === null ? 0 : typeof err.code === "number" ? err.code : null;
        resolve({ code, stdout: String(stdout), stderr: String(stderr) });
      },
    );
  });
}

function failure(what: string, r: Ran): string {
  const detail = plainText(r.stderr || r.stdout, 400);
  return detail ? `${what}: ${detail}` : what;
}

/**
 * Starts the daemon in the background with `pulse start --detach`: it runs in its own process
 * group, so it outlives the app, and the command returns once it listens. Resolves with the
 * daemon's `daemon.json` and an open connection.
 */
export async function spawnDaemon(d: DaemonEnv): Promise<{ info: DaemonInfo; client: RpcClient; pid: number | null }> {
  const r = await run(d, ["--json", "start", "--detach"], 45_000);
  if (r.code !== 0) throw new Error(failure("the daemon did not start", r));
  let pid: number | null = null;
  try {
    const out = JSON.parse(r.stdout) as { pid?: unknown };
    pid = typeof out.pid === "number" ? out.pid : null;
  } catch {
    // Older binaries print text; the pid is informational.
  }
  const info = await readDaemonInfo(d.home);
  if (info === null) throw new Error("the daemon started but wrote no daemon.json");
  const client = await connectVerified(d, info);
  if (client === null) throw new Error(`the daemon started but does not answer on port ${info.port}`);
  return { info, client, pid };
}

/** `pulse stop`: asks the daemon to shut down and waits until it has exited. */
export async function stopDaemon(d: DaemonEnv): Promise<void> {
  const r = await run(d, ["stop"], 130_000);
  if (r.code !== 0) throw new Error(failure("the daemon did not stop", r));
}

/**
 * A fresh single-use login code, from `pulse open --print`. The code format is the daemon's
 * business; asking the binary keeps it in one place.
 */
export async function mintLoginCode(d: DaemonEnv, port: number): Promise<string> {
  const r = await run(d, ["--json", "open", "--print"], 10_000);
  if (r.code !== 0) throw new Error(failure("cannot get a login code", r));
  let url = "";
  try {
    url = String((JSON.parse(r.stdout) as { url?: unknown }).url ?? "");
  } catch {
    // Fall through.
  }
  const code = loginCodeFrom(url, daemonOrigin(port));
  if (code === null) throw new Error("`pulse open --print` returned no login code for this daemon");
  return code;
}

/** `pulse --version`, e.g. `0.1.0`. */
export async function binaryVersion(d: DaemonEnv): Promise<string | null> {
  const r = await new Promise<Ran>((resolve) => {
    execFile(d.binary, ["--version"], { env: d.env, timeout: 10_000 }, (err, stdout, stderr) =>
      resolve({ code: err === null ? 0 : 1, stdout: String(stdout), stderr: String(stderr) }),
    );
  });
  if (r.code !== 0) return null;
  const m = /(\d+\.\d+\.\d+\S*)/.exec(r.stdout);
  return m?.[1] ?? null;
}
