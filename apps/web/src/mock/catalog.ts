// A copy of `pulse_contracts::catalog` for the mock's `system.event_types` and path checks.
// The real daemon serves the catalog; the app never hard-codes it.

import type { EventTypeInfo } from "../contracts";

const fsFile = (size: number | null) => ({
  path: "/home/me/University/Architettura/lecture-07.pdf",
  dir: "/home/me/University/Architettura",
  name: "lecture-07.pdf",
  ext: "pdf",
  size,
});
const calendar = {
  uid: "4f1c2e@calendar.example.com",
  title: "Architettura degli elaboratori",
  start: "2026-11-16T10:00:00.000Z",
  end: "2026-11-16T12:00:00.000Z",
  location: "Aula B",
  minutes_until: 10,
};

export const CATALOG: EventTypeInfo[] = [
  { type: "fs.file.created", description: "A file appeared and has stopped changing (after the source's quiet period).", subject: "the file's path", path_fields: ["subject", "data.path"], example: fsFile(482113) },
  { type: "fs.file.modified", description: "An existing file changed and has stopped changing.", subject: "the file's path", path_fields: ["subject", "data.path"], example: fsFile(491002) },
  { type: "fs.file.deleted", description: "A file was removed.", subject: "the file's path", path_fields: ["subject", "data.path"], example: fsFile(null) },
  {
    type: "fs.file.moved",
    description: "A file was renamed or moved within the watched tree.",
    subject: "the new path",
    path_fields: ["subject", "data.path", "data.from"],
    example: { from: "/home/me/University/Architettura/draft.pdf", ...fsFile(482113) },
  },
  {
    type: "fs.burst",
    description: "Many changes under one directory, folded into one event. Carries counts and a small sample, not every path.",
    subject: "the common directory",
    path_fields: ["subject", "data.sample[]"],
    example: { dir: "/home/me/University/dataset", count: 10412, created: 10400, modified: 12, deleted: 0, sample: ["/home/me/University/dataset/a/0001.csv"] },
  },
  {
    type: "git.commit.created",
    description: "A commit was created in a watched repository.",
    subject: "the repository path",
    path_fields: ["subject", "data.repo"],
    example: { repo: "/home/me/code/pulse", branch: "main", sha: "3f2a9c1e0b7d4a6f8e5c2b1a0d9e8f7c6b5a4d3e", message: "Fix flaky watcher test", author: "Ada <ada@example.com>" },
  },
  { type: "git.branch.switched", description: "HEAD moved to another branch.", subject: "the repository path", path_fields: ["subject", "data.repo"], example: { repo: "/home/me/code/pulse", from: "main", to: "feature/ingest" } },
  {
    type: "git.merge.conflicted",
    description: "A merge, rebase or cherry-pick stopped with conflicts.",
    subject: "the repository path",
    path_fields: ["subject", "data.repo", "data.files[]"],
    example: { repo: "/home/me/code/pulse", files: ["/home/me/code/pulse/src/main.rs"] },
  },
  {
    type: "git.push.completed",
    description: "A push from this machine completed.",
    subject: "the repository path",
    path_fields: ["subject", "data.repo"],
    example: { repo: "/home/me/code/pulse", remote: "origin", branch: "main", sha: "3f2a9c1e0b7d4a6f8e5c2b1a0d9e8f7c6b5a4d3e" },
  },
  {
    type: "process.exited",
    description: "A command wrapped with `pulse run -- <command>` exited.",
    subject: "the command line",
    path_fields: ["data.cwd"],
    example: { command: ["cargo", "test"], cwd: "/home/me/code/pulse", exit_code: 101, success: false, signal: null, duration_ms: 48211, output_tail: "test result: FAILED. 41 passed; 1 failed" },
  },
  { type: "schedule.fired", description: "A schedule reached its time.", subject: "the schedule (source) name", path_fields: [], example: { cron: "0 9 * * 1-5", every: null, scheduled_for: "2026-11-16T09:00:00.000Z" } },
  { type: "calendar.event.starting", description: "A calendar event starts within the source's lead time.", subject: "the event title", path_fields: [], example: calendar },
  { type: "calendar.event.ended", description: "A calendar event ended.", subject: "the event title", path_fields: [], example: calendar },
  {
    type: "ext.*",
    description: "Anything ingested from outside (`pulse ingest`, `POST /ingest`). Always `ext.<source>.<name>`; external sources cannot emit other types.",
    subject: "supplied by the sender",
    path_fields: [],
    example: { anything: "supplied by the sender" },
  },
];
