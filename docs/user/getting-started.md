# Getting started

This page takes you from a clone to a rule that runs, first with a harmless command (free, no
model), then with the two shipped templates (which run a real agent and cost money).

Pulse is developed and run on Linux. macOS should work but has not been run by anyone yet; see
[Status](../../README.md#status-and-honest-limits).

## Build

You need:

- **Rust** (stable, via [rustup](https://rustup.rs)) and a C compiler (`cc`; SQLite is compiled
  in). On Debian/Ubuntu: `build-essential`. On macOS: the Xcode command line tools.
- **Node.js 22 or later** and **pnpm** at the version in `package.json` (`corepack enable` gives
  you the right one). Only needed for the web UI and the desktop app.

From the repository root:

```sh
./scripts/build.sh --no-app     # the web UI and the pulse binary with the UI inside
./scripts/build.sh              # the same, plus the desktop app
```

| Output | What |
|---|---|
| `target/release/pulse` | the daemon and CLI, one binary (about 16 MB), with the web UI embedded |
| `apps/desktop/release/Pulse-<version>-linux-<arch>.AppImage` | the desktop app, on Linux |
| `apps/desktop/release/Pulse-<version>-mac-<arch>.dmg` | the desktop app, on macOS (never built by a developer yet) |

The release build is slow (link-time optimisation: about 7 minutes on an 8-core arm64 machine
from a cold cache). Without Node, `cargo build --release -p pulse-cli` builds the binary alone; it
then serves a page that says how to build the UI.

Put the binary on your `PATH`, for example `ln -s "$PWD/target/release/pulse" ~/.local/bin/pulse`.

## Where Pulse keeps things

Everything lives in the **Pulse home**: `$PULSE_HOME`, or `~/.pulse` if it is not set. Every
command takes `--home <dir>` too.

| Path | What |
|---|---|
| `pulse.toml` | your configuration: sources, agents, rules |
| `pulse.db` | the event log, deliveries and runs (SQLite) |
| `token` | the daemon's secret (mode 0600); whoever can read it controls Pulse |
| `daemon.json` | pid and port of the running daemon |
| `runs/<run id>/` | each run's `event.json`, and its working directory when the agent has no `cwd` |
| `logs/pulse.log` | the daemon's log, rotated at 10 MB |

To try Pulse without touching your real home, use a throwaway one:

```sh
export PULSE_HOME=$(mktemp -d)
```

## A first rule, with no model (one minute)

Start the daemon in the background and write a starter config:

```console
$ pulse init
Wrote /home/me/.pulse/pulse.toml.
$ pulse start --detach
Pulse started in the background (pid 99267) on http://127.0.0.1:7477
```

(`pulse start` without `--detach` runs in the foreground and logs to the terminal; Ctrl-C stops
it.)

Add a source that watches a folder, an agent that runs `wc -c` on a file, and a rule that wakes
the agent for every new file there. Append this to `pulse.toml`; the daemon reloads it when you
save:

```sh
mkdir -p ~/pulse-demo
cat >> "${PULSE_HOME:-$HOME/.pulse}/pulse.toml" <<'EOF'

[[source]]
name = "demo"
kind = "fs"
path = "~/pulse-demo"

[[agent]]
name = "counter"
adapter = "command"
command = ["wc", "-c", "--", "{{path}}"]
allow = ["fs.file.created:~/pulse-demo/**"]

[[rule]]
name = "count-bytes"
on = "fs.file.created"
where = { path = "~/pulse-demo/**" }
run = { agent = "counter" }
EOF
```

In a second terminal, follow what happens:

```console
$ pulse tail
Following events and runs; Ctrl-C stops.
```

Then make a file:

```sh
echo hello > ~/pulse-demo/note.txt
```

About two seconds later (Pulse waits for a file to stop changing), `pulse tail` shows the event
and the run it caused:

```
run 01M7HXVDXTRBNQR0XEKAPNR8D1  queued           rule=count-bytes agent=counter
2026-11-16T10:18:38.953Z  fs.file.created /home/me/pulse-demo/note.txt  [01M7HXVDXTRBNQR0XEKAPNR8D0]
run 01M7HXVDXTRBNQR0XEKAPNR8D1  succeeded        rule=count-bytes agent=counter exit=0
```

(The run's line can arrive before the event's line; they are separate notifications.)

```console
$ pulse runs logs 01M7HXVDXTRBNQR0XEKAPNR8D1
6 /home/me/pulse-demo/note.txt
$ pulse rules history count-bytes
2026-11-16T10:18:40.954Z  count-bytes          fired  run 01M7HXVDXTRBNQR0XEKAPNR8D1  [event 01M7HXVDXTRBNQR0XEKAPNR8D0]
```

`pulse events show <event id>` prints the event's data and every rule it reached;
`pulse runs show <run id>` prints exactly what was run, where, and when.

### In the UI

```sh
pulse open
```

opens the UI in your browser, signed in with a one-time code that is valid for two minutes
(`pulse open --print` prints the URL instead). The **Feed** shows the event with a green
`count-bytes fired` marker; `×2` next to it means two raw filesystem signals were folded into
this one event. Click the event for its detail, or **Runs** for the run and its output.

The UI is served by the daemon itself at `http://127.0.0.1:7477/`. A new browser tab needs a new
`pulse open`.

## The shipped templates (these run a real agent)

Two templates come with Pulse. Each adds a source (where needed), a Claude Code agent with narrow
permissions, and a rule:

```console
$ pulse init --list
lecture-notes    a new lecture PDF -> a summary and flashcards next to it (Claude Code)
build-failure    a build wrapped with `pulse run` fails -> a diagnosis (Claude Code, read-only)
$ pulse init --template lecture-notes --template build-failure
Added the lecture-notes template to /home/me/.pulse/pulse.toml. Review its paths before relying on it.
Added the build-failure template to /home/me/.pulse/pulse.toml. Review its paths before relying on it.
```

They refer to `~/University` and `~/code`. Edit those paths in `pulse.toml` to where your files
are. Until the folders exist, `pulse status` warns that the agents' working directories are
missing.

- **`lecture-notes`**: a PDF saved anywhere under `~/University` (once it has stopped changing for
  10 s) wakes Claude Code in that folder. It may read, and edit files under `~/University` only
  (`Edit(./**)`); it writes `<name>.summary.md` and `<name>.flashcards.md` beside the PDF.
- **`build-failure`**: wrap a build or test with `pulse run`:

  ```sh
  cd ~/code/myproject
  pulse run -- cargo test
  ```

  When the command fails, Claude Code reads the output tail and the code around it, with
  read-only tools, and explains the likely cause and fix. The diagnosis is the run's output
  (`pulse runs logs <id>`, or the run in the UI). `pulse run` exits with the command's exit code
  and stays silent if the daemon is not running, so it is safe in scripts.

**What they cost.** These run the real `claude` CLI on your account; Pulse does not run a model
and adds no cost of its own. For scale: one `lecture-notes` run on a one-page PDF with
`model = "haiku"` took 13.5 s and Claude Code reported $0.0058 (recorded in
[docs/internals/0009](../internals/0009-end-to-end.md#one-real-run)). Longer PDFs and larger
models cost more. The templates set no `model`, so Claude Code's default applies; add
`model = "haiku"` to the agent to choose the cheapest. Limits in the templates (`rate_limit`, one
run at a time, a short queue) bound how many runs a burst of files can cause.

Requirements: Claude Code installed (`claude` on the daemon's `PATH`) and signed in. If it is not
installed, `pulse status` warns that the agent's program cannot be found.

**Loop protection and your own files.** Pulse stops a rule from reacting to its own run's
output, and part of how it decides who wrote a file is a guess. In this version, a PDF you save
into `~/University` while a `lecture-notes` run is working there can be taken for the agent's
output and skipped. It is never lost silently: `pulse rules history lecture-notes` shows it as
`blocked: loop protection`, followed by the `pulse replay <event>` command that runs it. See
[Loop protection](loop-protection.md).

## Stopping

```sh
pulse stop        # stops the daemon; running runs are terminated and recorded as interrupted
pulse pause       # keeps watching and recording, but nothing fires; pulse resume undoes it
```

## Next

- [Rules](rules.md): matching, limits, approvals, testing, history.
- [Your first rule in five minutes](../../README.md#your-first-rule-in-five-minutes), from the UI.
- [Troubleshooting](troubleshooting.md): why did this fire, why didn't it.
