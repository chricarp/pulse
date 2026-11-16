# 0005: Sources

Status: v1, Phase 2. How the built-in sources in `pulse-sources` turn machine noise into a small number of events, and the decisions someone could reasonably have made differently. The module docs in the crate carry the detail; this page is the map.

## Shared pieces

- **Clock.** Sources take an injectable `Clock` (`now`, `sleep_until`). Tests use `TokioClock` on paused tokio time. The real clock sleeps on a `CLOCK_REALTIME` timerfd with an absolute deadline on Linux, so a timer that spans a suspend fires on resume instead of being late by the length of the suspend; elsewhere it sleeps in steps of at most a minute and re-reads the clock. This is the only "wake-up without news", and only off Linux.
- **Backpressure.** `EventSink::reserve` (new, additive) lets a source wait for room inside `select!` without losing the event it holds, so it keeps reading and coalescing while the engine is behind. Every buffer between the platform and the sink is bounded.
- **A source that ends.** The daemon runs each source in its own task and looks at how it ended. Anything but stopping when asked (a panic, an error, returning on its own) is `failed` with the reason (a panic's message, cut to 512 bytes), never the last health it reported. It is not restarted automatically: a source already retries what it knows to be transient and says so as `degraded`, so ending means a bug (a panic that the same input, often a cached one, would trigger again) or a condition the source judged permanent, and a retry loop would hide either. It starts again the next time the configuration is loaded (any edit, or a daemon restart). A restart with backoff was the alternative; it would turn an input that always panics into a periodic crash.
- **Untrusted numbers.** Arithmetic on anything read from input or config is checked or saturating; a value out of range is a config error (config) or a warning in health (content), never a panic.
- **State files.** Schedules and calendars keep a small JSON file in `$PULSE_HOME/sources/<kind>-<name>.json` (and the calendar's last download next to it), written atomically. Missing or unreadable state means "first start". This is what makes "don't repeat a fire after a restart" and "catch up once after downtime" possible.

## Filesystem (`fs`)

Three layers: `platform` (notify events to observations; the only place inotify and FSEvents differ), `coalesce` (a pure state machine, timestamps in, settled changes and the next deadline out), and the I/O around them (watches, scans, root presence, health, the sink).

**Net effect per path.** A pending entry records whether the path existed when its window opened and whether it exists now. When the path has been quiet for `quiet` (default 2s) it settles into created, modified, deleted, moved, or nothing. A path that never goes quiet (a log being appended) is reported every 5 minutes anyway.

**What existed before** comes from a bounded index of hashes (not paths) of known files, filled by the initial scan and kept current by signals; 200k entries, after which absence means "unknown". This one piece of state is what makes:
- an atomic save (write a temp file, rename it over the target) read as a modification of the target rather than a create and a delete, and a temp file renamed onto a *new* name read as a creation;
- FSEvents' merged, coarse reports usable at all: every report becomes "something happened here; a stat says it exists or not", and the index decides created versus modified versus deleted;
- a root that disappears and comes back (a rename away and back, a remounted disk) not flood created events for files that were there before.

The alternative, keeping a full listing, costs memory proportional to the tree and was rejected.

**Renames** pair by inotify's cookie, or for FSEvents (no cookie) by adjacency within `pair_window`, with "exists now" deciding which half is which. A half that never pairs is a move out (deletion) or in (appearance). A rename from an ignored name (an editor temp file, a `.crdownload`) or from a file created inside the same window means "new content at the target". A rename to an ignored name (vim's `file~` backup) is a deletion of the visible file. Directory renames are scanned: each file inside becomes `fs.file.moved`.

**Downloads.** In-progress download names (`.part`, `.crdownload`, `.download`, `.partial`, `.opdownload`) are ignored, and while one exists the file it will become is held back (up to 5 minutes), so Firefox's empty placeholder is not reported as created before the download finishes. Chrome's rename into place is one `fs.file.created`. A download written directly to its final name with pauses longer than `quiet` can still be reported early; nothing at this layer can tell that from a finished file.

**Storm unit: the directory where the activity spreads.** When pending entries reach `burst_threshold` (default 200), planning walks down from the root: at a directory with at least a threshold of changes under it, if one child holds at least as much as everything else and everything else is below the threshold, that child dominates and the walk descends into it, leaving the rest as individual changes; otherwise the activity is spread here and this directory becomes the storm root. An archive unpacked into `dataset/` folds at `dataset` (it spreads over `dataset/part00..99`), a clone folds at the clone's directory, and an edit to `notes.md` beside them is still its own event. Folding at the scope root was rejected because it hides unrelated edits; folding at the deepest dense directory was rejected because it fragments one unzip into dozens of bursts. Storms that later appear next to each other under one parent merge into it.

A storm stops tracking paths: it keeps exact net effects per path *hash* (so a file created then written forty times counts once) up to a global budget of 100k, then counts signals by kind (an over-count of modifications), plus a sample of 8 paths. It settles as one `fs.burst` when its subtree has been quiet for `2 × quiet`, or every 5 minutes while it lasts. A storm with no net effect (a build creating and deleting temp files) emits nothing. Hard bounds: 1024 pending entries (past it the whole scope folds), 16 storms, 64 held rename halves, 256 held downloads.

Measured: 10,000 files in 100 directories plus one edit outside them gives exactly two events (one burst with `created: 10000`, one `fs.file.modified`), with at most ~250 per-path entries alive at once (test `ten_thousand_files_are_one_burst…` and the manual check with an unzip).

**Ignores.** Built-in: VCS internals (`.git`, `.hg`, `.svn`, `.jj`, …), dependency and cache directories (`node_modules`, `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.tox`, `.direnv`, `.idea`, …), editor swap/backup/lock files (`*.swp`, `*~`, `.#*`, `#*#`, `4913`, JetBrains `___jb_tmp___`), generic temp (`*.tmp`), OS metadata (`.DS_Store`, `._*`, `Thumbs.db`, `.Trash*`, …), and download temp names. User `ignore` globs follow gitignore's intuition: no `/` matches a name at any depth, a leading `/` or `~` is absolute, anything else is relative to the root. An ignored directory hides its subtree; on Linux it is not watched at all.

**Names that are not UTF-8** cannot be events (paths are text everywhere: events, scopes, globs). Such a file is skipped and the source's health records it (`last_error`, shown by `pulse sources`), rather than disappearing silently.

**Linux (inotify).** One non-recursive watch per directory, managed by the source. A new directory is watched *before* it is listed, so a file created right after `mkdir` is seen by the watch, the listing, or both (duplicates merge). The watch limit is reported as degraded health with the current `max_user_watches` and the `sysctl` to raise it; the rest of the tree keeps working. Queue overflow (the kernel's, or ours when the watcher thread outruns the source) triggers degraded health and a rescan: unknown files are reported as new, known files written since shortly before the overflow as modified. Deletions during the lost window are not seen, and health says so. A watched directory moved away has its watches dropped immediately, so stale paths are never reported.

**macOS (FSEvents).** One recursive watch on the root; ignored directories are filtered, not excluded (`notify` does not expose FSEvents exclusion paths). Paths come back canonical (`/private/var/…`) and are mapped to the configured root. Reports for a path right after its rename report in the same batch are dropped, since they are merged flags of the rename.

**Root.** A missing root is degraded health ("does not exist; waiting for it to appear") with a watch on the nearest existing ancestor; a root that is removed or moved away goes back to waiting; an unreadable root fails the source. A root that is a file is waited on as missing.

**Symlinks** are never followed inside the tree: a symlink is a file (its creation, replacement or removal is reported; its size is the link's), and a symlinked directory is not descended, so a link cannot pull another tree (or a loop) into scope. A root that is itself a symlink is watched through it, and events use the root as configured, so paths match the scopes the user wrote (scopes compare text; see 0003).

## Git (`git`)

**Detection reads files, never runs `git`.** `logs/HEAD` for commits and branch switches (each new line classified by its message: `commit…`, `cherry-pick:`, `revert:`, `am:`, merges that say `Merge made by`, `checkout: moving from A to B`); `logs/refs/remotes/<remote>/<branch>` lines `update by push` for pushes (fetches write `fetch: …`); `MERGE_HEAD` / `CHERRY_PICK_HEAD` / `REVERT_HEAD` / `rebase-merge/` / `rebase-apply/` plus the index's conflict stages (index v2–v4 parsed directly) for conflicts. The reflog approach was verified against git 2.43 in the tests. Only the git directories are watched, never the working tree, so a checkout that rewrites thousands of files is one reflog line and one `git.branch.switched`.

Decisions:
- **Amend** is a new commit: `git.commit.created`.
- **Rebase** reports nothing for the commits it rewrites and no branch switch (it starts and ends on the same branch); it reports `git.merge.conflicted` if it stops on conflicts.
- **Fast-forward** merges and pulls create no commit and report nothing; a merge or pull that creates a merge commit is a commit.
- **Detached HEAD**: a switch to a non-branch reports `to` as the commit id (not the expression typed); commits there have `branch: null`.
- **Conflicts** are reported once per episode; resolving all of them ends the episode, so the next conflicting step of a rebase is a new event.
- **Message and author** come from the commit's loose object (new commits are loose), capped at 4 KiB / 256 bytes; once objects are packed, the reflog's subject line and identity are used instead.
- **Branch** of a commit is tracked through the HEAD reflog (checkouts, rebase start/finish) and re-synchronised from `HEAD` after each batch; whether a name is a branch is checked in loose refs and `packed-refs`.
- **Worktrees and submodules** (`.git` is a file): `gitdir:` and `commondir` are followed. A linked worktree's source sees its own HEAD log; remote-tracking reflogs live in the common directory, so a push from any worktree of the repository is seen by every source on it. Commits in another worktree belong to that worktree's source.
- **History is not replayed** at startup (reflogs are read from their end). A repository that appears while the source runs (`git init`, `git clone`) is read from the start, skipping lines older than its appearance; until then the source waits with degraded health.

Pushes to a URL without a remote-tracking ref (`git push https://… branch`) leave no reflog and are not seen.

## Schedule (`schedule`)

Five-field cron (names, ranges, steps, lists; both day fields restricted means either matches) in the local zone or a configured IANA zone, or a fixed `every`. One timer for the next fire.

**DST.** A local time skipped by a spring-forward fires at the first instant after the gap; a repeated local time fires once, at its first occurrence. So `30 2 * * *` fires at 03:00 on the spring-forward day and once on the fall-back day, and an hourly schedule neither fires twice at 03:00 nor twice in the repeated hour.

**Missed fires.** At most one catch-up per schedule, for the latest missed time, with `late: true` (a new, additive field on `ScheduleFiredData`); never a backlog. This covers both the daemon being down (from the state file) and the machine sleeping (the timer wakes late). A schedule seen for the first time, or whose definition changed, does not catch up. `every` schedules keep their phase across restarts.

## Calendar (`calendar`)

An own ICS reader (folding, parameters with quoting, escaping, nesting; lenient about LF line ends and missing `VCALENDAR`) and the `rrule` crate for recurrence. Times: `Z` is UTC; `TZID` is an IANA name, an IANA name behind a vendor prefix (`/mozilla.org/…/Europe/Rome`), a common Windows name (Outlook), or failing those the zone's `VTIMEZONE` standard offset (whole hours); floating times use `X-WR-TIMEZONE` or the user's zone; all-day events are midnight to midnight in the user's zone. `RRULE` (with `UNTIL` normalised to UTC, since exports write it as a date, a local time or UTC), `RDATE`, `EXDATE`, `RECURRENCE-ID` overrides and `STATUS:CANCELLED` (on a master or an instance) are handled.

**Identity and firing.** A one-off event is its `UID`; a recurring instance is `UID` plus its original start. `starting` (at `start − lead`) and `ended` fire at most once per identity and kind, so an event moved between polls does not fire again, and one removed or cancelled before its time never fires. A fire up to 15 minutes overdue (after a suspend or restart) still fires, and `minutes_until` shows how late; older ones are dropped. What fired is persisted.

**Fetching.** Files are re-read when their mtime or size changes; URLs use `ureq` with rustls (ring) and the platform verifier, `If-None-Match` / `If-Modified-Since` (the last body is cached so a 304 after a restart still has content), a 30 s timeout, a 10 MiB cap and 5 redirects. Failures back off from 1 minute, doubling up to `refresh`, with degraded health saying what failed and when it retries. ureq is blocking and runs on the blocking pool; the source stops without waiting for a fetch in flight, which then ends within its timeout.

**Bounds.** 20,000 events, 2,000 instances per event per window, 1 KiB per title or location; the expansion window is about a day around now plus the refresh and lead.

**Parsing is linear in the input.** Each content line costs a bounded number of steps: components nest at most 8 deep (deeper ones are skipped whole, and health says how many), so closing one compares at most 8 open components; unfolded lines are cut at 64 KiB and only one is held at a time; at most 1,000 `VTIMEZONE`s are kept. A file must be a regular file (not a pipe or device) and is read through the 10 MiB limit rather than trusting its size, which can change between the check and the read.

## Process wrapper (`process::run_wrapped`)

When stdout and stderr are terminals the command runs on a pseudo-terminal (`pty-process`): it sees a TTY (colours, line buffering), its two streams arrive in their real order, and the window size follows ours. If stdin is a terminal too, it is put in raw mode and forwarded, so Ctrl-C, Ctrl-Z and `/dev/tty` prompts (sudo, ssh) reach the command through its own terminal. Otherwise the command gets pipes, passed through chunk by chunk. SIGINT, SIGTERM, SIGHUP and SIGQUIT sent to the wrapper are forwarded; when wrapper and command share the terminal's foreground group (pipes with an interactive stdin), SIGINT and SIGQUIT already reached both and are not doubled. After the command exits, output is drained for at most 200 ms, so a background process holding the pipes cannot keep the wrapper alive.

The tail keeps the last `tail_bytes` of combined output, then removes escape sequences, resolves carriage-return overwrites (progress bars), and drops a partial first line. `exit_code` is `None` for a signal; the CLI should exit with `128 + signal`, as a shell would. A program that cannot be started is an error, not an event (the CLI should exit 127).

Honest limits: in raw mode, Ctrl-Z stops the command but not the wrapper, so the shell does not regain control the way it would with a direct run. The thread reading stdin cannot be cancelled and ends with the process.

## Ingest (`ingest::parse`)

One JSON object, an array, or JSON lines (by content type, or sniffed: several values in a JSON body are JSON lines). A minimal shape (`type`, `subject`, `time`, `data`, `dedupe_key`, `causation_run`; unknown fields are errors, because they are usually typos) or a CloudEvent (recognised by `specversion`; `id` is the default dedupe key, `source` is ignored, `data_base64` and Pulse-set `pulse*` attributes are refused). A type in a built-in namespace (`fs.…`, `git.…`, `process.…`, `schedule.…`, `calendar.…`, `pulse.…`) is refused with an explanation rather than silently renamed to `ext.<source>.fs.…`. Limits: body, event count, `type` 128 B, `subject` 1 KiB, `dedupe_key` 256 B, serialized `data` 64 KiB. A batch is accepted or refused whole; a refusal lists every bad item (up to 100) with index, line and field. `events.ingest` (what `pulse ingest` uses) applies the same type check (`ingest::check_type`); it used to rename a built-in type to `ext.<source>.fs.…` and accept it.

## Not done, or weaker than wanted

- macOS paths (FSEvents backend selection and canonical-path mapping, the pty on Darwin, the stepped wall-clock sleep) type-check and pass clippy for `aarch64-apple-darwin` but have never run. The FSEvents translation is tested on Linux with synthetic events shaped like what `notify` produces from merged flags.
- `notify` 8's inotify backend also subscribes to open and close-without-write events, so reads in a watched tree generate (filtered) kernel events. Harmless, but it adds queue pressure during a large `grep -r`.
- After an inotify overflow, deletions in the lost window are not reported, and modifications are judged by mtime.
- A directory moved out of the tree produces no per-file deletions (the index keeps hashes, not names).
- Relative paths in project files reach `build` unresolved (`SourceEnv` has no base directory); the daemon must pass absolute paths.
