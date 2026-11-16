// `pnpm dev:real`: UI development against a real daemon.
//
// Creates a temp HOME (with ~/University and ~/Inbox) and a Pulse home inside it, writes a config
// whose agents are the fake agents from crates/pulse-agents/tests/fixtures (no real agent ever
// runs), starts `target/debug/pulse` on a free port, starts Vite proxying /ws and /auth to it, and
// prints a login URL for Vite's origin. Ctrl-C stops both and removes the temp directory.
//
// Build the daemon first: cargo build -p pulse-cli. Never touches ~/.pulse.

import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const web = resolve(import.meta.dirname, "..");
const repo = resolve(web, "../..");
const fixtures = join(repo, "crates/pulse-agents/tests/fixtures");
const pulseBin = process.env.PULSE_BIN ?? join(repo, "target/debug/pulse");

const freePort = () =>
  new Promise((ok, fail) => {
    const s = createServer();
    s.once("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => ok(port));
    });
  });

const root = mkdtempSync(join(tmpdir(), "pulse-dev-"));
const home = join(root, "home");
const pulseHome = join(home, ".pulse");
for (const d of ["University", "Inbox", ".pulse"]) mkdirSync(join(home, d), { recursive: true });
writeFileSync(
  join(pulseHome, "pulse.toml"),
  `# Development config (pnpm dev:real). Agents are fakes; nothing real runs.

[[source]]
name = "university"
kind = "fs"
path = "~/University"

[[source]]
name = "inbox"
kind = "fs"
path = "~/Inbox"

[[agent]]
name = "study"
adapter = "claude-code"
program = "${fixtures}/fake-claude"
allow = ["fs.file.created:~/University/**"]
env = { FAKE_AGENT_STREAM = "${fixtures}/claude-code/tool-denied.jsonl", FAKE_AGENT_DELAY = "0.3" }

[[agent]]
name = "echo"
adapter = "command"
command = ["${fixtures}/fake-agent", "exit", "0"]
allow = ["fs.*:~/**", "ext.*"]

[[rule]]
name = "lecture-notes"
on = "fs.file.created"
where = { path = "~/University/**/*.pdf" }
debounce = "3s"
run = { agent = "study", prompt = "Summarize {{path}}." }

[[rule]]
name = "inbox"
on = "fs.file.*"
where = { path = "~/Inbox/**" }
supervised = true
run = { agent = "echo" }
`,
);

const env = { ...process.env, HOME: home, PULSE_HOME: pulseHome };
const daemonPort = await freePort();
const vitePort = await freePort();
const children = [];
let stopping = false;
const stop = (code = 0) => {
  if (stopping) return;
  stopping = true;
  for (const c of children) if (c.exitCode === null) c.kill("SIGTERM");
  Promise.all(children.map((c) => (c.exitCode === null ? new Promise((r) => c.once("exit", r)) : null))).then(() => {
    rmSync(root, { recursive: true, force: true });
    process.exit(code);
  });
};
process.on("SIGINT", () => stop());
process.on("SIGTERM", () => stop());

const daemon = spawn(pulseBin, ["start", "--ready-line", "--port", String(daemonPort)], { env, stdio: ["ignore", "pipe", "inherit"] });
children.push(daemon);
daemon.once("exit", (code) => {
  if (!stopping) {
    console.error(`pulse exited (${code})`);
    stop(1);
  }
});
await new Promise((ok) => daemon.stdout.on("data", (d) => String(d).includes("ready") && ok()));

const vite = spawn(process.execPath, [join(web, "node_modules/vite/bin/vite.js"), "--port", String(vitePort), "--strictPort"], {
  cwd: web,
  env: { ...process.env, PULSE_DAEMON: `http://127.0.0.1:${daemonPort}` },
  stdio: "inherit",
});
children.push(vite);

const code = execFileSync(pulseBin, ["open", "--print"], { env }).toString().trim().split("#")[1];
console.log(`
Daemon  http://127.0.0.1:${daemonPort}  (HOME=${home})
Log in  http://127.0.0.1:${vitePort}/#${code}   (valid for two minutes, once)
More    HOME=${home} PULSE_HOME=${pulseHome} ${pulseBin} open --print   (then use port ${vitePort})
Try     touch "${home}/University/week-1.pdf"   or   touch "${home}/Inbox/note.txt"
`);
