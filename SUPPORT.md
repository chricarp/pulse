# Getting help

Start with the [README](README.md) and the [user guide](docs/user/README.md). When something
does not fire, fires unexpectedly or will not start, read
[Troubleshooting](docs/user/troubleshooting.md) first: for every rule an event matched, Lictory
Pulse records what happened and why, and `pulse rules history <rule>` and
`pulse events show <id>` usually answer the question. For the desktop app, see
[its platform notes](docs/user/desktop.md#platform-notes).

Search [existing issues](https://github.com/chricarp/pulse/issues) first. Use the bug form for a
reproducible defect, the proposal form for intentional behavior changes, or the help form for
setup and usage questions. Include:

- `pulse --version`, your OS and version, and how you built it (`./scripts/build.sh`,
  `--no-app`, `cargo build`, or the desktop package);
- the output of `pulse status`, and of `pulse --json status` when the question is about a rule,
  source or agent that cannot work as written;
- the relevant lines of the daemon's log in `$PULSE_HOME/logs/` (`pulse.log`), not the whole
  file;
- the part of `pulse.toml` involved, and what you already tried.

Never include the token (`$PULSE_HOME/token`), an agent's MCP token, a login link, the event log
or the database (`pulse.db`), or a run's `event.json`. The event log and run output contain file
names, paths, commit messages, calendar titles and program output from your machine, so read
anything you copy from them, `pulse tail`, `pulse events` or the UI before you share it, and
paste only what the question needs. Paths and names in `pulse.toml` and in the log can be
personal too.

Community support is best effort, with no response-time guarantee. Security reports belong in
[the security channel](.github/SECURITY.md). Conduct concerns follow
[the code of conduct](CODE_OF_CONDUCT.md).
