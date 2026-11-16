# The config file

Everything Lictory Pulse watches and does is defined in one TOML file, `$PULSE_HOME/pulse.toml`
(`~/.pulse/pulse.toml` by default), plus any [trusted project files](projects.md). The UI and the
CLI edit this same file: there is no second copy of a rule anywhere.

- **The daemon reloads it when you save it.** A change settles for 150 ms, then the whole file
  is parsed and checked. `kill -HUP <daemon pid>` also reloads.
- **A broken file does not stop Pulse.** If the file no longer parses, its last good version
  stays in effect and the errors (file, line, item, field) are shown by `pulse status`, in the
  UI, and in `$PULSE_HOME/logs/pulse.log`.
- **Values a source cannot use are errors too**, reported the same way when the file loads or
  when the UI or CLI saves: a cron step larger than its field, an unknown time zone, an
  `every`, `lead` or `refresh` over its limit, an `ignore` glob that is not a clean path. A
  source does not start with them and then fail.
- **Unknown keys are errors.** A misspelt key is reported, never silently ignored. This page
  lists every key the parser accepts; the authoritative schema is
  [`crates/pulse-contracts/src/config/mod.rs`](../../crates/pulse-contracts/src/config/mod.rs).
- **Comments survive.** When the UI or CLI changes a definition, only the keys whose meaning
  changed are rewritten; your comments and ordering stay. A new definition is added at the end
  of the file. Removing one removes the comments directly above and below it (with no blank line
  in between); comments separated from it by a blank line, like the file's header, stay.
- **Warnings** are for config that loads but cannot work: a rule whose `on` matches no event
  type, a `where` key no matching event has, a relative glob on a path field, scopes that can
  never cover the rule's events, an agent program that is not installed, a `cwd` that does not
  exist. They are printed by `pulse start` and `pulse status` and shown in the UI.

`pulse init` writes a commented starter file. `pulse init --template <name>` appends a shipped
template (`pulse init --list` shows them).

## Values

| Kind | Syntax | Examples |
|---|---|---|
| Duration | a number and a unit (`ms`, `s`, `m`, `h`, `d`), units may be combined largest first | `"500ms"`, `"10s"`, `"15m"`, `"1h30m"`, `"7d"` |
| Rate | `<count>/<period>`, count 1 to 10000; period `s`, `m`, `h`, `d` (or `second`, `minute`, `hour`, `day`, `sec`, `min`) or a duration | `"10/hour"`, `"3/minute"`, `"5/10m"`, `"100/day"` |
| Name | 1 to 64 of `a-z`, `0-9`, `-`, `_`, starting with a letter or digit | `"lecture-notes"` |
| Path | absolute, or starting with `~/` (your home directory). Relative paths are an error in your own file and resolve against the project directory in a project file | `"~/University"` |
| Type glob | an event type, where `*` matches anything including dots | `"fs.file.created"`, `"fs.*"`, `"git.*"`, `"ext.ci.*"` |
| Path glob | `*` (within one path segment), `**` (any number of segments; `/a/**` also matches `/a`), `?`, `{a,b}` | `"~/University/**/*.pdf"` |

## `[daemon]`

Only in your own `pulse.toml`, not in project files.

| Key | Default | Meaning |
|---|---|---|
| `port` | `7477` | TCP port on `127.0.0.1`. `0` picks a free port (written to `daemon.json`). Read at start: restart the daemon after changing it. `pulse start --port` overrides it. |
| `log_level` | `"info"` | `error`, `warn`, `info`, `debug` or `trace`. Read at start. |
| `mcp_events_extension` | `false` | Serve the draft MCP Events extension (SEP-3415). Experimental; read at start. See [MCP](mcp.md). |

## `[limits]`

Only in your own `pulse.toml`. Bounds for the engine and for loop protection.

| Key | Default | Range | Meaning |
|---|---|---|---|
| `max_chain_depth` | `3` | 1–32 | At most this many runs in one causal chain (a run caused by an event caused by a run…). |
| `causation_grace` | `"30s"` | | How long after a run ends files in its working directory are still attributed to it. See [loop protection](loop-protection.md). |
| `dedupe_ttl` | `"10m"` | > 0 | How long an event's dedupe key suppresses repeats (external events). |
| `dedupe_capacity` | `10000` | 1–1000000 | Dedupe keys remembered at once; oldest forgotten first. |
| `debounce_capacity` | `10000` | 1–1000000 | Pending debounce timers at once, including debounces waiting for a full agent. Past it, new events skip the debounce but still pass cooldown and rate limits. Read at start. |
| `run_output_max_bytes` | `4194304` (4 MiB) | 4096–2³² | Output kept per run. Past it the first half and the most recent output are kept, with a marker saying how much was dropped. |

