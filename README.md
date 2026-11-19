<p align="center">
  <img src="assets/icon.svg" alt="" width="96" height="96" />
</p>

<h1 align="center">Lictory Pulse</h1>

<p align="center">
  Wakes coding agents when something happens on your machine, and records why.
</p>

<p align="center">
  <a href="docs/user/getting-started.md">Get started</a> ·
  <a href="docs/user/README.md">User guide</a> ·
  <a href="docs/internals/">Design record</a> ·
  <a href="CONTRIBUTING.md">Contribute</a> ·
  <a href=".github/SECURITY.md">Security</a>
</p>

Lictory Pulse is a small daemon that watches your machine and wakes coding agents when something
happens. A lecture PDF lands in `~/University`, and a summary and flashcards appear beside it. A
build fails, and a diagnosis of why is waiting when you look. Pulse watches files, git
repositories, commands that exit, schedules, calendars and anything that can send it JSON. It
records every event in a durable log, matches events against your rules, and starts Claude Code,
Codex, any command, or wakes an MCP client, within the permissions you give each agent.

Pulse never runs a model itself. It decides **when** an agent runs, **what it is told**, and keeps
a record of **why**: for every rule an event matched, whether it fired, and if not, the reason.

It is local-first: one Rust binary, a SQLite file, a web UI the daemon serves on `127.0.0.1`, an
optional desktop app, and a CLI. Idle, it measured zero CPU and about 17 MB of memory on the
development machine.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/feed-dark.png">
  <img alt="The Lictory Pulse feed: file, git and process events, each tagged with what became of it, such as 'lecture-notes succeeded', 'review-commit awaiting approval' next to an earlier 'review-commit succeeded', 'build-failed cooldown' and 'format-docs loop: own run'; the summaries and flashcards an agent wrote are marked 'by agent'" src="docs/images/feed-light.png">
</picture>

*The feed. Each event shows what it caused and what became of it: a run that succeeded, one
waiting for your approval (the commit before it was approved and its run succeeded), a rule held
by its cooldown or disabled, and (orange) a rule that loop protection stopped from reacting to
its own run's output. Files an agent wrote are marked **by agent** (with a `?` where Pulse
infers it). The screenshots show a fictional home directory and the stand-in agents Pulse's
tests use; `pnpm --dir apps/web docs:images` makes them.*

