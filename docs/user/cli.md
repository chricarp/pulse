# CLI reference

One binary, `pulse`. `pulse start` runs the daemon; every other command is a client of it, except
`init` (writes a file) and `run` (works, silently, without a daemon). This page follows
`pulse --help` for version 0.1.0.

Global options, accepted by every command:

| Option | Meaning |
|---|---|
| `--home <DIR>` | The Pulse home. Default: `$PULSE_HOME`, else `~/.pulse`. |
| `--json` | Print JSON instead of text (for scripts). |
| `-h`, `--help` / `-V`, `--version` | |

Exit codes: `0` success, `1` error, `2` usage error, `3` the daemon is not running. `pulse run`
exits with the wrapped command's code instead (see below).

Text output escapes control and invisible characters from events and run output (`\u{1b}`), so a
hostile file name cannot recolour, retitle or write to your terminal. `--json` output is plain
JSON.

## The daemon

| Command | Does |
|---|---|
| `pulse start [-d/--detach] [--port <PORT>]` | Run the daemon, in the foreground unless `--detach` (which returns once it listens). `--port` overrides `[daemon] port`. Refuses if a daemon already runs for this home. Prints config errors and warnings first. |
| `pulse stop` | Stop the daemon and wait until it has exited. Running runs are terminated and recorded as `interrupted`. |
| `pulse status` | Version, port, home, pause state, counts of rules, agents, sources and runs, and config errors and warnings. |
| `pulse pause` | Rules are still evaluated and recorded, but nothing fires, and queued runs wait. |
| `pulse resume` | Undo `pause`. |
| `pulse open [--print]` | Open the web UI in the browser, signed in with a one-time code valid for two minutes. `--print` prints the URL instead. |
| `pulse init [-t/--template <NAME>]… [--list]` | Write a commented starter `pulse.toml` (if there is none) and append the named templates. Refuses, writing nothing, if the result would not be valid. |

## Events

