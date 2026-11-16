// The mock machine: a student's laptop with a university folder, a couple of repositories, a
// website, a calendar that is failing, and project files in several trust states.

import type {
  AgentConfig,
  ConfigError,
  ConfigOrigin,
  Project,
  RuleConfig,
  RuleRuntimeStatus,
  SourceConfig,
  SourceHealth,
} from "../contracts";

export const HOME = "/home/ada";
export const PULSE_HOME = `${HOME}/.pulse`;
export const USER_FILE: ConfigOrigin = { kind: "user", path: `${PULSE_HOME}/pulse.toml` };
export const THESIS: ConfigOrigin = {
  kind: "project",
  name: "thesis",
  dir: `${HOME}/thesis`,
  path: `${HOME}/thesis/pulse.toml`,
};

export interface Def<T> {
  config: T;
  origin: ConfigOrigin;
}

export function sources(now: number): (Def<SourceConfig> & { health: SourceHealth })[] {
  const iso = (ago: number) => new Date(now - ago).toISOString();
  return [
    {
      config: { name: "university", enabled: true, kind: "fs", path: "~/University", ignore: ["**/.obsidian/**"], recursive: true },
      origin: USER_FILE,
      health: { state: "healthy", since: iso(6 * 3_600_000), events_emitted: 0, last_event_at: null },
    },
    {
      config: { name: "pulse-repo", enabled: true, kind: "git", path: "~/code/pulse" },
      origin: USER_FILE,
      health: { state: "healthy", since: iso(6 * 3_600_000), events_emitted: 0 },
    },
    {
      config: { name: "site", enabled: true, kind: "fs", path: "~/code/site", ignore: ["node_modules/**", "dist/**"], recursive: true, quiet: "1s" },
      origin: USER_FILE,
      health: { state: "healthy", since: iso(6 * 3_600_000), events_emitted: 0 },
    },
    {
      config: { name: "downloads", enabled: true, kind: "fs", path: "~/Downloads", ignore: [], recursive: true, burst_threshold: 200 },
      origin: USER_FILE,
      health: {
        state: "degraded",
        message:
          "Watch limit reached (8192 inotify watches): 214 new subdirectories under ~/Downloads are not watched. Raise fs.inotify.max_user_watches (for example `sudo sysctl fs.inotify.max_user_watches=65536`) or point this source at a smaller directory.",
        since: iso(52 * 60_000),
        events_emitted: 0,
        last_error: { at: iso(52 * 60_000), message: "inotify_add_watch: No space left on device (ENOSPC)" },
      },
    },
    {
      config: { name: "lectures", enabled: true, kind: "calendar", url: "https://calendar.example.edu/u/ada/lectures.ics", refresh: "15m", lead: "10m" },
      origin: USER_FILE,
      health: {
        state: "failed",
        message:
          "The calendar feed answered 404 Not Found. The URL may have changed: check `url` for source \"lectures\" in ~/.pulse/pulse.toml.",
        since: iso(3 * 3_600_000),
        events_emitted: 0,
        last_error: { at: iso(4 * 60_000), message: "GET https://calendar.example.edu/u/ada/lectures.ics: 404 Not Found" },
      },
    },
    {
      config: { name: "morning", enabled: true, kind: "schedule", cron: "0 9 * * 1-5", timezone: "Europe/Rome" },
      origin: USER_FILE,
      health: { state: "healthy", since: iso(6 * 3_600_000), events_emitted: 0 },
    },
    {
      config: { name: "old-photos", enabled: false, kind: "fs", path: "~/Pictures/2019", ignore: [], recursive: true },
      origin: USER_FILE,
      health: { state: "disabled", since: iso(9 * 86_400_000), events_emitted: 0 },
    },
    {
      config: { name: "thesis/tex", enabled: true, kind: "fs", path: ".", ignore: ["build/**"], recursive: true },
      origin: THESIS,
      health: { state: "healthy", since: iso(6 * 3_600_000), events_emitted: 0 },
    },
  ];
}

