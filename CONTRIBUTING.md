# Contributing to Lictory Pulse

Thanks for helping. Lictory Pulse runs all day on someone's machine and turns what happens there
into instructions for agents that can edit files and run programs. We review contributions with
that in mind: a clear problem, an agreed scope, visible evidence.

Opening a pull request does not oblige maintainers to review or merge it. We may ask for a
smaller scope, defer it, or solve the problem differently.

## Your first contribution

1. **Pick a problem.** Issues labelled `good first issue` are scoped by a maintainer: each names
   the files involved and what counts as done. Comment on the issue before you start so two
   people don't take the same one. Otherwise, open an issue first (see
   [Agree on the problem first](#agree-on-the-problem-first)).
2. **Ask early.** Ask in the issue itself when the question is about that issue; use the
   **Setup or usage help** issue form for anything else.
3. **Build it locally.** Follow [Install and build](README.md#install-and-build): Rust stable
   with a C compiler, Node.js 22 or later, and pnpm at the version in `package.json`
   (`corepack enable`). `./scripts/build.sh --no-app` builds the web UI and the `pulse` binary;
   `./scripts/build.sh` also packages the desktop app. [AGENTS.md](AGENTS.md#crates) shows where
   each part lives.
4. **Check as you go.** Run the tests nearest your change: `cargo test -p <crate>` for a crate,
   `pnpm --dir apps/web test` for UI logic, `pnpm --dir apps/desktop test` for the desktop
   shell's logic.
5. **Open the pull request.** Fill in the template, show that it works (see below), and run the
   full sequence in [Show that it works](#show-that-it-works) first. If something on your machine
   prevents part of it, say which step and what happened; a maintainer can help.

## Before you start

1. Read [`AGENTS.md`](AGENTS.md). Its principles, glossary, conventions and testing rules apply
   to code, UI copy and docs from every contributor, human or agent.
2. Read [the ways to hurt yourself](AGENTS.md#the-ways-to-hurt-yourself) in it and follow them
   exactly. They protect your own machine, your money and other people's work: they are not
   style advice.
3. Before changing the engine, read
   [docs/internals/0002-architecture.md](docs/internals/0002-architecture.md). Before touching
   prompt rendering, the command adapter, ingest or authentication, read the
   [threat model](docs/internals/0003-threat-model.md).
4. Search existing issues before filing or starting work.

## Agree on the problem first

- **Small, obvious bug fixes** can go straight to a pull request. Explain the defect and why the
  fix is obvious.
- **Larger bug fixes** need an issue that a maintainer has confirmed, so the failure and the
  intended behavior are established.
- **Features and intentional behavior changes** need a proposal issue with explicit maintainer
  agreement on direction and scope. Link that agreement in your pull request. Calling a behavior
  change a bug fix does not exempt it.

The test for new behavior is the list of things [we do not compromise on](AGENTS.md#what-we-do-not-compromise-on):
small, invisible when idle, the daemon as the product, honest abstraction, a reviewable history.
Changes that add machinery, configuration or screens without making "when does an agent run,
what is it told, and why" clearer or safer are unlikely to be accepted.

## One problem per pull request

Split independent fixes, even when each is useful. Include the contract, store, daemon, client,
test and documentation changes that one problem needs; a feature is done when it works from
every surface that applies (UI, CLI, config file, MCP) and the docs describe it. Adjacent
cleanups and refactors go in their own pull request.

Use a conventional commit title in plain language, e.g.
`fix(rules): a rule's cooldown survives a config reload`. The **Conventional PR title** check
enforces the format (`feat`, `fix`, `perf`, `docs`, `chore`, `ci`, `test`, `refactor`, `style`,
`build`, `revert`; the scope is optional). There is no release automation yet, so the type does
not choose a version.

## Changes that always need a maintainer

These areas carry the product's safety properties. Expect slower, deeper review, and call them
out in the pull request:

- authentication and tokens: the daemon token, agent MCP tokens, login codes, resume codes
- the proof handshake that clients require before they send a credential
- prompt rendering, the untrusted-data fence, and the command adapter's argument handling
- permission scopes and what an agent may hear about or read
- loop protection: causation, chain limits, cooldowns, rate limits, the circuit breaker
- process supervision and signals: process groups, timeouts, cancellation, the orphan guarantee
- config write-back (revisions and conflicts) and project trust
- store schema and migrations, which are append-only
- the MCP surface: tools, their arguments and what they return
- dependencies, CI and packaging, and agent or contributor instructions (`AGENTS.md`, `CLAUDE.md`,
  this file)

## Show that it works

Explain how you established the problem, how you checked the change, and what you observed.
"Tests pass" alone is not evidence.

- Add or update tests as [Testing](AGENTS.md#testing) in `AGENTS.md` describes: at the lowest
  layer that can fail, no sleeps, and a bug fix starts with a test that fails for that bug.
  Tests use the fake agents in `crates/pulse-agents/tests/fixtures`, never a real one.
- For UI changes, include before/after screenshots, and a short recording when timing or
  interaction matters. Upload them to the pull request; never commit them. Screenshots use a
  throwaway `PULSE_HOME` and fictional paths, never your own. The images in `docs/images/` are
  the exception: `pnpm --dir apps/web docs:images` makes them
  ([0008](docs/internals/0008-web-ui.md#documentation-images)).
- Before requesting review, run the full sequence and report the results:

  ```sh
  cargo fmt --all --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  pnpm install --frozen-lockfile
  pnpm --dir apps/web typecheck && pnpm --dir apps/web lint && pnpm --dir apps/web test
  pnpm --dir apps/web test:browser     # the UI in Chromium, against the mock daemon
  pnpm --dir apps/web build
  cargo clean -p pulse-daemon && cargo build -p pulse-cli
  pnpm --dir apps/web test:real        # the UI against the real daemon built just above
  pnpm --dir apps/desktop typecheck && pnpm --dir apps/desktop test
  ```

  The browser suites need Playwright's Chromium
  (`pnpm --dir apps/web exec playwright install chromium`). `test:real` runs the debug `pulse`
  binary, which embeds the web UI only when it is compiled after `apps/web/dist` exists; hence
  the `cargo clean -p pulse-daemon`. For changes to the desktop shell, also run
  `apps/desktop/e2e/run.sh <output dir>`, which drives the real app under Xvfb (Linux,
  `xvfb-run`) with a release binary; see
  [0010](docs/internals/0010-desktop-shell.md). Say what you could not check, such as macOS.

If an agent did the work, name the model and harness at the end of the description. We judge the
problem, scope and evidence, not who or what wrote it.

## Security

Never report vulnerabilities in public issues. See [the security policy](.github/SECURITY.md).

## Closing and reconsideration

A pull request that misses these requirements may be closed before deeper review. The closing
comment names the specific requirement and what would fix it. Address it and ask for
reconsideration, or open focused replacements linked to the original.

## Community and contribution rights

Follow the [code of conduct](CODE_OF_CONDUCT.md). See [governance](GOVERNANCE.md) for decisions
and maintainership, and [support](SUPPORT.md) for help.

By intentionally submitting a contribution, you agree to license it under the project's
[MIT License](LICENSE). You retain copyright in your contributions; no copyright assignment, CLA
or DCO sign-off is currently required. Submit only work you have the right to contribute,
including any required employer approval. Preserve third-party license notices and identify
copied code, assets or generated material that needs attribution. A human contributor remains
responsible for AI-assisted work, its correctness and the right to submit it.

The owner must complete the [owner checklist](docs/operations/open-source.md#owner-checklist) in
the publication runbook before soliciting or merging outside contributions.
