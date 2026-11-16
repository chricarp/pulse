<!--
Read CONTRIBUTING.md first. Solve one underlying problem per pull request.
Title: a conventional commit in plain language, e.g.
"fix(rules): a rule's cooldown survives a config reload".
-->

## Problem

<!-- What is wrong or missing, and for whom? Expected behavior and reproduction steps where relevant. -->

## Change

<!-- The chosen behavior and how it solves the problem. Explain why changes in several areas belong to the same fix. -->

## Scope and agreement

<!-- Link the confirmed issue or the maintainer agreement on a proposal. For a small, obvious fix, explain why it qualifies without one. -->

## Tradeoffs

<!-- Alternatives considered, limits of this change, follow-ups. Delete if none. -->

## Affected surfaces

<!-- Tick what this change touches; explain each in Change or Verification. A surface that does not apply to a feature: say so. -->

- [ ] Web UI
- [ ] CLI
- [ ] Config file (`pulse.toml`)
- [ ] MCP
- [ ] Desktop app
- [ ] Permissions and scopes
- [ ] Loop protection
- [ ] Prompt rendering or the command adapter
- [ ] Process handling (supervision, signals, timeouts)
- [ ] Authentication, tokens or the proof handshake
- [ ] Config write-back or project trust
- [ ] Store schema or migrations
- [ ] Dependencies, CI or packaging
- [ ] Checked on macOS
- [ ] Checked on Linux
- [ ] Documentation (`docs/user/`, `docs/internals/`, README)

## Verification

<!--
How you established the problem and checked the change, and what you observed.
Focused tests for the behavior, plus the full sequence from CONTRIBUTING.md
("Show that it works") with results. State what you could not check.
UI changes: before/after screenshots (and a recording for interaction or timing),
taken with a throwaway PULSE_HOME, uploaded here, never committed.
-->

<!-- If an agent did the work, name the model and harness here. -->
