# Project files and trust

A repository can carry its own `pulse.toml` with sources, agents and rules for that project, so a
team can share "when the build fails, diagnose it". Cloning a repository must never be enough to
run a command, so a project file does nothing until you trust it, and only for the exact content
you trusted.

## Trusting a project

Trusting is two steps: look, then trust exactly what you looked at.

```sh
pulse project trust ~/code/notes-app            # shows what it would load; trusts nothing
pulse project trust ~/code/notes-app --hash 3f9c…   # trusts that content, and only that
pulse project trust ~/code/notes-app --name notes   # the prefix for its definitions
pulse project list
pulse project untrust ~/code/notes-app
```

Without `--hash`, `pulse project trust` prints what the file would add (each agent with the
program or command it runs, argument by argument, its tools, scopes, working directory and
environment; each rule with its triggers, whether it needs your approval, and its prompt; each
source), what changed since you last trusted it, and its content hash, and then prints the exact
command that trusts it. It exits with status 1, because nothing was trusted. With `--hash`, Pulse
trusts the file only if it still has that content; if it changed in between (a `git pull` while
you were reading), nothing is trusted and you are asked to look again. With `--json`, the review
is the `data` of a `not_trusted` error (exit 1, see [JSON output](cli.md#json-output)); its
`hash` is what to pass back.

In the UI, **Projects → Trust** (or **Review and trust again** for a changed file) shows the same
review before the **Trust this content** button does anything. If the file changes while the
dialog is open, the button trusts nothing and the dialog shows the new content instead.

- Trust is bound to the file's **content hash**. When the file changes (a `git pull`, a branch
  switch, your own edit) the project becomes **changed** and is inactive until you trust it
  again. A deleted file shows as **missing**. `pulse project list` and the Projects view show the
  state.
- Changes you make through Pulse itself (the UI or `pulse rules add --project …`) re-bind the
  trust to the new content, since you made them. If the file changed underneath first, the change
  is refused instead, so nothing you did not review becomes trusted along with it.
- Runs of a project's agents that are still queued or waiting for approval when the project is
  untrusted or its file changes do not start (see [Rules](rules.md#when-a-definition-changes)).
- Pulse keeps the text you trusted (files up to 64 KiB) to show what changed. Trust recorded
  before this was added cannot show changes; the review then shows the whole file.
- Trusting requires the file to parse. A trusted file that later fails to parse is inactive (a
  project file has no "last good version").
- The project's name defaults to the directory's name and must be a valid name (lowercase
  letters, digits, `-`, `_`).

## What a project file may contain

- `[[source]]`, `[[agent]]` and `[[rule]]`, with the same keys as your own file.
- Not `[daemon]`, `[limits]` or `[retention]`: those are only yours.
- Names are prefixed with the project's name: rule `build-failed` in project `notes` is
  `notes/build-failed` everywhere (CLI, UI, history).
- A project rule's `run.agent` names the project's own agent if it defines one by that name,
  otherwise one of yours. So a project can define rules that wake your `diagnose` agent with your
  scopes.
- Relative paths (`path = "docs"`, `cwd = "."`) resolve against the project directory.
  **Scopes** (`allow`) must still be absolute or start with `~/`.

## Adding to a project from the CLI

```sh
pulse rules add --file rule.toml --project notes
pulse agents add --file agent.toml --project notes
pulse sources add fs docs ~/code/notes-app/docs --project notes
```

## What trust does not do

Trusting a project file is trusting whoever wrote it, for that content: its rules can start
agents with its prompts. Read it before trusting it, as you would a script. The review shows what
the file defines, not what a program it names will do: a rule that runs `make generate` is only
as safe as the repository's Makefile, which trust does not cover. A repository used as
an agent's working directory can also bring its own agent settings (`.claude/settings.json`),
which Pulse does not control; see [Agents](agents.md#what-pulse-passes-to-the-agent).