`[limits.circuit_breaker]`: a rule that fires more than `max_fires` times within `window` is held
for `open_for`. This works whether or not loop attribution is right. When the hold ends, or
`pulse rules unsnooze` ends it, the breaker counts only fires from then on.

| Key | Default | Range |
|---|---|---|
| `max_fires` | `20` | 1–10000 |
| `window` | `"10m"` | > 0 |
| `open_for` | `"1h"` | > 0 |

`[limits.prompt]`: size limits for rendered prompts.

| Key | Default | Range | Meaning |
|---|---|---|---|
| `inline_max_chars` | `200` | 16–100000 | A `{{…}}` value substituted into the instruction is cut to this many characters. |
| `field_max_bytes` | `4096` | 64–1000000 | Each string in the fenced event block is cut to this many bytes. |
| `data_max_bytes` | `32768` | 1024–4000000 | The whole fenced event block is at most this many bytes. |

## `[retention]`

Only in your own `pulse.toml`. Pruning runs at start and then hourly, so the log can exceed these
limits by about an hour's worth of events. The database file does not shrink; SQLite reuses the
freed space.

| Key | Default | Meaning |
|---|---|---|
| `events_max_age` | `"30d"` | Events older than this are pruned… |
| `events_max_count` | `1000000` | …and so are the oldest past this count. |
| `runs_max_age` | `"90d"` | Finished runs older than this are pruned with their output and their working directories under `runs/`. An event a kept run refers to is kept until then. |

## `[[source]]`

One table per source. Keys every source has:

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Unique name. Events carry it as `source = "pulse://<kind>/<name>"`. |
| `kind` | required | `fs`, `git`, `schedule` or `calendar`. |
| `enabled` | `true` | `false` stops the source. |

`kind = "fs"`: a directory tree.

| Key | Default | Meaning |
|---|---|---|
| `path` | required | The directory to watch. |
| `ignore` | `[]` | Globs added to the built-in ignores. No `/`: a name at any depth (`"*.log"`, `"build"`). A leading `/` or `~`: absolute. Otherwise relative to `path`. |
| `recursive` | `true` | Watch subdirectories. |
| `quiet` | `"2s"` | A path must be quiet this long before its change is reported. |
| `burst_threshold` | `200` | Pending changes under one directory beyond which they fold into one `fs.burst` (1–1000000). |

`kind = "git"`: one repository.

| Key | Default | Meaning |
|---|---|---|
| `path` | required | The repository's working tree. |

`kind = "schedule"`: set exactly one of `cron` and `every`.

| Key | Default | Meaning |
|---|---|---|
| `cron` | | Five fields: minute hour day-of-month month day-of-week, e.g. `"0 9 * * 1-5"`. A step must fit its field (`*/90` in the minute field is an error; use `every = "90m"`), and a day none of its months has (`"0 0 30 2 *"`) is an error. |
| `every` | | A fixed interval, e.g. `"30m"`. At most `"3650d"`. |
| `timezone` | the system zone | IANA zone name for `cron`, e.g. `"Europe/Rome"`. An unknown name is an error. |

`kind = "calendar"`: set exactly one of `url` and `path`.

| Key | Default | Meaning |
|---|---|---|
| `url` | | An `http://` or `https://` ICS feed. |
| `path` | | A local `.ics` file. |
| `refresh` | `"15m"` | How often to re-read it. At most `"365d"`. |
| `lead` | `"10m"` | How long before an event starts `calendar.event.starting` fires. At most `"365d"`. |

What each source emits: [Sources](sources.md).

## `[[agent]]`