> **Status: 0.1.0, early.** Developed and run only on Linux arm64. Read
> [Status and honest limits](#status-and-honest-limits) before relying on it.

## Try it in 60 seconds, with no model

This uses a plain command (`wc -c`) as the "agent", so it costs nothing. Build first (see
[Install and build](#install-and-build); the release build takes several minutes), then:

```sh
export PATH="$PWD/target/release:$PATH"
export PULSE_HOME=$(mktemp -d)      # a throwaway Pulse home; leave it out to use ~/.pulse
mkdir -p ~/pulse-demo

pulse init                          # writes a commented starter $PULSE_HOME/pulse.toml
pulse start --detach                # the daemon, in the background

cat >> "$PULSE_HOME/pulse.toml" <<'EOF'

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

The daemon reloads `pulse.toml` when it changes. In a second terminal (with the same
`PULSE_HOME`), run `pulse tail`, then create a file:

```console
$ echo hello > ~/pulse-demo/note.txt

# pulse tail, about two seconds later (Pulse waits for the file to stop changing):
2026-11-16T10:18:38.953Z  fs.file.created /home/me/pulse-demo/note.txt  [01M7HXVDXTRBNQR0XEKAPNR8D0]
run 01M7HXVDXTRBNQR0XEKAPNR8D1  queued           rule=count-bytes agent=counter
run 01M7HXVDXTRBNQR0XEKAPNR8D1  succeeded        rule=count-bytes agent=counter exit=0

$ pulse runs logs NR8D1        # the last few characters of an id are enough
6 /home/me/pulse-demo/note.txt
$ pulse rules history count-bytes
2026-11-16T10:18:40.954Z  count-bytes          fired  run 01M7HXVDXTRBNQR0XEKAPNR8D1  [event 01M7HXVDXTRBNQR0XEKAPNR8D0]
```

`pulse open` opens the same thing in your browser: the event in the **Feed** with a green
`count-bytes fired` marker, and the run with its output under **Runs**. `pulse stop` stops the
daemon.

### The same with a real agent: the two shipped templates

```console
$ pulse init --template lecture-notes --template build-failure
Added the lecture-notes template to /home/me/.pulse/pulse.toml. Review its paths before relying on it.
Added the build-failure template to /home/me/.pulse/pulse.toml. Review its paths before relying on it.
```

- [`lecture-notes`](templates/lecture-notes.toml): a PDF saved under `~/University` wakes
  Claude Code, which may read and may edit files only under that folder, and writes
  `<name>.summary.md` and `<name>.flashcards.md` beside it.
- [`build-failure`](templates/build-failure.toml): `pulse run -- cargo test` (or any command)
  under `~/code`; when it fails, Claude Code reads the output and the code with read-only tools
  and explains the likely cause and fix, as the run's output.

**These run a real agent, on your Claude account, and cost money.** They need Claude Code
installed and signed in. Pulse adds no cost of its own; the cost is the agent's. For scale: one
`lecture-notes` run on a one-page PDF with `model = "haiku"` took 13.5 s and cost $0.0058 as
reported by Claude Code. The templates set no model, so Claude Code's default applies; add
`model = "haiku"` to the agent for the cheapest. Edit the `~/University` and `~/code` paths
first. More in [Getting started](docs/user/getting-started.md#the-shipped-templates-these-run-a-real-agent).

## Install and build

There are no prebuilt packages yet. You need:

- Rust, stable, via [rustup](https://rustup.rs), and a C compiler (SQLite is compiled in).
- Node.js 22 or later and pnpm at the version pinned in `package.json` (`corepack enable`), for
  the web UI and the desktop app.

```sh
git clone https://github.com/chricarp/pulse.git && cd pulse
./scripts/build.sh --no-app    # web UI + the pulse binary (UI embedded): target/release/pulse
./scripts/build.sh             # the same, plus the desktop app in apps/desktop/release/
```

- **Headless:** `target/release/pulse` is everything: `pulse start` is the daemon, every other
  subcommand is a client of it, and the daemon serves the web UI at `http://127.0.0.1:7477/`
  (`pulse open` signs your browser in). Put it on your `PATH`. Without Node,
  `cargo build --release -p pulse-cli` builds the binary alone, serving a placeholder page
  instead of the UI.
- **Desktop app:** `Lictory-Pulse-<version>-linux-<arch>.AppImage` on Linux, a `.dmg` on macOS. It finds or
  starts the daemon and shows the same UI in a window, with a tray menu and notifications.
  Quitting it does not stop the daemon. See [The desktop app](docs/user/desktop.md).

Data lives in `$PULSE_HOME`, default `~/.pulse`: `pulse.toml` (your config), `pulse.db` (the
log), `token` (the daemon's secret), `logs/`.

## Your first rule in five minutes

A rule has three parts: a **source** that produces events, an **agent** with the scopes it may
see, and the **rule** joining them. You can write them in the UI, with the CLI, or in
`pulse.toml`, and **all three edit the same file**. There is no second copy of a rule anywhere:
the UI and CLI ask the daemon, which rewrites `pulse.toml` keeping your comments and layout; when
you edit the file yourself, the daemon reloads it and the UI updates.

**From the UI** (`pulse open`): **Sources → New source** for the folder; **Agents → New agent**
(for a `command` agent, the argv and its scopes); then **Rules → New rule** (or press `N`): a name,
the event types, `where` conditions, limits, the agent and its prompt. Before saving, pick a past
event in **Test against a past event** on the right: it runs the daemon's real routing as a dry
run and shows the decision and the exact prompt the agent would get. **Create rule** (Ctrl/Cmd+S)
writes the rule into `pulse.toml`.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/rule-test-dark.png">
  <img alt="The rule editor beside the rule tester: the tester shows that the rule would be debounced and then fire, and the rendered prompt with the event fenced as untrusted data" src="docs/images/rule-test-light.png">
</picture>

*Editing `lecture-notes` and testing it against a past event: debounced, then it fires; below,
the rule's instruction and the event data, fenced as untrusted.*

**From the file or the CLI:** the same rule, as TOML:

```toml
[[rule]]
name = "count-pdfs"
on = "fs.file.created"
where = { path = "~/pulse-demo/**/*.pdf" }
debounce = "10s"
rate_limit = "20/hour"
run = { agent = "counter" }
```

Save it into `pulse.toml`, or pass the bare table (without `[[rule]]`) to `pulse rules add -f
rule.toml` (`--replace` to change it later). Then `pulse rules show count-pdfs`, `pulse test-rule count-pdfs --event <id>`, and
`pulse rules history count-pdfs`. If a rule can never fire as written (a typo in `on`, scopes that
do not cover its events, a missing program), `pulse status` and the UI say so.

Guides: [Rules](docs/user/rules.md), [Sources](docs/user/sources.md),
[Agents and permissions](docs/user/agents.md), [Config reference](docs/user/config.md),
[CLI reference](docs/user/cli.md), [all guides](docs/user/README.md).

## Security model

Pulse turns things that happen on your machine into instructions for agents that can edit files
and run programs. The full threat model is
[docs/internals/0003-threat-model.md](docs/internals/0003-threat-model.md). In short:

- **Event content is untrusted data.** File names, commit messages, calendar titles, command
  output and external payloads can be chosen by someone else. The rule's prompt is the
  instruction; the event is appended after it as JSON, between two lines carrying a random
  per-run delimiter, introduced as untrusted data not to be followed. Values you substitute into
  the instruction (`{{path}}`) are cut to one line, stripped of control and invisible characters,
  and truncated. This **lowers** the chance of prompt injection; it does not make a model immune.
- **No shell, ever.** A `command` agent's arguments are an array, each rendered on its own, so
  event content cannot add arguments or commands; the program itself cannot be templated.
  Prompts reach Claude Code and Codex on stdin, never in their arguments.
- **Scopes, default deny.** An agent hears only about events its `allow` list covers
  (`"fs.file.created:~/University/**"`), enforced by the router and by MCP, not by prompt text.
- **Unattended runs cannot ask for more.** Claude Code runs with `--permission-mode=dontAsk`
  and Codex with a read-only sandbox and no approvals unless you configure otherwise; Pulse never
  adds a flag that skips checks. The templates pre-approve only what they need.
- **Supervised rules** wait for you to approve each run, with the rendered prompt and the exact
  command in front of you. A run whose agent or rule changed before it started does not start.
  Approving needs the daemon's token; an agent's MCP credentials cannot approve, edit rules or
  read outside its scopes.
- **Loop protection** stops a rule from being triggered by its own run, limits chains of runs,
  and backs both with cooldowns, rate limits and a circuit breaker. Attributing a file to a run is
  partly a guess; see [Loop protection](docs/user/loop-protection.md).
- **Local only.** The daemon listens on `127.0.0.1`, every endpoint needs a token, and it checks
  `Host` and `Origin` against DNS rebinding and other web pages. The Pulse home is private to
  your account (directories `0700`; the token, log and database `0600`), made so at every start.
- **Other accounts on the machine.** While the daemon is stopped, anyone can listen on its port.
  So every client (CLI, web UI, desktop app, the `pulse mcp` bridge) first asks whatever answers
  there to prove it knows the secret, and sends nothing if it cannot. `pulse open` hands the
  browser its single-use login code through a private file, never on a command line, where other
  accounts could read it. If you suspect the token leaked: `pulse stop`, delete
  `$PULSE_HOME/token`, `pulse start`.
- **Edits never overwrite silently.** Every rule, agent and source has a revision; saving over a
  definition that changed meanwhile (in the file, the CLI or another window) is refused and shows
  what changed.
- **Project files** in repositories do nothing until you have reviewed what they define and
  trusted that exact content (`pulse project trust`), and stop again whenever it changes.

What is **not** a safety boundary:

- **Isolation and permissions are separate controls, and neither is a full safety boundary.**
  Pulse's scopes decide what an agent is *told*; they do not limit what a running agent can *do*.
  That is up to the agent's own permission mode and sandbox, which are not a complete sandbox
  either: Claude Code still applies your settings files and those of the directory it runs in.
- An agent runs as you. It can read `$PULSE_HOME/token` and with it do anything the daemon can.
  The per-agent MCP token keeps credentials out of an agent's configuration; it does not contain a
  hostile agent.
- Any program running as your user is fully trusted. The event log is not encrypted; treat
  `$PULSE_HOME` as sensitive.
- The checks against other local accounts have gaps, listed in the
  [threat model](docs/internals/0003-threat-model.md): a browser and the desktop app send the
  proof request and the token on separate connections, milliseconds apart; an MCP client you
  configure with the HTTP URL sends its agent token without asking for a proof (the `pulse mcp`
  bridge does ask); and any local process can keep the daemon's 512 connection slots busy, which
  blocks the CLI and UI while it does.
- The Linux AppImage runs without Chromium's OS sandbox on Ubuntu 23.10 and later (see below).

## Status and honest limits

- **Platforms.** Developed and used only on **Linux arm64** (Ubuntu 24.04). On **macOS** and
  **Linux x64** nobody has used Pulse yet. What has run there is CI: the Rust tests pass (their
  end-to-end part drives the real daemon and its file watcher, FSEvents on macOS) and the
  desktop package builds. The desktop app, the browser UI and the timer behaviour on macOS are
  **untested**. Windows is not supported.
- **CI** (`.github/workflows/`) runs the Rust tests on Linux x64 and macOS, the web and desktop
  unit checks, and the desktop packaging on four targets. It first ran on 19 November 2026.
  The browser suites and the desktop end-to-end scenarios are not in CI; they have run only on
  the development machine.
- **Desktop app.** Unsigned, with no auto-update. On macOS, Gatekeeper blocks the first open
  (workaround in [the desktop guide](docs/user/desktop.md#platform-notes)). The tray icon on real
  desktops, native notifications, the `.dmg`, and x64 builds are **unverified**; what was tested
  ran under a virtual display on Linux arm64. On **Ubuntu 23.10 and later** the AppImage runs
  **without Chromium's OS sandbox** (`--no-sandbox`), because the AppImage cannot use Chromium's
  setuid helper and Ubuntu restricts the unprivileged user namespaces the sandbox needs; the app
  says so in its menus. The browser UI (`pulse open`) is the alternative.
- **`pulse open` and sandboxed browsers.** A browser installed as a snap or flatpak (stock
  Ubuntu Firefox is one) usually cannot read files under `~/.pulse`, so the login page
  `pulse open` hands it may not load. `pulse open --print` prints the address to paste instead.
- **MCP events extension (SEP-3415)** is experimental, **off by default**, and tracks a draft
  that will change; no mainstream client uses it yet. The plain MCP tools are what works today.
- **Loop protection partly guesses.** Filesystems do not say which process wrote a file. For
  Claude Code, Pulse reads the paths its file tools write and attributes exactly those. For
  everything it cannot see (shell commands, `command` and Codex agents) it guesses from the
  run's working directory, which can block a change you made yourself during a run (recorded,
  and `pulse replay` runs it) and can miss an agent's own writes, which then only cooldowns,
  rate limits and the circuit breaker stop (by default up to 20 runs in 10 minutes). Details in
  [Loop protection](docs/user/loop-protection.md#how-pulse-knows-who-caused-an-event).
- **Runs.** A process that leaves its run's process group (`setsid`, daemons) is not killed with
  the run. A run that was running when the daemon stopped is recorded `interrupted`, not
  restarted. Debounces pending when the daemon stops are lost.
- **Sources.** A slow trickle of many files is reported file by file, not folded into one event.
  After an inotify overflow, deletions in the lost window are not reported. A git push to a URL
  without a remote-tracking branch is not seen. A calendar fire more than 15 minutes late (after
  a long suspend) is dropped. Calendar URLs are fetched directly and ignore `HTTP_PROXY` and
  `HTTPS_PROXY`, because a feed's redirects cannot be checked through a proxy; behind a
  proxy-only network, use a local `.ics` file. Calendars have a size and recurrence budget; the
  source's health says when one is hit.
- **Measured** on the development machine (release build): about 17 MB of memory and zero CPU
  when idle; unzipping 10,000 files into a watched folder produced one event and peaked at 18 MB.
  Details in [0009](docs/internals/0009-end-to-end.md#measurements).

## Roadmap

Not in v1, and recorded in the design notes as possible next steps:

- Reading Codex's file changes the way Claude Code's are read, so loop protection guesses less
  for Codex agents too.
- Code signing and notarization, auto-update, and a `.deb` with an AppArmor profile that restores
  Chromium's sandbox on Ubuntu ([0010](docs/internals/0010-desktop-shell.md#known-gaps-in-v1)).
- Notifications for single loop-protection blocks (today only a tripped circuit breaker notifies).
- Killing every process a run started, including ones that left its group, with a cgroup per run
  when the daemon runs as a systemd user service ([0006](docs/internals/0006-agent-adapters.md#the-orphan-guarantee)).
- Answering `wait_for_events` as an MCP Task, and turning SEP-3415 on by default once the draft is
  merged and a client uses it ([0007](docs/internals/0007-mcp-and-sep-3415.md)).
- Windows, clipboard, screen, app-focus and browser sources, email, cloud accounts, mobile, remote
  access, and outgoing webhooks as a rule action are out of v1; the code leaves seams for them
  ([0002](docs/internals/0002-architecture.md#not-in-v1-seams-only)).

## Contributing and support

Read [Contributing](CONTRIBUTING.md) first: it has the commands that must pass and the areas
that always need a maintainer. [AGENTS.md](AGENTS.md) holds the conventions, the crates, the
testing rules and the ways to hurt yourself (never run against your real `~/.pulse`, never kill
by name, never run a real agent in a test); it applies to human and agent contributors alike.
The design record is [docs/internals/](docs/internals/), beginning with
[0002-architecture.md](docs/internals/0002-architecture.md).

See also [Governance](GOVERNANCE.md) and the [Code of conduct](CODE_OF_CONDUCT.md). Use
[Support](SUPPORT.md) for setup and usage questions; report vulnerabilities through the
[security policy](.github/SECURITY.md), never in a public issue.

## License

Lictory Pulse is a sibling of [Lictory](https://github.com/chricarp/lictory) and, like it, uses
the [MIT License](LICENSE). Bundled and linked materials retain their own
[third-party notices](THIRD_PARTY_NOTICES.md).
