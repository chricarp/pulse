// A real Pulse daemon for browser tests: the `pulse` binary (with this UI embedded) on a temp
// HOME and PULSE_HOME, port 0, real sources watching temp directories, and the fake agents
// from crates/pulse-agents/tests/fixtures. Never the user's ~/.pulse.
//
// Only processes this file spawned are ever signalled, by the PID captured at spawn.

import { spawn, type ChildProcess, execFileSync } from "node:child_process";
import { createHmac } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { once } from "node:events";

const repo = resolve(import.meta.dirname, "../../..");
export const fixtures = join(repo, "crates/pulse-agents/tests/fixtures");
export const pulseBin = process.env.PULSE_BIN ?? join(repo, "target/debug/pulse");

export interface Daemon {
  /** The temp HOME (`~`). */
  home: string;
  pulseHome: string;
  port: number;
  /** A fresh `pulse open` URL with a login code. */
  loginUrl(): string;
  /** The daemon token, to check that it never reaches anything else. */
  token(): string;
  /** A login code that expired a second ago, signed with the real secret. */
  expiredLoginUrl(): string;
  pulse(...args: string[]): string;
  /** `pulse ingest -s <source>`: one external event `ext.<source>.<type>`. */
  ingest(source: string, type: string, subject: string): void;
  readConfig(): string;
  writeConfig(text: string): void;
  stop(): Promise<void>;
  /** Starts it again on the same port and home. */
  start(): Promise<void>;
  /** Stops it and removes the temp directory. */
  dispose(): Promise<void>;
}

export const BASE_CONFIG = (_home: string) => `# Test configuration. Comments must survive edits made from the UI.

[[source]]
name = "inbox"
kind = "fs"
path = "~/Inbox"
quiet = "200ms"

[[agent]]
name = "echo"
adapter = "command"
command = ["${fixtures}/fake-agent", "exit", "0"]
allow = ["fs.*:~/**", "ext.*"]

[[agent]]
name = "writer"
adapter = "claude-code"
program = "${fixtures}/fake-claude"
allow = ["ext.*"]
env = { FAKE_AGENT_STREAM = "${fixtures}/claude-code/tool-denied.jsonl", FAKE_AGENT_DELAY = "0.25" }

# Fires for every file that appears in ~/Inbox.
[[rule]]
name = "inbox-file"
on = "fs.file.created"
where = { path = "~/Inbox/**" }
run = { agent = "echo" }

[[rule]]
name = "careful"
on = "ext.review.*"
supervised = true
run = { agent = "echo" }

[[rule]]
name = "write-up"
on = "ext.write.*"
run = { agent = "writer", prompt = "Write it up: {{subject}}" }
`;

export async function startDaemon(config?: (home: string) => string): Promise<Daemon> {
  const root = mkdtempSync(join(tmpdir(), "pulse-web-e2e-"));
  const home = join(root, "home");
  const pulseHome = join(home, ".pulse");
  mkdirSync(join(home, "Inbox"), { recursive: true });
  mkdirSync(pulseHome, { recursive: true });
  writeFileSync(join(pulseHome, "pulse.toml"), (config ?? BASE_CONFIG)(home));
  const env = { ...process.env, HOME: home, PULSE_HOME: pulseHome };
  let child: ChildProcess | null = null;
  let port = 0;

  const start = async () => {
    const c = spawn(pulseBin, ["start", "--ready-line", "--port", String(port)], { env, stdio: ["ignore", "pipe", "pipe"] });
    child = c;
    let out = "";
    let err = "";
    c.stderr?.on("data", (d: Buffer) => (err += d.toString()));
    const ready = new Promise<number>((resolveReady, reject) => {
      c.stdout?.on("data", (d: Buffer) => {
        out += d.toString();
        const m = /^ready (\d+)$/m.exec(out);
        if (m) resolveReady(Number(m[1]));
      });
      c.once("exit", (code) => reject(new Error(`pulse exited (${code}) before it was ready: ${err}`)));
    });
    port = await ready;
  };

  const stop = async () => {
    const c = child;
    child = null;
    if (!c || c.exitCode !== null) return;
    const exited = once(c, "exit");
    c.kill("SIGTERM");
    await exited;
  };

  const pulse = (...args: string[]) => execFileSync(pulseBin, args, { env }).toString().trim();

  // Read the build before starting anything: a missing `dist` must not leave a daemon behind.
  const stale = `Run \`pnpm build\`, then \`cargo clean -p pulse-daemon && cargo build -p pulse-cli\`.`;
  let built: string;
  try {
    built = readFileSync(resolve(import.meta.dirname, "../dist/index.html"), "utf8");
  } catch {
    throw new Error(`apps/web/dist is missing. ${stale}`);
  }
  const asset = (html: string) => /assets\/index-[^"]+\.js/.exec(html)?.[0];
  await start();
  try {
    const served = await (await fetch(`http://127.0.0.1:${port}/`)).text();
    if (asset(served) !== asset(built)) {
      throw new Error(`${pulseBin} serves a different UI build than apps/web/dist. ${stale}`);
    }
  } catch (e) {
    await stop();
    throw e;
  }

  return {
    home,
    pulseHome,
    get port() {
      return port;
    },
    loginUrl: () => pulse("open", "--print"),
    token: () => readFileSync(join(pulseHome, "token"), "utf8").trim(),
    expiredLoginUrl: () => {
      const secret = readFileSync(join(pulseHome, "token"), "utf8").trim();
      const expiry = String(Date.now() - 1000);
      const mac = createHmac("sha256", secret).update("pulse/ui-login/v1\0").update(expiry).digest("hex");
      return `http://127.0.0.1:${port}/#login=${expiry}.${mac}`;
    },
    pulse,
    ingest: (source, type, subject) => {
      execFileSync(pulseBin, ["ingest", "-s", source], { env, input: `${JSON.stringify({ type, subject })}\n`, stdio: ["pipe", "pipe", "pipe"] });
    },
    readConfig: () => readFileSync(join(pulseHome, "pulse.toml"), "utf8"),
    writeConfig: (text) => writeFileSync(join(pulseHome, "pulse.toml"), text),
    stop,
    start,
    dispose: async () => {
      await stop();
      rmSync(root, { recursive: true, force: true });
    },
  };
}
