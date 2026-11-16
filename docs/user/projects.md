# Project files and trust

A repository can carry its own `pulse.toml` with sources, agents and rules for that project, so a
team can share "when the build fails, diagnose it". Cloning a repository must never be enough to
run a command, so a project file does nothing until you trust it, and only for the exact content
you trusted.

## Trusting a project

```sh
pulse project trust ~/code/notes-app            # reads ~/code/notes-app/pulse.toml
pulse project trust ~/code/notes-app --name notes
pulse project list
pulse project untrust ~/code/notes-app
```

- Trust is bound to the file's **content hash**. When the file changes (a `git pull`, a branch
  switch, your own edit) the project becomes **changed** and is inactive until you trust it
  again. A deleted file shows as **missing**. `pulse project list` and the Projects view show the
  state.
- Changes you make through Pulse itself (the UI or `pulse rules add --project …`) re-bind the
  trust to the new content, since you made them.
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
agents with its prompts. Read it before trusting it, as you would a script. A repository used as
an agent's working directory can also bring its own agent settings (`.claude/settings.json`),
which Pulse does not control; see [Agents](agents.md#what-pulse-passes-to-the-agent).