export function agents(): Def<AgentConfig>[] {
  return [
    {
      config: {
        name: "study",
        adapter: "claude-code",
        model: "sonnet",
        permission_mode: "acceptEdits",
        allowed_tools: ["Read", "Write", "Edit"],
        extra_args: [],
        allow: ["fs.*:~/University/**", "calendar.*", "schedule.fired"],
        cwd: "~/University",
        timeout: "15m",
        max_concurrent: 1,
        max_queued: 8,
        env: {},
      },
      origin: USER_FILE,
    },
    {
      config: {
        name: "fixer",
        adapter: "codex",
        sandbox: "workspace-write",
        extra_args: [],
        allow: ["process.exited", "git.*:~/code/**"],
        cwd: "~/code/pulse",
        timeout: "20m",
        max_concurrent: 1,
        max_queued: 4,
        env: { RUST_BACKTRACE: "1" },
      },
      origin: USER_FILE,
    },
    {
      config: {
        name: "formatter",
        adapter: "command",
        command: ["prettier", "--write", "--", "{{path}}"],
        stdin: "none",
        allow: ["fs.file.*:~/code/site/**"],
        cwd: "~/code/site",
        timeout: "1m",
        max_concurrent: 2,
        max_queued: 8,
        env: {},
      },
      origin: USER_FILE,
    },
    {
      config: {
        name: "editor",
        adapter: "mcp",
        allow: ["fs.*:~/code/**", "git.*", "process.exited"],
        timeout: "15m",
        max_concurrent: 1,
        max_queued: 8,
        env: {},
      },
      origin: USER_FILE,
    },
    {
      config: {
        name: "thesis/builder",
        adapter: "command",
        command: ["latexmk", "-pdf", "-interaction=nonstopmode", "main.tex"],
        stdin: "none",
        allow: ["fs.*:~/thesis/**"],
        cwd: "~/thesis",
        timeout: "5m",
        max_concurrent: 1,
        max_queued: 2,
        env: {},
      },
      origin: THESIS,
    },
  ];
}

export function rules(): Def<RuleConfig>[] {
  const base = { where: {}, debounce_by: "rule" as const, supervised: false, enabled: true, archived: false };
  return [
    {
      config: {
        ...base,
        name: "new-lecture",
        description: "Summarize each new lecture PDF and write flashcards next to it.",
        on: ["fs.file.created", "fs.file.moved"],
        where: { path: "~/University/**/*.pdf" },
        debounce: "3s",
        debounce_by: "subject",
        cooldown: "30s",
        rate_limit: "10/hour",
        run: {
          agent: "study",
          prompt:
            "A new lecture PDF appeared: {{subject}}.\nSummarize it in ten bullet points and write Anki-style flashcards to a file next to it called {{name}}.cards.md.",
        },
      },
      origin: USER_FILE,
    },
    {
      config: {
        ...base,
        name: "diagnose-failure",
        description: "When a wrapped command fails, ask Codex what went wrong. Needs approval.",
        on: ["process.exited"],
        where: { success: false, cwd: "~/code/**" },
        supervised: true,
        cooldown: "2m",
        run: {
          agent: "fixer",
          prompt:
            "`{{subject}}` failed with exit code {{exit_code}} in {{cwd}}. Read the output tail, find the cause, and propose a minimal fix. Do not commit.",
        },
      },
      origin: USER_FILE,
    },
    {
      config: {
        ...base,
        name: "format-on-save",
        description: "Run prettier on files saved in the site.",
        on: ["fs.file.modified"],
        where: { path: "~/code/site/src/**/*.{ts,tsx,css}" },
        cooldown: "5s",
        run: { agent: "formatter" },
      },
      origin: USER_FILE,
    },
    {
      config: {
        ...base,
        name: "sync-notes",
        description: "Keep the notes index up to date.",
        on: ["fs.file.*"],
        where: { path: "~/University/notes/**" },
        run: { agent: "study", prompt: "Update ~/University/notes/INDEX.md for the change to {{subject}}." },
      },
      origin: USER_FILE,
    },
    {
      config: {
        ...base,
        name: "morning-brief",
        on: ["schedule.fired"],
        where: { source: "morning" },
        run: { agent: "study", prompt: "Write today's study plan to ~/University/today.md from the calendar and open tasks." },
      },
      origin: USER_FILE,
    },
    {
      config: {
        ...base,
        name: "lecture-prep",
        on: ["calendar.event.starting"],
        where: { minutes_until: { lte: 15 } },
        rate_limit: "3/hour",
        run: { agent: "study", prompt: "{{title}} starts in {{minutes_until}} minutes in {{location}}. List what to review." },
      },
      origin: USER_FILE,
    },
    {
      config: {
        ...base,
        name: "commit-review",
        description: "Review each commit on main. Disabled while the repo is being restructured.",
        on: ["git.commit.created"],
        where: { branch: "main" },
        enabled: false,
        run: { agent: "fixer", prompt: "Review commit {{sha}} in {{repo}}: {{message}}" },
      },
      origin: USER_FILE,
    },
    {
      config: {
        ...base,
        name: "old-experiment",
        on: ["fs.burst"],
        archived: true,
        run: { agent: "study", prompt: "Something big happened in {{subject}}." },
      },
      origin: USER_FILE,
    },
    {
      config: {
        ...base,
        name: "thesis/build-on-change",
        description: "Rebuild the thesis PDF when a .tex file changes.",
        on: ["fs.file.modified"],
        where: { path: "~/thesis/**/*.tex" },
        debounce: "5s",
        run: { agent: "thesis/builder" },
      },
      origin: THESIS,
    },
  ];
}

