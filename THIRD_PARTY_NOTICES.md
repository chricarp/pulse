# Third-party materials

Lictory Pulse's original code is offered under the root [MIT license](LICENSE). Third-party
materials retain their own licenses; that license does not relicense them.

## Origin of the original material

These are facts recorded in the repository, not a rights declaration:

- The code, tests and documentation in this repository were written with AI coding agents at
  Christian Carpinelli's direction during November 2026, as the commit history and its
  `Co-Authored-By` trailers show.
- `assets/icon.svg` (the app icon: a rounded square with the letter "P", rendered in a system
  font) came with the repository's initial commit, which was created by a project template. Its
  provenance has not been confirmed.

The owner has not yet made a declaration about who created the original code and artwork, or
about employer, client or purchased-material exceptions. That declaration is an item on the
[owner checklist](docs/operations/open-source.md#owner-checklist); until it is recorded here,
this file makes no claim about it.

## Materials distributed as source

No third-party source code is vendored in this repository. Everything under `crates/` and
`apps/` is original to the project; dependencies are fetched by cargo and pnpm and are not
committed.

These test fixtures contain material produced by other programs:

| Material | Location | What it is |
| --- | --- | --- |
| Recorded agent output | `crates/pulse-agents/tests/fixtures/claude-code/`, `crates/pulse-agents/tests/fixtures/codex/` | The JSON event streams and error output of Anthropic's Claude Code (2.1.325) and OpenAI's Codex CLI (0.169), recorded by this project on 2026-11-16 in an empty temporary directory with each CLI's cheapest model, then scrubbed (paths, ids, signatures and account details replaced or removed). They include short model-generated answers to the project's test prompts. `codex/tools.synthetic.jsonl` is written by hand from Codex's documented event format. See [the fixtures README](crates/pulse-agents/tests/fixtures/README.md). |
| Calendar files | `crates/pulse-sources/testdata/*.ics` | Written for the tests in the format of Google Calendar, Apple Calendar and Outlook exports (their `PRODID` lines name those products). The events, names and identifiers are invented. |
| Fake agents | `crates/pulse-agents/tests/fixtures/fake-*` | Original shell scripts. |

## Fonts, icons and other assets

The web UI and the desktop app ship no third-party fonts, icons or images:

- The UI uses the operating system's fonts (`ui-sans-serif`, `system-ui`, `ui-monospace` and
  similar stacks in `apps/web/src/styles.css`); no font file is bundled.
- The UI's line icons (`apps/web/src/ui/icons.tsx`) were drawn for this app.
- The app icon is `assets/icon.svg` (see above). The desktop build renders it to PNG
  (`apps/desktop/scripts/icons.mjs`) with a font installed on the build machine (Liberation Sans
  on Linux, Helvetica on macOS), so the shipped PNGs contain that font's rendering of one letter;
  no font file is shipped.

So there is no `LICENSES/` directory: nothing bundled as an asset requires its own license text
to travel with it.

## What the built products contain

- **The `pulse` binary** statically links about 170 Rust crates (the exact set depends on the
  target) and SQLite (compiled in through `rusqlite`'s bundled build; SQLite is in the public
  domain). It also embeds the built web UI, which bundles React, React DOM, scheduler and
  TanStack Virtual (all MIT). Most of these licenses (MIT, Apache-2.0, BSD, ISC, Unicode-3.0,
  CDLA-Permissive-2.0 for the CA certificates in `webpki-roots`) require their notices to
  accompany a binary distribution. The binary and the UI do not display or contain those
  notices today: the generated report below must be shipped with any binary that is
  distributed.
- **The desktop app** bundles Electron, which includes Chromium, Node.js and their own
  dependencies. Electron's distribution carries its license (`LICENSE`) and Chromium's
  third-party notices (`LICENSES.chromium.html`); check that the packaged AppImage or dmg still
  contains them. The app's own code is two esbuild bundles with no npm dependencies besides
  Electron, plus the same `pulse` binary as above.

## Dependency and artifact review

After `pnpm install --frozen-lockfile`, from the repository root, run:

```sh
pnpm run licenses:report /tmp/pulse-licenses
```

This writes `inventory.json` (every Rust crate resolved for the target and every installed npm
package, with version, declared license, which product it reaches, notice-file hashes, the
commit and the lockfile hashes) and `THIRD_PARTY_NOTICES.txt` (the notice texts, grouped into
what is linked into the binary, what the web UI and desktop app carry, and build tools). It
performs no registry lookups. Pass `--target <triple>` for another Rust target. Reports stay
outside Git; keep the report for the exact commit with any artifact you publish.

The command fails when a dependency declares no license, or a license outside the policy in
[`scripts/license-report.mjs`](scripts/license-report.mjs): permissive licenses only, plus a
short list of packages reviewed one by one that are used only to build:

- `lightningcss` (Vite's CSS minifier) and `@resvg/resvg-js` (renders the icon at build time):
  MPL-2.0.
- `truncate-utf8-bytes` (via electron-builder): WTFPL.
- `argparse` (via js-yaml): Python-2.0.

None of these reaches the binary, the UI or the desktop app; a listed package that does fails
the report again.

The report does not cover the Electron and Chromium binaries' own notices (inside the Electron
distribution, see above), npm platform variants not installed on the machine that ran it, Rust
targets other than the one given, or the operating system. A package that has license metadata
but no license file is listed under `missingNotices`; obtain its upstream notice before
distributing it (today one shipped crate, `ts-rs`, has none in its published package). This is
not an SBOM and not a compatibility verdict.
