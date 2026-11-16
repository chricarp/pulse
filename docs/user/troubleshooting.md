# Troubleshooting

Pulse records every event, and for every rule an event matches, a **delivery**: what happened and
why. So "why did this fire?" and "why didn't it?" are answered from the log, not guessed.

## Start here

```sh
pulse status                      # is it running, is it paused, are there config errors or warnings?
pulse sources                     # is the source healthy, and has it seen anything?
pulse events -n 20                # did the event happen at all?
pulse events show <event id>      # what did each rule decide about it, and why?
pulse rules history <rule>        # what has this rule decided recently?
```

`pulse status` lists config problems with file and line:

```
config   1 error(s); the last good config is in effect:
  ~/.pulse/pulse.toml:216:1: rule "typo", field `debunce`: unknown field `debunce`, expected one of `name`, `description`, `on`, `where`, `debounce`, `debounce_by`, `cooldown`, `rate_limit`, `supervised`, `enabled`, `archived`, `run`
```

While your file has an error, the last version that worked stays in effect, so a typo never
silently turns everything off; but your latest change is not active either until you fix it.

## My rule didn't fire

Work down this list.

1. **No event.** `pulse events -s <source>` shows nothing:
   - `pulse sources` says the source is `degraded` or `failed`, and why (a folder that does not
     exist yet, a calendar that cannot be fetched, the inotify watch limit).
   - An `fs` event appears only after the file has been quiet for 2 s (the source's `quiet`).
   - The file is ignored: VCS folders, `node_modules`, editor swap files, `*.tmp`, downloads in
     progress, and your `ignore` globs.
   - Many files at once became one `fs.burst`, not one `fs.file.created` each. A rule on
     `fs.file.created` does not see them; add `fs.burst` to its `on`, or `debounce` the rule.
   - Git: a fast-forward pull, a rebase, or a push to a URL without a remote-tracking branch
     report no commit or push (see [Sources](sources.md#git-a-repository)).
2. **An event, but no delivery for your rule.** `pulse events show <id>` says
   `No rule matched this event.`, or lists other rules but not yours. The event's type is not
   in the rule's `on`, or its `where` does not hold. Check with a dry run:

   ```console
   $ pulse test-rule my-rule --event <id>
   No match: the event's type or `where` does not match this rule (or it is archived).
   ```

   Common causes: a path glob that does not start with `/` or `~` (Pulse warns about it), a
   typo in `on` (Pulse warns: "no event type matches …, so this rule never fires"), `"*"` in a
   path glob not crossing `/` (use `**`), a number written as a string (`exit_code = "1"` is the
   text `"1"`; write `1`), the rule is archived.
3. **A delivery that says `blocked`.** It says why:

   | Reason, as printed | What to do |
   |---|---|
   | `the rule is disabled` | `pulse rules enable <rule>` |
   | `the rule is snoozed until …` | `pulse rules unsnooze <rule>` |
   | `Pulse is paused` | `pulse resume` |
   | `the agent's scopes do not cover it (it would need "…")` | add a scope to the agent's `allow` that covers the event; see [Scopes](agents.md#scopes) |
   | `loop protection: the event was attributed to this rule's own run …` | if you made the change, `pulse replay <event>`; see [Loop protection](loop-protection.md) |
   | `loop protection: a chain of N runs exceeds max_chain_depth M` | runs triggering runs; raise `[limits] max_chain_depth` only if the chain is intended |
   | `the circuit breaker holds the rule until … (it fired too often)` | find out why it fires so often, then `pulse rules unsnooze <rule>` |
   | `cooling down until …` / `rate limited until …` | the rule's `cooldown` / `rate_limit` |
   | `the agent's queue is full (N of M)` | the agent's `max_concurrent` + `max_queued` runs are taken; a rule with a `debounce` waits instead (below) |
   | `the prompt could not be rendered: …` | fix the template |
4. **`debounced until …`** means it is waiting for matching events to stop. A later delivery for
   the same event says `fired` when the debounce ends. **`waiting: the agent's queue is full`**
   means the debounce ended while the agent had no room; it fires when one of the agent's runs
   ends. Debounces pending or waiting when the daemon stops are lost.
5. **`awaiting approval`**: the rule is supervised. `pulse runs list --status pending_approval`.
6. **It fired, but the run failed.** `pulse runs show <id>` shows the error and exactly what was
   run; `pulse runs logs <id>` shows the output. Typical: the program is not on the daemon's
   `PATH` (Pulse warns), the agent's `cwd` does not exist (Pulse warns), the agent was denied a
   tool it needed (Claude Code's runs end with an error naming every denied tool), a `command`
   argument that would start with `-` (add `--` before it; the error says so).
7. **It fired, but the run was cancelled without starting**, with "the agent was removed or
   changed after this run was created" (or the rule was): its agent or rule changed while the run
   waited for approval or a free slot, and a run never starts with a definition that is no
   longer current. `pulse replay <event id>` routes the event again with what is configured now.

## It fired and I don't know why

```sh
pulse runs show <run id>          # the triggering event, rule, chain depth, exact invocation
pulse events show <event id>      # the event, its cause, every delivery
```

The run detail in the UI has the same, plus the prompt split into the rule's instruction and the
fenced event data, and a snapshot of the rule and agent as they were when it ran.

If an event says it was caused by a run (`from run …` in the CLI), that run's activity is what
triggered it. `~` means the cause was inferred, not observed (see [Loop protection](loop-protection.md#how-pulse-knows-who-caused-an-event)).

## A save or `add` is refused

- **"a rule named … already exists; pass --replace"**: `pulse rules add` (and `agents add`,
  `sources add`) only create. Use `--replace` to replace the existing one.
- **"This rule changed while you were editing it"** in the UI: someone (you in an editor, the
  CLI, another window) changed it after the editor opened. Your form is kept; choose **Reload**
  or **Save mine anyway**. See [Rules](rules.md#editing-while-something-else-edits).
- **"… changed since that hash was printed"** from `pulse project trust --hash`: the project file
  changed after you reviewed it. Run `pulse project trust <dir>` again and read what it says now.
- **"project … changed since it was trusted"** when editing a project's rule: the file changed
  (a `git pull`) and is inactive until you review and trust it again.

## Something keeps firing

1. `pulse pause` stops all firing at once (queued runs wait too). Running runs continue:
   `pulse runs list --status running`, then `pulse runs cancel <id>`.
2. `pulse rules history <rule>` shows what it fired on. If the events are the agent's own writes
   and were not attributed to it, the agent is probably writing outside its `cwd`; see
   [Loop protection](loop-protection.md#how-pulse-knows-who-caused-an-event).
3. Add `cooldown`, `rate_limit` or `debounce` to the rule, or tighten its `where`.
4. `pulse resume`.

## The UI says "log in again" or shows nothing

The browser holds the daemon's token for one tab. Run `pulse open` for a new tab, or after the
two-minute code expired. If the daemon is not running, the UI says so and reconnects when it is
back.

## The daemon's own log

`$PULSE_HOME/logs/pulse.log` (rotated at 10 MB, five old files kept). At the default `info` level
it records starts and stops, config reloads and their errors, and little else. It never contains
tokens, request bodies or event content. Set `[daemon] log_level = "debug"` and restart for
more.

## Port 7477 is taken

Set `[daemon] port` in `pulse.toml` (restart the daemon), or start with `pulse start --port
<port>`. `port = 0` picks a free one; clients find it in `$PULSE_HOME/daemon.json`.