| Command | Does |
|---|---|
| `pulse tail [filters] [--no-runs]` | Follow events, and run state changes, live. Ctrl-C stops. |
| `pulse events [filters] [-n/--limit <N>] [--cursor <C>] [--oldest-first] [--since <T>]` | Query the log, newest first, 50 at a time. `--since` takes RFC 3339 or a duration ago (`2h`). A page ends with `More: pulse events --cursor …` when there is more. |
| `pulse events show <ID>` | One event: its data, its cause, every rule it reached and why, the runs it caused. |
| `pulse replay <EVENT> [--rule <RULE>]…` | Re-emit a past event as a new one (with no cause) and route it, to every rule or only the named ones. |
| `pulse run [--tail-bytes <N>] -- <COMMAND>…` | Run a command as if directly, then report `process.exited` with the last `N` bytes (default 8192) of its output. Exits with the command's code (128 + signal if it was killed, 127 if it could not be started). Silent if the daemon is not running. |
| `pulse ingest -s/--source <NAME>` | Send external events, one JSON object per stdin line, typed `ext.<NAME>.<type>`. See [Sources](sources.md#external-events-pulse-ingest-and-post-ingest). |

Filters for `tail` and `events`:

| Option | Meaning |
|---|---|
| `-t, --type <GLOB>` | event type glob, repeatable (`-t 'fs.*'`) |
| `-s, --source <NAME>` | source name or `pulse://` URI, repeatable |
| `--subject <GLOB>` | glob over the subject |
| `--path <PATH>` | events carrying a path under this directory |
| `--text <TEXT>` | case-insensitive text in the subject or data |

## Rules

`pulse rules <COMMAND>`; a subcommand is required.

| Command | Does |
|---|---|
| `list [--all]` | Active rules with their state; `--all` includes archived ones. |
| `show <NAME>` | The definition as TOML, the file it is in, its state, its last fire. |
| `add -f/--file <FILE> [--project <P>]` | Add a rule from a TOML file (the bare table `show` prints), or replace the rule of that name. `-` reads stdin. Written to `pulse.toml` (or the project's), comments kept. Prints warnings if it can never fire. |
| `remove <NAME>` | Delete it from its file. History stays in the log. |
| `enable <NAME>` / `disable <NAME>` | Set `enabled` in the file. |
| `snooze <NAME> <FOR>` | Hold it for a duration (`1h`, `30m`) or until an RFC 3339 time. |
| `unsnooze <NAME>` | End a snooze, and release a tripped circuit breaker. |
| `archive <NAME>` / `reopen <NAME>` | Hide it and stop routing to it, keeping its history; and back. |
| `history <NAME> [--outcome <O>]… [-n <N>]` | Its decisions, newest first. `--outcome` is `fired`, `awaiting-approval`, `debounced` or `blocked`. A loop-protection block is followed by the `pulse replay` command that undoes it. |

`pulse test-rule [RULE] -e/--event <ID> [-f/--file <FILE>] [--project <P>] [--fresh]`: a dry run of
a saved rule, or of an unsaved draft from a file, against a past event. Prints the decision and,
if it would fire, the full prompt. `--fresh` ignores the rule's history (snooze, cooldown, rate
limit). Records nothing and runs nothing.

## Runs

`pulse runs <COMMAND>`; a subcommand is required.

| Command | Does |
|---|---|
| `list [--rule <R>] [--agent <A>] [--status <S>]… [-n <N>]` | Runs, newest first. Statuses: `pending_approval`, `queued`, `running`, `succeeded`, `failed`, `timed_out`, `cancelled`, `rejected`, `interrupted`. |
| `show <ID>` | The event, timing, exactly what was run and where, the prompt, any error. |
| `logs <ID> [-f/--follow]` | The run's output; `-f` follows it until it ends. |
| `approve <ID>` | Start a run that is waiting for approval. |
| `reject <ID> [--reason <TEXT>]` | Refuse it. |
| `cancel <ID>` | Stop a queued or running run (its whole process group). |
| `rerun <ID>` | A new run with the same event, prompt and configuration snapshot, now. Skips approval, cooldown and rate limit. |

Run and event ids are the full 26-character ids; prefixes are not accepted.

## Agents, sources, projects

| Command | Does |
|---|---|
| `pulse agents` / `pulse agents list` | Agents, their running and queued counts, their scopes. |
| `pulse agents show <NAME>` | The definition as TOML. |
| `pulse agents add -f/--file <FILE> [--project <P>]` | Add or replace an agent from a TOML file; `-` reads stdin. |
| `pulse agents remove <NAME>` | Delete it. Refused while a rule uses it. |
| `pulse agents mcp-credentials <NAME>` | The `/mcp` URL, the agent's MCP token, and commands to paste into Claude Code and Codex. See [MCP](mcp.md). |
| `pulse mcp --agent <AGENT>` | A stdio bridge to the daemon's `/mcp` as that agent, for MCP clients that launch a server. Stdout carries MCP messages only. |
| `pulse sources` / `pulse sources list` | Sources with their health and event counts. |
| `pulse sources add <KIND> <NAME> [TARGET] […]` | Add or replace a source: `fs NAME DIR [--ignore <GLOB>]…`, `git NAME REPO`, `schedule NAME --cron "0 9 * * 1-5" [--timezone <TZ>]` or `--every 1h`, `calendar NAME FILE.ics|URL [--lead <D>]`. `--project <P>` writes it to a trusted project's file. |
| `pulse sources remove <NAME>` | Delete it. |
| `pulse project trust <DIR> [--name <NAME>]` | Trust `DIR/pulse.toml` as it is now. `--name` is the prefix for its definitions (default: the directory's name). |
| `pulse project untrust <DIR>` | Make it inactive. |
| `pulse project list` | Projects and their trust state. |

`agents add`, `rules add` and `sources add` edit the same file as the UI and your editor, through
the daemon, so it must be running.