Keys every agent has:

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Unique name. |
| `adapter` | required | `claude-code`, `codex`, `command` or `mcp`. |
| `allow` | `[]` | Permission scopes: `"<type glob>"` or `"<type glob>:<path glob>"`. **Default deny**: an agent with no scopes receives nothing. See [Agents and permissions](agents.md#scopes). |
| `cwd` | a new directory per run, `$PULSE_HOME/runs/<run id>/` | Working directory for runs. |
| `timeout` | `"15m"` | A run still going after this is terminated and recorded `timed_out`. |
| `max_concurrent` | `1` | Runs of this agent executing at once (1–64). |
| `max_queued` | `8` | Runs waiting for a slot (0–10000). Past this, new fires are blocked as `agent_busy`; a debounced rule's fire waits for a run to end instead. |
| `env` | `{}` | Extra environment variables for runs, e.g. `env = { RUST_LOG = "info" }`. Names starting with `PULSE_` are reserved. |

`adapter = "claude-code"`:

| Key | Default | Meaning |
|---|---|---|
| `program` | `claude` on `PATH` | The executable. |
| `model` | the CLI's default | Passed as `--model=`. |
| `permission_mode` | `"dontAsk"` | Passed as `--permission-mode=`. `dontAsk` denies anything not pre-approved without asking. |
| `allowed_tools` | `[]` | Each passed as `--allowedTools=<rule>`, e.g. `"Read"`, `"Edit(./**)"`, `"Bash(git diff:*)"`. |
| `disallowed_tools` | `[]` | Each passed as `--disallowedTools=<rule>`. |
| `extra_args` | `[]` | Appended to the command line as given. |

`adapter = "codex"`:

| Key | Default | Meaning |
|---|---|---|
| `program` | `codex` on `PATH` | The executable. |
| `model` | the CLI's default | Passed as `--model=`. |
| `sandbox` | `"read-only"` | Passed as `--sandbox=`. |
| `approval_policy` | `"never"` | Passed as `--config=approval_policy="…"`. Must be a bare word. |
| `extra_args` | `[]` | Appended as given, e.g. `["--skip-git-repo-check"]` to run outside a git repository. |

`adapter = "command"`:

| Key | Default | Meaning |
|---|---|---|
| `command` | required | The argv, as an array. Never run through a shell. Arguments after the first may contain `{{…}}` placeholders; the program may not. |
| `stdin` | `"prompt"` | What the process reads on stdin: `"prompt"` (the rendered prompt, if the rule has one), `"event"` (the event as JSON) or `"none"`. |

`adapter = "mcp"` has no further keys. It is never run; it is an identity with scopes for an MCP
client (see [MCP](mcp.md)). Rules cannot target it.

## `[[rule]]`

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Unique name. History refers to rules by name. |
| `description` | | Free text, shown in the UI. |
| `on` | required | A type glob or a list of them: `"fs.file.created"`, `["fs.file.created", "fs.file.moved"]`. |
| `where` | `{}` | Conditions on the event, all of which must hold. See below. |
| `debounce` | none | Wait until matching events stop for this long, then fire once with the latest. |
| `debounce_by` | `"rule"` | `"rule"`: one timer for the rule. `"subject"`: one timer per subject (each file fires once when it settles). |
| `cooldown` | none | Minimum time between fires. |
| `rate_limit` | none | At most this many fires per period, e.g. `"10/hour"`. |
| `supervised` | `false` | Every run waits for a person to approve it. |
| `enabled` | `true` | `false` turns the rule off (its matches are still recorded as blocked). |
| `archived` | `false` | Hidden from active lists and never routed; history is kept. |
| `run.agent` | required | The agent to wake. |
| `run.prompt` | | The instruction. Required for `claude-code` and `codex` agents, optional for `command`. |

`run` can be written inline (`run = { agent = "study", prompt = "…" }`) or as a `[rule.run]`
table after the rule's other keys.

### `where`

Keys name a field of the event: `subject`, `source` (the source's name, e.g. `"university"`),
`type`, or a field of the event's `data` (`path` and `data.path` mean the same; nested fields use
dots, `data.command.0` indexes an array). A field the event does not have never matches, except
with `{ exists = false }`.

| Value | Means | Example |
|---|---|---|
| a string | a glob. Path-aware when it starts with `/` or `~` | `path = "~/University/**/*.pdf"`, `name = "*.pdf"` |
| a number or boolean | equality | `success = false`, `exit_code = 101` |
| a list | any of these | `ext = ["pdf", "djvu"]` |
| a table | every operator holds: `eq`, `ne` (literal, not glob), `gt`, `gte`, `lt`, `lte` (numbers), `in` (a list of literals), `contains` (substring, or array membership), `glob`, `exists` | `exit_code = { ne = 0 }`, `size = { gt = 1000000 }`, `files = { exists = true }` |

When a field holds an array, a condition matches if any element matches (except `contains` and
`exists`, which look at the array itself).

A string that does not start with `/` or `~` is a plain text glob, in which `*` also matches `/`:
`path = "*.pdf"` matches every PDF anywhere. A relative path glob such as
`path = "University/*.pdf"` never matches, because event paths are absolute; Pulse warns about it.

### Prompt templates

`{{name}}` placeholders in `run.prompt` and in a `command` agent's arguments:
`{{id}}`, `{{type}}`, `{{source}}`, `{{subject}}`, `{{time}}`, `{{rule}}`, `{{agent}}`, and any
field of the event's data, as `{{path}}` or `{{data.path}}`. In `command` arguments only,
`{{prompt}}` is the rendered prompt. A missing field renders as nothing. There is no escape for a
literal `{{`.

Values substituted into the instruction are reduced to one line, stripped of control and
invisible characters, and cut to `inline_max_chars`. The full event is always appended to the
prompt as fenced, untrusted data; see [Security](../../README.md#security-model).
