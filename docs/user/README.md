# Lictory Pulse user guide

Task-oriented guides for people running Pulse. For how and why it is built the way it is, see
[docs/internals/](../internals/).

| Guide | For when you want to… |
|---|---|
| [Getting started](getting-started.md) | build Pulse, run a first rule with no model, add the shipped templates |
| [Rules](rules.md) | match events, debounce and limit, require approval, test a rule, read its history, replay, turn it off |
| [Loop protection](loop-protection.md) | understand why a rule did not react to its own run's output, and undo a wrong block |
| [Sources](sources.md) | watch folders, repositories, schedules and calendars; wrap commands; send external events |
| [Agents and permissions](agents.md) | define agents, know exactly what Pulse passes to Claude Code, Codex or a command, and set scopes |
| [Connecting an MCP client](mcp.md) | let a running agent query and subscribe to events |
| [Project files and trust](projects.md) | share rules in a repository without letting a clone run commands |
| [The desktop app](desktop.md) | use the tray app, and know its limits |
| [CLI reference](cli.md) | every command and option |
| [Config file reference](config.md) | every key in `pulse.toml` |
| [Troubleshooting](troubleshooting.md) | find out why something fired, or didn't |

The security model is summarised in the [README](../../README.md#security-model) and set out in
[docs/internals/0003-threat-model.md](../internals/0003-threat-model.md).