export function ruleStates(now: number): Record<string, RuleRuntimeStatus> {
  const at = (ms: number) => new Date(now + ms).toISOString();
  const tomorrow9 = new Date(now);
  tomorrow9.setDate(tomorrow9.getDate() + 1);
  tomorrow9.setHours(9, 0, 0, 0);
  return {
    "sync-notes": { circuit_open_until: at(38 * 60_000), last_fired_at: at(-22 * 60_000) },
    "morning-brief": { snoozed_until: tomorrow9.toISOString(), last_fired_at: at(-26 * 3_600_000) },
  };
}

export function projects(now: number): Project[] {
  const iso = (ago: number) => new Date(now - ago).toISOString();
  return [
    {
      name: "thesis",
      dir: `${HOME}/thesis`,
      file: `${HOME}/thesis/pulse.toml`,
      state: "trusted",
      trusted_hash: "9f2c4b1d7e8a6f3c0b5d2e1a4c7f8b9e0d3a6c5b2e1f4a7d8c9b0e3f6a2d5c8b",
      current_hash: "9f2c4b1d7e8a6f3c0b5d2e1a4c7f8b9e0d3a6c5b2e1f4a7d8c9b0e3f6a2d5c8b",
      trusted_at: iso(12 * 86_400_000),
    },
    {
      name: "notes",
      dir: `${HOME}/notes`,
      file: `${HOME}/notes/pulse.toml`,
      state: "trusted",
      trusted_hash: "1b4e7a0d3c6f9b2e5a8d1c4f7b0e3a6d9c2f5b8e1a4d7c0f3b6e9a2d5c8f1b4e",
      current_hash: "1b4e7a0d3c6f9b2e5a8d1c4f7b0e3a6d9c2f5b8e1a4d7c0f3b6e9a2d5c8f1b4e",
      trusted_at: iso(3 * 86_400_000),
    },
    {
      name: "dotfiles",
      dir: `${HOME}/code/dotfiles`,
      file: `${HOME}/code/dotfiles/pulse.toml`,
      state: "changed",
      trusted_hash: "c3f6a9d2e5b8c1f4a7d0e3b6c9f2a5d8e1b4c7f0a3d6e9b2c5f8a1d4e7b0c3f6",
      current_hash: "e7b0c3f6a9d2e5b8c1f4a7d0e3b6c9f2a5d8e1b4c7f0a3d6e9b2c5f8a1d4e7b0",
      trusted_at: iso(40 * 86_400_000),
    },
    {
      name: "vendor-lib",
      dir: `${HOME}/code/vendor-lib`,
      file: `${HOME}/code/vendor-lib/pulse.toml`,
      state: "untrusted",
      current_hash: "5a8d1c4f7b0e3a6d9c2f5b8e1a4d7c0f3b6e9a2d5c8f1b4e7a0d3c6f9b2e5a8d",
    },
  ];
}

export function configErrors(): ConfigError[] {
  return [
    {
      file: `${HOME}/notes/pulse.toml`,
      line: 14,
      column: 12,
      item: 'rule "daily-digest"',
      field: "debounce",
      message: 'expected a duration like "500ms", "10s", "15m", "1h30m" or "7d", got "5 min"',
    },
    {
      file: `${HOME}/notes/pulse.toml`,
      line: 22,
      column: 1,
      item: 'agent "notes-bot"',
      field: "allow[0]",
      message: 'invalid scope "fs.*:notes/**": expected "<type glob>" or "<type glob>:<path glob>" with an absolute path',
    },
  ];
}
