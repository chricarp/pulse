# CLI reference

One binary, `pulse`. `pulse start` runs the daemon; every other command is a client of it, except
`init` (writes a file) and `run` (works, silently, without a daemon). This page follows
`pulse --help` for version 0.1.0.

Global options, accepted by every command:

| Option | Meaning |
|---|---|
| `--home <DIR>` | The Pulse home. Default: `$PULSE_HOME`, else `~/.pulse`. |
| `--json` | Print JSON instead of text, for scripts: see [JSON output](#json-output). |
| `-h`, `--help` / `-V`, `--version` | |

Exit codes: `0` success, `1` error, `2` usage error, `3` the daemon is not running. `pulse run`
exits with the wrapped command's code instead (see below).

Text output escapes control and invisible characters from events and run output (`\u{1b}`), so a
hostile file name cannot recolour, retitle or write to your terminal.

## JSON output

With `--json`, every command except `pulse run` and `pulse mcp` keeps this contract, whether it
succeeds or fails, with a daemon or without:

- **stdout carries only JSON**, as the daemon's types serialize (never escaped for a terminal).
  A command that answers once prints one document. The streams print one compact object per
  line: `tail` prints each notification (`events.batch`, `runs.changed`, `state.changed`) and
  `runs logs` each output chunk.
- **On success** the exit code is `0` and stdout is the answer. Commands that only do something
  answer with what they did: `stop` with `{"was_running": true|false}`, `init` with
  `{"path", "created", "added"}` (`--list` with `{"templates": […]}`), `start` (with or without
  `--detach`) with `{"pid", "port"}` once the daemon listens, `remove` with `{"removed": NAME}`.
- **On failure** the exit code is `1`, `2` (usage) or `3` (no daemon), stdout is one error object,
  and stderr is empty. For a stream, the error object is the last line.
- **stderr** is for people in both modes (`tail` says it is following, `start` prints config
  problems, `events` the next cursor in text mode). A program never needs it.

The error object:

```json
{
  "error": {
    "kind": "stale",
    "message": "a rule named \"r\" already exists; pass --replace to replace it",
    "code": -32007,
    "data": { "current": "9f2c…" }
  }
}
```

| Field | Meaning |
|---|---|
| `kind` | What went wrong, as a word (below). |
| `message` | For people. |
| `code` | The daemon's JSON-RPC error code, when the daemon refused the request. |
| `data` | What the daemon attached, or what the CLI knows more; absent when there is nothing. |

| `kind` | Exit | When | `data` |
|---|---|---|---|
| `usage` | 2 | The command line is wrong (unknown command, missing argument). | |
| `not_running` | 3 | No daemon for this home. | |
| `invalid_params` | 1 | An argument or input is invalid, whether the CLI or the daemon found it. | |
| `not_found` | 1 | No such rule, agent, source, project, event or run. | |
| `stale` | 1 | What the request was based on is no longer so: `add` without `--replace` when the name exists, `project trust --hash` when the file changed. | `{"current": REVISION}` |
| `config_invalid` | 1 | The change would make a config file invalid (or `init --template` would). | `{"errors": [{"file", "line", "column", "item", "field", "message"}]}` |
| `conflict` | 1 | The request conflicts with state: an illegal run transition, an agent still used by a rule, a daemon already running for this home (`start`). | |
| `not_trusted` | 1 | `project trust` without `--hash`: a review, which trusts nothing. | the review: what it would load, what changed, its `hash` |
| `not_pulse` | 1 | Something else listens on Pulse's port; the token was not sent to it. | |
| `connection` | 1 | The connection to the daemon failed or closed (a `tail` whose daemon stopped). | |
| `io` | 1 | A local file or directory could not be read or written. | |
| `unavailable`, `internal`, `unauthorized`, `not_implemented` | 1 | The daemon's other error codes. | as sent |

`pulse run` is the wrapped command: its output and exit code are the command's, and `--json`
changes nothing. `pulse mcp` keeps stdout for MCP messages; its failures are text on stderr.

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
| `pulse tail [filters] [--no-runs]` | Follow events live, and the state changes of the runs they cause, always after the event. With filters, only the runs of the events shown (and of events from before `tail` started). Ctrl-C stops. |
| `pulse events [filters] [-n/--limit <N>] [--cursor <C>] [--oldest-first] [--since <T>]` | Query the log, newest first, 50 at a time. `--since` takes RFC 3339 or a duration ago (`2h`). A page ends with `More: pulse events --cursor …` when there is more. |
| `pulse events show <ID>` | One event: its data, its cause, every rule it reached and why, the runs it caused. |
| `pulse replay <EVENT> [--rule <RULE>]…` | Re-emit a past event as a new one (with no cause) and route it, to every rule or only the named ones. |
| `pulse run [--tail-bytes <N>] -- <COMMAND>…` | Run a command as if directly, then report `process.exited` with the last `N` bytes (default 8192) of its output. Exits with the command's code (128 + signal if it was killed, 127 if it could not be started). Silent if the daemon is not running. |
| `pulse ingest -s/--source <NAME>` | Send external events, one JSON object (or CloudEvent) per stdin line, typed `ext.<NAME>.<type>`, parsed as `POST /ingest` parses them. A bad line stops it; the lines before it were sent. See [Sources](sources.md#external-events-pulse-ingest-and-post-ingest). |

Filters for `tail` and `events`:

| Option | Meaning |
|---|---|
| `-t, --type <GLOB>` | event type glob, repeatable (`-t 'fs.*'`) |
| `-s, --source <NAME>` | source name or `pulse://` URI, repeatable |
| `--subject <GLOB>` | glob over the subject |
| `--path <PATH>` | events carrying a path under this directory |
| `--text <TEXT>` | case-insensitive text in the subject or data |

## Rules

`pulse rules [COMMAND]`; without one, `list`.

| Command | Does |
|---|---|
| `list [--all]` | Active rules with their state; `--all` includes archived ones. |
| `show <NAME>` | The definition as TOML, the file it is in, its state, its last fire. |
| `add -f/--file <FILE> [--project <P>] [--replace]` | Add a rule from a TOML file (the bare table `show` prints). `-` reads stdin. Refused if a rule of that name exists, unless `--replace`. Written to `pulse.toml` (or the project's), comments kept. Prints warnings if it can never fire. |
| `remove <NAME>` | Delete it from its file. History stays in the log. |
| `enable <NAME>` / `disable <NAME>` | Set `enabled` in the file. |
| `snooze <NAME> <FOR>` | Hold it for a duration (`1h`, `30m`) or until an RFC 3339 time. |
| `unsnooze <NAME>` | End a snooze, and release a tripped circuit breaker. |
| `archive <NAME>` / `reopen <NAME>` | Hide it and stop routing to it, keeping its history; and back. |
| `history <NAME> [--outcome <O>]… [-n <N>]` | Its decisions, newest first. `--outcome` is `fired`, `awaiting-approval`, `debounced`, `waiting` or `blocked`. A loop-protection block says how its event's cause was known (`exact`, `reported`, `inferred`) and is followed by the `pulse replay` command that undoes it. |

`pulse test-rule [RULE] -e/--event <ID> [-f/--file <FILE>] [--project <P>] [--fresh]`: a dry run of
a saved rule, or of an unsaved draft from a file, against a past event. Prints the decision and,
if it would fire, the full prompt. `--fresh` ignores the rule's history (snooze, cooldown, rate
limit), not the event's recorded cause: loop protection still applies to an event caused by a
run, and the output says so with the `pulse replay` command for it. Records nothing and runs
nothing.

## Runs

`pulse runs [COMMAND]`; without one, `list`.

| Command | Does |
|---|---|
| `list [--rule <R>] [--agent <A>] [--status <S>]… [-n <N>]` | Runs, newest first. Statuses: `pending_approval`, `queued`, `running`, `succeeded`, `failed`, `timed_out`, `cancelled`, `rejected`, `interrupted`. |
| `show <ID>` | The event, timing, exactly what was run (or, before it starts, will run) and where, the prompt, any error. |
| `logs <ID> [-f/--follow]` | The run's output; `-f` follows it until it ends. |
| `approve <ID>` | Start a run that is waiting for approval. Refused, and the run cancelled, if its agent or rule changed since it was created. |
| `reject <ID> [--reason <TEXT>]` | Refuse it. |
| `cancel <ID>` | Stop a queued or running run (its whole process group). |
| `rerun <ID>` | A new run with the same event and prompt, now, with the agent as it is configured now. Skips approval, cooldown and rate limit. Refused if the agent no longer exists. |

Wherever a command takes a run or event id (`runs …`, `events show`, `replay`, `test-rule
--event`), the last characters of the id are enough, in either case, from three on: the eight the
UI shows (`BKV2DFS1` for `01KAC4ZJ6E5W8Q0N2HBKV2DFS1`), or fewer (`DFS1`). Ids made close together
share their first characters, so the end is the part that tells them apart. A short id is looked up
among the newest 10,000 runs (or events), the "recent" ones; an older one needs the full id. If
more than one id ends in what you typed, nothing is done and the candidates are listed (under
`--json`, an `invalid_params` error with `data.candidates`); if none does, it is `not_found`. A
full id is used as it is.

## Agents, sources, projects

| Command | Does |
|---|---|
| `pulse agents` / `pulse agents list` | Agents, their running and queued counts, their scopes. |
| `pulse agents show <NAME>` | The definition as TOML. |
| `pulse agents add -f/--file <FILE> [--project <P>] [--replace]` | Add an agent from a TOML file; `-` reads stdin. Refused if the name exists, unless `--replace`. |
| `pulse agents remove <NAME>` | Delete it. Refused while a rule uses it. |
| `pulse agents mcp-credentials <NAME>` | The `/mcp` URL, the agent's MCP token, and commands to paste into Claude Code and Codex. See [MCP](mcp.md). |
| `pulse mcp --agent <AGENT>` | A stdio bridge to the daemon's `/mcp` as that agent, for MCP clients that launch a server. Stdout carries MCP messages only. |
| `pulse sources` / `pulse sources list` | Sources with their health and event counts. |
| `pulse sources add <KIND> <NAME> [TARGET] […] [--replace]` | Add a source (refused if the name exists, unless `--replace`): `fs NAME DIR [--ignore <GLOB>]…`, `git NAME REPO`, `schedule NAME --cron "0 9 * * 1-5" [--timezone <TZ>]` or `--every 1h`, `calendar NAME FILE.ics|URL [--lead <D>]`. `--project <P>` writes it to a trusted project's file. |
| `pulse sources enable <NAME>` / `disable <NAME>` | Set `enabled` in its file: stop watching, keeping the definition, and back. |
| `pulse sources remove <NAME>` | Delete it. |
| `pulse project trust <DIR> [--name <NAME>] [--hash <HASH>]` | Without `--hash`: show what `DIR/pulse.toml` would load, what changed since it was trusted, and its content hash; trust nothing (exit status 1). With `--hash`: trust it only if it still has that content. `--name` is the prefix for its definitions (default: the directory's name). See [Projects](projects.md). |
| `pulse project untrust <DIR>` | Make it inactive. |
| `pulse project` / `pulse project list` | Projects and their trust state. |

`agents add`, `rules add` and `sources add` edit the same file as the UI and your editor, through
the daemon, so it must be running. They create; `--replace` replaces a definition of that name
whatever it is now. `remove` removes whatever is there under that name.
