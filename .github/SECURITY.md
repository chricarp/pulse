# Security policy

Lictory Pulse is a local daemon that turns what happens on a machine into prompts and commands
for coding agents, which can edit files and run programs as the user. The daemon's token
(`$PULSE_HOME/token`) grants full control of that daemon: whoever holds it can define rules and
agents, approve runs and so run commands as the user. The event log (`pulse.db`) holds file
paths, commit messages, calendar titles and program output from the machine. We take reports
seriously.

What the project defends against, what it does not, and the known limits of each control are in
the [threat model](../docs/internals/0003-threat-model.md). Read it before reporting: a behavior
it already lists as an honest limit is not a new vulnerability, though a way to make that limit
worse than it says is.

## Supported versions

There is no release yet. Until the first release, security fixes land on the default branch only;
build from its latest commit to receive them.

| Version                             | Supported |
| ----------------------------------- | --------- |
| Latest commit on the default branch | Yes       |
| Older commits                       | No        |
| Builds or packages made from them   | No        |

From the first release on, security fixes are planned to go into the latest release only,
shipped as a new release; older releases will not be patched. This table will be updated when
that release is published.

## Reporting a vulnerability

Report privately through GitHub's
[private vulnerability reporting](https://github.com/chricarp/pulse/security/advisories/new) for
this repository. Do not open a public issue, pull request or discussion.

Include what is affected, the commit, your OS, how to reproduce it, and the impact you observed.
The most valuable reports are anything that lets something other than the user cause a run, read
the token or the event log, or let an agent escape its scopes:

- another local account, or a web page open in the user's browser, reaching the daemon, causing
  a run, or obtaining the token, an agent token, a login code or a resume code;
- event content (a file name, a commit message, a calendar entry, program output, an external
  payload) adding arguments or commands, reaching a shell, escaping the untrusted-data fence, or
  changing what a person sees in the UI, the CLI, a notification or an approval;
- a repository's files causing a run, or a write outside the trusted file, without
  `pulse project trust` of that exact content;
- an agent hearing about or reading events outside its `allow` scopes, or using its MCP token to
  approve runs, change the configuration or reach anything beyond its scopes;
- the daemon signalling a process it did not start, or loop protection being defeated by
  content rather than configuration.

Never include real tokens, your own event log or other people's data in a report.

Test only against your own machine and your own Lictory Pulse home: use a throwaway
`PULSE_HOME`, and the fake agents in `crates/pulse-agents/tests/fixtures` instead of a real
agent. Do not test against machines run by others without their permission, and do not access,
modify or retain data that is not yours.

We will acknowledge the report, keep you informed while we investigate and fix it, and credit you
when the fix ships unless you prefer otherwise. Please give us a reasonable opportunity to
remediate before any public disclosure.
