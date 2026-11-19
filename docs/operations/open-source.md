# Repository and publication

Lictory Pulse is meant to be distributed as open source under the root MIT license, the way its
sibling [Lictory](https://github.com/chricarp/lictory) is. Contribution terms are in
[CONTRIBUTING.md](../../CONTRIBUTING.md); the origin of the original material and the
bundled-material inventory are in [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md). Those
are the sources of truth rather than a second licensing discussion here. Crates are
`publish = false` and npm packages `private`, to prevent accidental publication to a registry.

This runbook covers what must be checked before the repository is made public, before any
artifact is published, and the repository settings that keep it safe afterwards. It does not
certify that anything is safe to publish. Keep completion evidence (the commit, the license
report, the date, who checked) with the decision it supports.

## Where things stand

- The repository [chricarp/pulse](https://github.com/chricarp/pulse) is **private**. Nothing has
  been published: no release, no package, no container image, no website or documentation site.
- **The CI workflows have never executed a job.** `ci.yml`, `desktop.yml` and the hygiene
  workflows below were written and linted locally (YAML validity). Every run since the first
  push was refused before it started: Actions on a private repository is billed to the account,
  and GitHub answered "recent account payments have failed or your spending limit needs to be
  increased". Expect the first runs to need fixes.
- **macOS has never been run.** The FSEvents watcher, the pseudo-terminal of `pulse run`, timer
  behaviour and the desktop app on macOS are untested; so are Linux x64 and the `.dmg`. Only
  Linux arm64 has been developed and run on (README, "Status and honest limits").
- **The desktop package is unsigned**: an ad-hoc signature on macOS (Gatekeeper blocks the first
  open), no signature on Linux, no notarization and no auto-update
  ([0010](../internals/0010-desktop-shell.md#packaging)). On Ubuntu 23.10 and later the AppImage
  runs without Chromium's OS sandbox.
- **Third-party notices do not travel with the products yet.** The `pulse` binary statically
  links crates and embeds a web UI bundle whose licenses require their notices to accompany a
  binary; nothing in the binary, the UI or the desktop package carries them
  ([THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md#what-the-built-products-contain)).
- The images in `docs/images/` are made by `pnpm --dir apps/web docs:images`
  ([0008](../internals/0008-web-ui.md#documentation-images)): the real daemon and UI, a
  fictional scenario in a throwaway home under `/tmp`, and the stand-in agents. Regenerate them
  after a UI change they show.

## Owner checklist

> **Only the owner can do or declare these.** Contributors and agents working on this
> repository must not do them, and must not write any of them down as done on the owner's
> behalf. Record each one here (date and outcome) when it is done.

- [x] **Rights declaration.** Done on 19 November 2026: the owner confirmed it when asked, and
      it is recorded in
      [THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md#origin-of-the-original-material). It
      covers:
  - the code, tests and documentation written with AI coding agents at the owner's direction
    during November 2026 (the commit trailers name the agents);
  - the artwork. The owner chose to replace `assets/icon.svg`, a template placeholder of
    unconfirmed provenance, rather than confirm it: the icon there now was drawn for the project
    on the same day. The placeholder remains in the Git history;
  - the recorded agent output in `crates/pulse-agents/tests/fixtures/` (output of Claude Code
    and Codex CLI and of the models they ran), which the owner confirmed may be published as
    test data.
- [ ] **Make the repository public**, after every item in
      [Before the repository is made public](#before-the-repository-is-made-public) is done.
- [ ] **Enable private vulnerability reporting** (Settings → Security), so the link in
      [SECURITY.md](../../.github/SECURITY.md) works, and check who receives reports.
- [ ] **Branch rulesets** for `main` (see [GitHub configuration](#github-configuration)),
      including which checks are required and whether CODEOWNERS approval is required.
- [x] **Discussions**: decided on 19 November 2026: not enabled, issues only. If that changes,
      link them from `.github/ISSUE_TEMPLATE/config.yml`, [SUPPORT.md](../../SUPPORT.md) and
      [CONTRIBUTING.md](../../CONTRIBUTING.md); today those files point only at issues.
- [ ] **The first release**: whether there is one, when, what it contains (binary, AppImage,
      dmg; which platforms), how it is versioned, signed and built, and which release line gets
      security fixes. Then update the supported-versions table in
      [SECURITY.md](../../.github/SECURITY.md). No release workflow exists; adding one is part of
      this decision.

## Before the repository is made public

Everything pushed to a public repository is published, including the whole history. Check:

- [x] The owner checklist's rights declaration is recorded.
- [x] No credentials, personal data or machine-specific names in the tree **or the history**:
      run the **Secret scan** workflow manually (it scans the whole history on
      `workflow_dispatch`) and review every finding; regenerate the images in `docs/images/`
      (`pnpm --dir apps/web docs:images`, after the build
      [0008](../internals/0008-web-ui.md#documentation-images) gives) and review them and the
      recorded fixtures by eye. Revoke a credential that ever reached a commit before
      cleaning it up; rewriting history does not un-publish it.
- [x] Every Markdown link resolves, and the README links only to things that exist.
- [x] A clean checkout builds and passes the full sequence in
      [CONTRIBUTING.md](../../CONTRIBUTING.md#show-that-it-works) without private files, accounts
      or paid services. Record the result and the machine it ran on.
- [ ] The CI workflows have run green at least once on a pull request (they can run while the
      repository is private; the scanning workflows that need a public repository skip
      themselves until then).
- [x] The license report passes for the commit being published (see below).

### The record for 19 November 2026

Checked by an AI coding agent (Claude Code) at the owner's direction, on the machine the project
was developed on: Linux arm64, Ubuntu 24.04, rustc 1.100.0, Node 24.22. Commit `3ffab81` is the
branch that became this change, before this record was added to it; the record changes only
this file and two workflow triggers.

- **Secrets.** The Secret scan workflow could not run (see the CI item below), so the same
  gitleaks release it pins (8.30.1) was run locally over every commit reachable from the
  branches on GitHub, with and without `.gitleaksignore`. It reported four findings, exactly
  the four entries of `.gitleaksignore`, and nothing else. Run the workflow itself once the
  repository is public and compare.
- **Personal data and machine names.** The tree and every line the history adds were searched
  for the development machine's user name, host name and home path and for private email
  addresses: none. Commit authors and committers are GitHub `noreply` addresses. The only
  contact address in the tree is the one the code of conduct publishes on purpose.
- **Images and fixtures.** `docs/images/` was regenerated at `3ffab81` and looked at; it shows
  the fictional scenario under `/tmp/pulse-demo-*` and nothing of the machine. The recorded
  agent output carries placeholder ids, `/tmp/pulse-run` paths and no account details.
- **Links.** 202 links and anchors in the 35 tracked Markdown files resolve; the external ones
  answer, except those into this repository, which cannot until it is public.
- **Clean checkout.** A fresh clone of `3ffab81` passed every step of the sequence: 332 Rust
  tests, the web unit tests, the 21 browser tests against the mock, the 15 against the real
  daemon, and the desktop shell's typecheck and tests. Not run: `apps/desktop/e2e/run.sh`, and
  anything on macOS or Linux x64.
- **A test that fails now and then.** The same sequence on the commit before (`043b0e3`) failed
  once, in `mcp::event_ids_increase_in_commit_order_across_paths_and_restarts`: the test
  restarts its daemon and found the home still locked. Run alone 200 times, the `pulse-daemon`
  integration tests failed that way 3 times. The likely cause is in the test harness, not the
  daemon: a child forked by a sibling test holds a copy of the lock's descriptor until it
  execs. A fix for the harness follows this change as its own pull request.
- **License report.** `pnpm run licenses:report` passed at `3ffab81` for
  `aarch64-unknown-linux-gnu` (no problems; 21 packages without a notice file in the package,
  as before).
- **CI: not done, by the owner's decision.** GitHub refused to start any job while the
  repository was private (see [Where things stand](#where-things-stand)). Rather than wait, the
  owner chose on 19 November 2026 to publish on the strength of the local record above and get
  the workflows green afterwards, before any check is required on `main`. Until that has
  happened, nothing here has been built or tested on macOS or Linux x64.

## Before any artifact is published

A binary, an AppImage, a dmg, or anything else built from this repository and offered for
download is a distribution of every dependency in it. For each artifact:

- [ ] Run `pnpm run licenses:report <dir outside the repository>` for the exact commit and
      target, on a machine of the artifact's platform. It must pass. Keep the report with the
      artifact's record.
- [ ] Ship the report's `THIRD_PARTY_NOTICES.txt` with the artifact (next to the binary in a
      release, and inside the desktop package). There is no mechanism for this yet: building one
      is part of the first-release decision.
- [ ] Check that the desktop package still contains Electron's `LICENSE` and
      `LICENSES.chromium.html`. The license report does not cover them.
- [ ] Obtain upstream notices for shipped packages the report lists under `missingNotices`.
- [ ] State the platform status honestly in the release notes: what has been run, and on what.
- [ ] Decide signing and notarization; until then, say the package is unsigned and what that
      means on each platform (as [the desktop guide](../user/desktop.md#platform-notes) does).

The report inventories the Rust crates resolved for one target and the npm packages installed on
the machine that ran it. It does not cover the Electron and Chromium binaries' own notices, npm
platform variants not installed there, other Rust targets, or the operating system; see
[THIRD_PARTY_NOTICES.md](../../THIRD_PARTY_NOTICES.md#dependency-and-artifact-review) for what it
checks and the reviewed exceptions.

## GitHub configuration

Repository settings are live state. Verify them in GitHub; do not treat this file as evidence
that protection is enabled.

- Protect `main` against deletion and force pushes. Require pull requests, resolved
  conversations, the **CI** and **Desktop** checks once they have run green, and
  **Conventional PR title**. Add CODEOWNERS approval when an independent reviewer is available;
  record any necessary sole-maintainer bypass narrowly and audit its use.
- Prefer squash merges with the conventional PR title. Delete merged topic branches.
- Default the Actions token to read-only. Require maintainer approval for workflows from fork
  pull requests for all external contributors. The workflows here use no secrets; keep it that
  way for anything a pull request triggers. Keep third-party actions pinned to full commit SHAs
  with the version in a comment; Dependabot proposes updates.
- Keep the title workflow's `pull_request_target` metadata-only: it must never check out or run
  pull-request code, or put the title or body into a shell.
- Turn on secret scanning, push protection and private vulnerability reporting, and keep
  dependency and security alerts enabled. Verify who receives reports; the
  [code of conduct](../../CODE_OF_CONDUCT.md) documents the sole-maintainer reporting limitation.
- Apply labels by running **Repository labels** by hand: it creates or updates the entries in
  `.github/labels.json` without deleting other labels.

## Security scanning

| Workflow                | Scope                                                                 |
| ----------------------- | --------------------------------------------------------------------- |
| `codeql.yml`            | Rust, TypeScript and Actions, security-extended, build mode none      |
| `dependency-review.yml` | Cargo, npm and workflow changes in pull requests; high/critical       |
| `scorecard.yml`         | OpenSSF Scorecard                                                     |
| `workflow-lint.yml`     | actionlint and zizmor for workflow changes                            |
| `gitleaks.yml`          | Commits a pull request adds; the whole history on manual dispatch     |

The first three need a public repository (or GitHub Code Security) and skip themselves while the
repository is private. The other two run either way. Reviewed suppressions belong inline with a
reason (as in `pr-title.yml`); reviewed secret-scanner false positives use `.gitleaksignore`
fingerprints. The entries there today (a fixed test token and RFC 6455's sample WebSocket key)
were found by replaying gitleaks' generic rule locally, not by gitleaks itself: compare them with
the first real scan (a local run of the same gitleaks release on 19 November 2026 reported
exactly these), and remember that a fingerprint names a commit, so rewriting history needs new
entries. `main` was fast-forwarded to the development branch `v1`, not squashed, so they hold.

Dependabot proposes weekly updates for Cargo, npm (through `pnpm-lock.yaml`) and GitHub Actions,
after a week of cooldown. Electron updates are not grouped with other development dependencies:
review them against [0010](../internals/0010-desktop-shell.md).

## Triage and review

Start reports with `needs-triage`; use `needs-information` for missing context and `accepted`
only after explicit scope agreement. `good first issue` needs a bounded problem and verification
guidance. Avoid automatic stale closure of unresolved defects. See
[contributing](../../CONTRIBUTING.md) for scope and review requirements,
[support](../../SUPPORT.md) for help and [governance](../../GOVERNANCE.md) for decisions and
maintainership.
