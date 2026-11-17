// Makes every image in docs/images/: `pnpm --dir apps/web docs:images` (see 0008, "Documentation
// images"). Not a test; playwright.images.config.ts runs it.
//
// A fictional student's day, played on the real daemon (daemon.ts: a temp HOME shown as `~`, a
// temp PULSE_HOME, port 0): lecture PDFs saved under ~/University, a notes app under ~/code with
// real git commits and `pulse run` builds, downloads. Every agent's program is a copy of the
// stand-ins in crates/pulse-agents/tests/fixtures, written into the config before the daemon
// starts, so no real agent can ever run. The stand-ins replay streams written below.
//
// Each image is taken in a light and a dark page at once (the same daemon state), at 1440×900
// and scale 1, after waiting for the specific state it shows. The two desktop images come from
// the Electron shell's `showcase` scenario (apps/desktop/e2e/harness.ts) adopting this daemon,
// when Electron can run here; otherwise they are left as they are and the output says why.

import { spawn, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { hostname, userInfo } from "node:os";
import { dirname, join, resolve } from "node:path";

import { expect, test as base, type Browser, type Page } from "@playwright/test";

import { fixtures, pulseBin, startDaemon, type Daemon } from "./daemon";

const repo = resolve(import.meta.dirname, "../../..");
const OUT = resolve(process.env.DOCS_IMAGES_OUT ?? join(repo, "docs/images"));
const mod = process.platform === "darwin" ? "Meta" : "Control";
const THEMES = ["light", "dark"] as const;
type Theme = (typeof THEMES)[number];

// The clock the pages show: a fixed UTC offset that puts "now" at mid-morning, so the
// images read as a day's work whenever they are made, and the machine's own zone is not shown.
const zone = (() => {
  let d = (10 - new Date().getUTCHours() + 24) % 24;
  if (d > 14) d -= 24;
  return d === 0 ? "Etc/GMT" : d > 0 ? `Etc/GMT-${d}` : `Etc/GMT+${-d}`;
})();

// ---------------------------------------------------------------------------------------------
// The stand-in agents' streams: Claude Code's stream-json, as the fixtures record it, cut down to
// the fields the adapter reads. A line "#write PATH" makes the stand-in write that file right
// after the tool call before it, as Claude Code does.

type Line = object | string;
const session = "00000000-0000-4000-8000-0000000000d0";
const init = (cwd: string): Line => ({ type: "system", subtype: "init", cwd, session_id: session, tools: ["Glob", "Grep", "Read", "Write"], mcp_servers: [], model: "claude-haiku-5-5", permissionMode: "dontAsk", claude_code_version: "2.1.325" });
const thinking = (text: string): Line => ({ type: "assistant", message: { role: "assistant", content: [{ type: "thinking", thinking: text }] } });
const say = (text: string): Line => ({ type: "assistant", message: { role: "assistant", content: [{ type: "text", text }] } });
let toolIds = 0;
const tool = (name: string, input: Record<string, string>, result: string): Line[] => {
  const id = `toolu_demo${String(++toolIds).padStart(4, "0")}`;
  return [
    { type: "assistant", message: { role: "assistant", content: [{ type: "tool_use", id, name, input }] } },
    ...(name === "Write" ? [`#write ${input.file_path}`] : []),
    { type: "user", message: { role: "user", content: [{ tool_use_id: id, type: "tool_result", content: result }] } },
  ];
};
const finish = (text: string, cost: number, turns: number): Line => ({ type: "result", subtype: "success", is_error: false, result: text, total_cost_usd: cost, num_turns: turns, session_id: session });
// Progress lines the adapter drops: they keep a run going without adding output.
const quiet = (n: number): Line[] => Array.from({ length: n }, (_, i) => ({ type: "system", subtype: "thinking_tokens", estimated_tokens: 40 * (i + 1), session_id: session }));

interface Lecture {
  /** Relative to ~/University. */
  file: string;
  title: string;
  pages: number;
  sections: string[];
  cards: string[];
}

const LECTURES: Record<string, Lecture> = {
  determinants: {
    file: "Linear Algebra/lecture-04-determinants.pdf",
    title: "Lecture 4: Determinants",
    pages: 9,
    sections: [
      "Section 1: The determinant as signed volume. det(A) is the factor by which A scales areas (2D) or volumes (3D); its sign says whether orientation flips.",
      "Section 2: Cofactor expansion along any row or column, and why row reduction is the practical way to compute it: O(n^3) instead of O(n!).",
      "Section 3: Properties. det(AB) = det(A) det(B); det(A^T) = det(A); A is invertible exactly when det(A) ≠ 0.",
    ],
    cards: ["What does the sign of a determinant tell you?", "Which row operation does not change the determinant?", "When is a square matrix invertible?", "What is det(AB)?"],
  },
  dynamic: {
    file: "Algorithms/week-05-dynamic-programming.pdf",
    title: "Week 5: Dynamic programming",
    pages: 14,
    sections: [
      "Section 1: Optimal substructure and overlapping subproblems, the two conditions under which dynamic programming beats plain recursion.",
      "Section 2: Memoization (top-down) against tabulation (bottom-up), with Fibonacci and rod cutting as examples.",
      "Section 3: Longest common subsequence in O(nm), and reconstructing the answer from the table.",
    ],
    cards: ["What two properties make a problem suitable for DP?", "Memoization or tabulation: which one is top-down?", "What is the running time of LCS?", "How do you recover the subsequence itself?"],
  },
  greedy: {
    file: "Algorithms/week-06-greedy-algorithms.pdf",
    title: "Week 6: Greedy algorithms",
    pages: 11,
    sections: [
      "Section 1: The greedy-choice property, and the exchange argument used to prove a greedy algorithm optimal.",
      "Section 2: Interval scheduling: pick the interval that finishes first. Huffman codes: merge the two rarest symbols.",
      "Section 3: Where greedy fails: 0/1 knapsack, and coin systems where taking the largest coin is not optimal.",
    ],
    cards: ["What is an exchange argument?", "Which interval does greedy scheduling pick first?", "Why does greedy fail for 0/1 knapsack?", "What does Huffman coding merge at each step?"],
  },
  shortest: {
    file: "Algorithms/week-07-shortest-paths.pdf",
    title: "Week 7: Shortest paths",
    pages: 18,
    sections: [
      "Section 1: Single-source shortest paths. Relaxation: if d[u] + w(u,v) < d[v], set d[v] = d[u] + w(u,v). Every algorithm here is an order in which to relax edges.",
      "Section 2: Dijkstra. Take the closest vertex not yet final, relax its edges. Correct for non-negative weights; O((V+E) log V) with a binary heap.",
      "Section 3: Why negative edges break Dijkstra. A vertex taken early could later be reached more cheaply through a negative edge, which violates the invariant.",
      "Section 4: Bellman-Ford. Relax every edge V-1 times. One more pass that still improves a distance proves a negative cycle reachable from s. O(VE).",
      "Section 5: Floyd-Warshall. Dynamic programming over intermediate vertices: d_k[i][j] = min(d_{k-1}[i][j], d_{k-1}[i][k] + d_{k-1}[k][j]). O(V^3); works with negative edges but not negative cycles.",
      "Section 6: Choosing an algorithm. Non-negative weights: Dijkstra. Negative edges, one source: Bellman-Ford. Dense graph, all pairs: Floyd-Warshall. Unweighted: breadth-first search.",
    ],
    cards: [
      "When is a vertex's distance final in Dijkstra?",
      "What detects a negative cycle?",
      "What is the running time of Bellman-Ford?",
      "What does Floyd-Warshall iterate over?",
      "Which algorithm for an unweighted graph?",
      "What does relaxing an edge do?",
      "Why can't Dijkstra handle negative edges?",
      "When is Floyd-Warshall the better choice?",
    ],
  },
};

function studyStream(home: string, l: Lecture, tail: number): Line[] {
  const pdf = join(home, "University", l.file);
  const base = pdf.replace(/\.pdf$/, "");
  const name = base.slice(base.lastIndexOf("/") + 1);
  return [
    init(join(home, "University")),
    thinking("A new lecture PDF. Read it first, then write the summary and the flashcards next to it."),
    ...tool("Read", { file_path: pdf }, `PDF, ${l.pages} pages. ${l.title}`),
    say(`${l.title}: ${l.pages} pages. Summary:`),
    ...l.sections.map(say),
    ...tool("Write", { file_path: `${base}.summary.md`, content: `# ${l.title}\n\n${l.sections.join("\n\n")}\n` }, `File created successfully at: ${base}.summary.md`),
    ...l.cards.map((c, i) => say(`Flashcard ${i + 1}: ${c}`)),
    ...tool("Write", { file_path: `${base}.flashcards.md`, content: l.cards.map((c) => `Q: ${c}\nA: …`).join("\n\n") }, `File created successfully at: ${base}.flashcards.md`),
    ...quiet(tail),
    say(`Wrote ${name}.summary.md and ${name}.flashcards.md (${l.cards.length} cards).`),
    finish(`Wrote ${name}.summary.md and ${name}.flashcards.md (${l.cards.length} cards).`, 0.0061, 4),
  ];
}

function diagnoseBuild(home: string): Line[] {
  const repoDir = join(home, "code", "notes-app");
  return [
    init(join(home, "code")),
    thinking("cargo build failed in notes-app. The output tail names src/filter.rs line 42."),
    ...tool("Read", { file_path: join(repoDir, "src", "filter.rs") }, "pub fn by_tag(notes: &[Note], wanted: &str) -> Vec<&Note> {\n    notes.iter().filter(|n| n.tags.contains(tag)).collect()\n}"),
    say("The build fails because `by_tag` filters on `tag`, which does not exist: the parameter is called `wanted`."),
    say("Fix: in src/filter.rs line 42, use `n.tags.iter().any(|t| t == wanted)`."),
    finish("`by_tag` uses an undefined `tag`; the parameter is `wanted`. Change line 42 to `n.tags.iter().any(|t| t == wanted)`.", 0.0034, 2),
  ];
}

function reviewCommit(home: string): Line[] {
  const repoDir = join(home, "code", "notes-app");
  return [
    init(join(home, "code")),
    ...tool("Read", { file_path: join(repoDir, "src", "filter.rs") }, "pub fn by_tag(notes: &[Note], wanted: &str) -> Vec<&Note> { … }"),
    say("The commit adds tags to notes and a filter by tag. The filter compares tags case-sensitively, so `Exam` and `exam` are different tags; that may be intended."),
    say("No test covers a note without tags. Otherwise it looks good."),
    finish("Looks good. Consider a test for notes without tags, and whether tags should be case-insensitive.", 0.0029, 2),
  ];
}

const writeStream = (file: string, lines: Line[]) => {
  // Replaced, not rewritten: a stand-in that is already reading the old stream keeps it.
  writeFileSync(`${file}.next`, `${lines.map((l) => (typeof l === "string" ? l : JSON.stringify(l))).join("\n")}\n`);
  renameSync(`${file}.next`, file);
};

// ---------------------------------------------------------------------------------------------
// The configuration: the shipped templates' rules, with the stand-ins as programs. They are found
// on the daemon's PATH (~/bin first), under names no real agent has.

const config = (home: string) => {
  const streams = join(dirname(home), "streams");
  return `# A student's machine: the scenario of the documentation images.

[limits]
# Short, so the scenario's own edits of CHANGELOG.md are not taken for the formatter's.
causation_grace = "5s"

[[source]]
name = "university"
kind = "fs"
path = "~/University"

[[source]]
name = "notes-app"
kind = "git"
path = "~/code/notes-app"

[[source]]
name = "notes-docs"
kind = "fs"
path = "~/code/notes-app/docs"

[[source]]
name = "downloads"
kind = "fs"
path = "~/Downloads"

[[source]]
name = "archive"
kind = "fs"
path = "~/Archive"

[[source]]
name = "lectures"
kind = "calendar"
path = "~/Calendar/lectures.ics"

[[agent]]
name = "study"
adapter = "claude-code"
program = "fake-claude"
allowed_tools = ["Read", "Edit(./**)"]
cwd = "~/University"
allow = ["fs.file.created:~/University/**"]
timeout = "15m"
max_concurrent = 1
# No queue: PDFs that arrive together are summarized one after another (their debounce waits).
max_queued = 0
env = { FAKE_AGENT_STREAM = "${streams}/study.jsonl", FAKE_AGENT_DELAY = "0.4" }

[[agent]]
name = "diagnose"
adapter = "claude-code"
program = "fake-claude"
allowed_tools = ["Read", "Grep", "Glob"]
cwd = "~/code"
allow = ["process.exited:~/code/**", "git.commit.created:~/code/**"]
timeout = "10m"
max_concurrent = 1
max_queued = 2
env = { FAKE_AGENT_STREAM = "${streams}/diagnose.jsonl", FAKE_AGENT_DELAY = "0.2" }

[[agent]]
name = "formatter"
adapter = "command"
command = ["fake-agent", "write-file", "{{path}}", "# Changelog"]
cwd = "~/code/notes-app/docs"
allow = ["fs.file.modified:~/code/notes-app/docs/**", "fs.file.created:~/Downloads/**"]

[[rule]]
name = "lecture-notes"
description = "Summarize each new lecture PDF and make flashcards from it."
on = "fs.file.created"
where = { path = "~/University/**/*.pdf" }
debounce = "10s"
debounce_by = "subject"
rate_limit = "20/hour"

[rule.run]
agent = "study"
prompt = """
A new lecture PDF was saved at {{path}}.

Read it, then write a summary and flashcards next to it.
"""

[[rule]]
name = "build-failed"
description = "Explain why a wrapped build or test command failed."
on = "process.exited"
where = { success = false, cwd = "~/code/**" }
cooldown = "2m"
rate_limit = "10/hour"

[rule.run]
agent = "diagnose"
prompt = """
A command wrapped with \`pulse run\` failed in {{cwd}}.

Find the most likely cause in the code and explain it in a few sentences, then give the fix you
suggest. Do not change any files.
"""

[[rule]]
name = "format-docs"
description = "Format Markdown in the notes app's docs when it changes."
on = "fs.file.modified"
where = { path = "~/code/notes-app/docs/**/*.md" }
run = { agent = "formatter" }

[[rule]]
name = "sort-downloads"
description = "File new downloads into folders by type."
on = "fs.file.created"
where = { path = "~/Downloads/*" }
enabled = false
run = { agent = "formatter" }

[[rule]]
name = "review-commit"
description = "Review each commit to the notes app before it is pushed."
on = "git.commit.created"
supervised = true

[rule.run]
agent = "diagnose"
prompt = "Review commit {{sha}} on {{branch}}: {{message}}"
`;
};

// The project file the trust review shows. Never trusted here.
const PROJECT_FILE = `# Rules for this repository. They do nothing until someone runs
# \`pulse project trust ~/code/notes-app\` after reading them.

[[agent]]
name = "lint"
adapter = "command"
command = ["cargo", "clippy", "--quiet", "--message-format=short"]
cwd = "."
allow = ["git.commit.created:~/code/notes-app/**"]

[[rule]]
name = "lint-on-commit"
description = "Run clippy on every commit."
on = "git.commit.created"
run = { agent = "lint" }
`;

// A stand-in for cargo on the PATH of \`pulse run\`: a build and a test run that fail.
const FAKE_CARGO = `#!/bin/sh
# Stands in for cargo in the documentation scenario: prints a failing build or test run.
case "$1" in
build)
  echo "   Compiling notes-app v0.3.0" >&2
  echo "error[E0425]: cannot find value \\\`tag\\\` in this scope" >&2
  echo "  --> src/filter.rs:42:44" >&2
  echo "error: could not compile \\\`notes-app\\\` (lib) due to 1 previous error" >&2
  exit 101 ;;
test)
  echo "running 12 tests"
  echo "test filter::tests::notes_without_tags ... FAILED"
  echo "test result: FAILED. 11 passed; 1 failed"
  exit 101 ;;
esac
exit 0
`;

const gitEnv = (home: string) => ({
  PATH: process.env.PATH,
  HOME: home,
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_AUTHOR_NAME: "Sam Rivera",
  GIT_AUTHOR_EMAIL: "sam@example.com",
  GIT_COMMITTER_NAME: "Sam Rivera",
  GIT_COMMITTER_EMAIL: "sam@example.com",
  TZ: "UTC",
});

function prepare(home: string) {
  const bin = join(home, "bin");
  mkdirSync(bin, { recursive: true });
  for (const f of ["fake-claude", "fake-agent"]) {
    copyFileSync(join(fixtures, f), join(bin, f));
    chmodSync(join(bin, f), 0o755);
  }
  writeFileSync(join(bin, "cargo"), FAKE_CARGO, { mode: 0o755 });
  mkdirSync(join(dirname(home), "streams"), { recursive: true });
  for (const d of ["University/Algorithms", "University/Linear Algebra", "Downloads", "code/notes-app/src", "code/notes-app/docs"]) {
    mkdirSync(join(home, d), { recursive: true });
  }
  const app = join(home, "code", "notes-app");
  writeFileSync(join(app, "Cargo.toml"), '[package]\nname = "notes-app"\nversion = "0.3.0"\nedition = "2024"\n');
  writeFileSync(join(app, "src", "filter.rs"), "pub fn by_tag(notes: &[Note], wanted: &str) -> Vec<&Note> {\n    notes.iter().filter(|n| n.tags.contains(tag)).collect()\n}\n");
  writeFileSync(join(app, "docs", "CHANGELOG.md"), "# Changelog\n\n## 0.3.0\n\n- Search across notebooks.\n");
  writeFileSync(join(app, "pulse.toml"), PROJECT_FILE);
  const git = (...args: string[]) => spawnSync("git", args, { cwd: app, env: gitEnv(home), stdio: "ignore" });
  git("init", "--quiet", "--initial-branch=main");
  git("add", ".");
  git("commit", "--quiet", "-m", "Search across notebooks");
}

// ---------------------------------------------------------------------------------------------

interface Delivery {
  event: string;
  rule: string;
  outcome: { kind: string; reason?: { kind: string } };
  run: string | null;
}
interface RunSummary {
  id: string;
  rule: string;
  event: string;
  status: string;
}

/** Process groups of desktop runs still going, stopped in teardown if the script did not end them. */
const groups = new Set<number>();

function stopGroup(pid: number) {
  try {
    process.kill(-pid, "SIGKILL");
  } catch {
    // Already gone.
  }
}

// A fixture, so that the daemon is stopped and its directory removed also after a timeout.
const test = base.extend<{ daemon: Daemon }>({
  // oxlint-disable-next-line no-empty-pattern -- Playwright fixtures take their dependencies by destructuring.
  daemon: async ({}, use) => {
    // /tmp rather than $TMPDIR: the home's path appears in event data, and $TMPDIR can name the user.
    const d = await startDaemon(config, { tmp: { parent: existsSync("/tmp") ? "/tmp" : dirname(repo), prefix: "pulse-demo-" }, prepare, path: (home) => [join(home, "bin")] });
    try {
      await use(d);
    } finally {
      for (const pid of groups) stopGroup(pid);
      await d.dispose();
    }
  },
});

test("documentation images", async ({ browser, daemon }) => {
  mkdirSync(OUT, { recursive: true });
  await scenario(browser, daemon);
});

async function scenario(browser: Browser, daemon: Daemon) {
  const home = daemon.home;
  const streams = join(dirname(home), "streams");
  const app = join(home, "code", "notes-app");
  const base = `http://127.0.0.1:${daemon.port}`;

  const json = <T>(...args: string[]) => JSON.parse(daemon.pulse("--json", ...args)) as T;
  const deliveries = (rule: string) => json<{ deliveries: Delivery[] }>("rules", "history", rule, "--limit", "200").deliveries;
  const runs = () => json<{ runs: RunSummary[] }>("runs", "list", "--limit", "200").runs;
  const count = (rule: string, kind: string) => deliveries(rule).filter((d) => d.outcome.kind === kind).length;
  const until = async (what: string, check: () => boolean) => expect.poll(check, { message: what, timeout: 60_000 }).toBe(true);

  const pdf = (l: Lecture) => {
    const path = join(home, "University", l.file);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, `%PDF-1.7\n% ${l.title}\n`);
  };
  const touchChangelog = (line: string) => writeFileSync(join(app, "docs", "CHANGELOG.md"), `${readFileSync(join(app, "docs", "CHANGELOG.md"), "utf8")}- ${line}\n`);
  const commit = (subject: string, file: string, text: string) => {
    writeFileSync(join(app, file), text);
    const env = gitEnv(home);
    spawnSync("git", ["add", "."], { cwd: app, env, stdio: "ignore" });
    const r = spawnSync("git", ["commit", "--quiet", "-m", subject], { cwd: app, env, encoding: "utf8" });
    if (r.status !== 0) throw new Error(`git commit failed: ${r.stderr}`);
  };
  const pulseRun = (...cmd: string[]) => {
    const r = spawnSync(pulseBin, ["run", "--", ...cmd], { cwd: app, env: daemon.env, encoding: "utf8" });
    if (r.status === null || r.status === 0) throw new Error(`pulse run ${cmd.join(" ")}: expected a failure, got ${r.status}`);
  };

  // ---- The morning, as it happened -----------------------------------------------------------

  // Two lectures at once: the second waits for the first run (the agent has no queue).
  writeStream(join(streams, "study.jsonl"), studyStream(home, LECTURES.determinants!, 0));
  pdf(LECTURES.determinants!);
  await until("the first lecture is debounced", () => count("lecture-notes", "debounced") >= 1);
  pdf(LECTURES.dynamic!);
  await until("the first lecture fires", () => count("lecture-notes", "fired") >= 1);
  // Its stand-in has opened the stream once it prints: only then is the next one put in place.
  const firstStudy = runs().find((r) => r.rule === "lecture-notes")!;
  await until("the first lecture's run prints", () => daemon.pulse("runs", "logs", firstStudy.id).length > 0);
  writeStream(join(streams, "study.jsonl"), studyStream(home, LECTURES.dynamic!, 0));
  await until("the second lecture waits for the agent", () => count("lecture-notes", "waiting") >= 1);

  writeFileSync(join(home, "Downloads", "bank-statement-october.csv"), "date,amount\n");
  writeFileSync(join(home, "Downloads", "whiteboard.jpg"), "jpg");

  writeStream(join(streams, "diagnose.jsonl"), diagnoseBuild(home));
  pulseRun("cargo", "build");
  await until("the failed build is diagnosed", () => runs().some((r) => r.rule === "build-failed" && r.status === "succeeded"));
  await until("the second lecture fires", () => count("lecture-notes", "fired") >= 2);

  touchChangelog("Tags, and a filter by tag.");
  await until("the formatter's own write is blocked", () => deliveries("format-docs").some((d) => d.outcome.kind === "blocked"));
  await until("both downloads were seen", () => count("sort-downloads", "blocked") >= 2);
  await until("the study runs are done", () => runs().filter((r) => r.rule === "lecture-notes" && r.status === "succeeded").length >= 2);

  writeStream(join(streams, "diagnose.jsonl"), reviewCommit(home));
  commit("Tag notes and filter by tag", "src/tags.rs", "pub struct Tag(pub String);\n");
  await until("the commit awaits approval", () => runs().some((r) => r.rule === "review-commit" && r.status === "pending_approval"));
  const first = runs().find((r) => r.rule === "review-commit" && r.status === "pending_approval")!;
  daemon.pulse("runs", "approve", first.id);
  await until("the review ran", () => runs().some((r) => r.id === first.id && r.status === "succeeded"));

  pulseRun("cargo", "test");
  await until("the test failure is held by the cooldown", () => deliveries("build-failed").some((d) => d.outcome.kind === "blocked"));

  writeStream(join(streams, "study.jsonl"), studyStream(home, LECTURES.greedy!, 0));
  pdf(LECTURES.greedy!);
  await until("the third lecture fires", () => count("lecture-notes", "fired") >= 3);
  await until("its run is done", () => runs().filter((r) => r.rule === "lecture-notes" && r.status === "succeeded").length >= 3);

  touchChangelog("Show tag counts in the sidebar.");
  await until("the second edit fires and its rewrite is blocked", () => deliveries("format-docs").filter((d) => d.outcome.kind === "blocked").length >= 2);
  commit("Show tag counts in the sidebar", "src/sidebar.rs", "pub fn tag_counts() {}\n");
  await until("the second commit awaits approval", () => runs().filter((r) => r.rule === "review-commit").length >= 2);
  daemon.pulse("rules", "snooze", "build-failed", "3h");

  // ---- The pictures --------------------------------------------------------------------------

  const pages = {} as Record<Theme, Page>;
  for (const theme of THEMES) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1, colorScheme: theme, reducedMotion: "reduce", locale: "en-US", timezoneId: zone });
    const page = await context.newPage();
    await page.goto(daemon.loginUrl());
    await expect(page.locator(".conn-line")).toContainText("Connected");
    pages[theme] = page;
  }
  const both = (f: (page: Page) => Promise<void>) => Promise.all(THEMES.map((t) => f(pages[t])));
  const go = (path: string) =>
    both(async (page) => {
      await page.goto(`${base}${path}`);
      await expect(page.locator(".conn-line")).toContainText("Connected");
    });
  const shoot = async (name: string) => {
    for (const theme of THEMES) await capture(pages[theme], join(OUT, `${name}-${theme}.png`));
  };

  // The feed: what each event caused.
  await go("/");
  await both(async (page) => {
    await expect(page.locator(".event-row").first()).toContainText("git.commit.created");
    await expect(page.locator(".event-row", { hasText: "loop" }).first()).toBeVisible();
    await expect(page.locator(".banner")).toContainText("waiting for your approval");
  });
  await shoot("feed");

  // The palette, over the feed.
  await both(async (page) => {
    await page.keyboard.press(`${mod}+k`);
    await expect(page.locator(".palette")).toContainText("Awaiting approval");
  });
  await shoot("palette");
  await both((page) => page.keyboard.press("Escape"));

  // An event loop protection stopped.
  await both(async (page) => {
    await page.locator(".event-row", { hasText: "loop" }).first().click();
    await expect(page.locator(".detail")).toContainText("Deliveries");
    await expect(page.locator(".detail")).toContainText("loop protection");
  });
  await shoot("event-detail");

  // The supervised run waiting for approval.
  const pending = runs().find((r) => r.rule === "review-commit" && r.status === "pending_approval")!;
  await go(`/runs/${pending.id}`);
  await both((page) => expect(page.locator(".run-detail")).toContainText("Approving runs exactly this"));
  await shoot("approval");

  await go("/rules");
  await both((page) => expect(page.locator(".detail, main").first()).toContainText("snoozed"));
  await shoot("rules");

  await go("/sources/lectures");
  await both((page) => expect(page.locator(".detail")).toContainText("does not exist"));
  await shoot("sources");

  // The rule editor and its tester, against the third lecture.
  await go("/rules/lecture-notes/edit");
  await both(async (page) => {
    await page.locator(".pick-event", { hasText: "week-06-greedy-algorithms.pdf" }).click();
    await page.getByRole("button", { name: /^Test/ }).click();
    await expect(page.locator(".test-result")).toContainText("When the debounce elapses");
  });
  await shoot("rule-test");

  // A lecture run streaming its output.
  writeStream(join(streams, "study.jsonl"), studyStream(home, LECTURES.shortest!, 25));
  pdf(LECTURES.shortest!);
  await until("the fourth lecture fires", () => count("lecture-notes", "fired") >= 4);
  const hero = runs().find((r) => r.rule === "lecture-notes" && (r.status === "running" || r.status === "queued"))!;
  await go(`/runs/${hero.id}`);
  await both(async (page) => {
    // The view follows the tail: the result of the last Write comes fully into view.
    await expect(page.locator(".output-scroll").getByText(/successfully at: .*week-07-shortest-paths\.flashcards\.md/)).toBeInViewport({ ratio: 1 });
  });
  expect(runs().find((r) => r.id === hero.id)?.status, "the run is still running when it is captured").toBe("running");
  await shoot("run-output");
  await until("the hero run is done", () => runs().some((r) => r.id === hero.id && r.status === "succeeded"));

  // Reviewing a project file before trusting it (never trusted here).
  await go("/projects");
  await both(async (page) => {
    await page.getByRole("button", { name: "Trust a project" }).click();
    await page.getByRole("dialog").locator("input").first().fill("~/code/notes-app");
    await page.getByRole("dialog").getByRole("button", { name: "Review" }).click();
    await expect(page.getByRole("dialog").locator(".review-def .argv").first()).toContainText("clippy");
  });
  await shoot("trust-review");
  await both((page) => page.keyboard.press("Escape"));

  await both((page) => page.context().close());
  await desktop(daemon);
}

/** One screenshot, after checking that nothing on the page names this machine. */
async function capture(page: Page, file: string) {
  await page.mouse.move(100, 600);
  await page.evaluate(() => document.fonts.ready.then(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))));
  const text = await page.evaluate(() =>
    [document.body.innerText, ...Array.from(document.querySelectorAll("input, textarea, select"), (e) => (e as HTMLInputElement).value), ...Array.from(document.querySelectorAll("[title]"), (e) => e.getAttribute("title") ?? "")].join("\n"),
  );
  const user = userInfo().username;
  for (const secret of [repo, hostname(), user.length >= 3 ? user : null, process.env.HOME]) {
    if (secret && text.includes(secret)) throw new Error(`${file}: the page shows "${secret}", which comes from this machine`);
  }
  await page.screenshot({ path: file, animations: "disabled", caret: "hide" });
  process.stdout.write(`wrote ${file}\n`);
}

// ---------------------------------------------------------------------------------------------
// The desktop shell, adopting this daemon (apps/desktop/e2e/harness.ts, scenario `showcase`).

async function desktop(daemon: Daemon) {
  const dir = join(repo, "apps", "desktop");
  const electron = join(dir, "node_modules", "electron", "dist", "electron");
  const xvfb = spawnSync("sh", ["-c", "command -v xvfb-run"], { encoding: "utf8" }).stdout.trim();
  const missing = process.platform !== "linux" ? "the desktop images are made on Linux, under Xvfb" : !existsSync(electron) ? `Electron is not unpacked (node ${join(dir, "node_modules/electron/install.js")})` : !xvfb ? "xvfb-run is not installed" : null;
  if (missing) {
    process.stdout.write(`desktop-light.png and desktop-dark.png left as they are: ${missing}. See 0010, "Running the end-to-end scenarios".\n`);
    return;
  }
  const build = spawnSync(process.execPath, [join(dir, "scripts", "build.mjs"), "--e2e"], { cwd: dir, stdio: "inherit" });
  if (build.status !== 0) throw new Error("building the desktop harness failed");
  const out = join(dirname(daemon.home), "desktop");
  for (const theme of THEMES) {
    mkdirSync(join(out, theme), { recursive: true });
    const env = {
      PATH: process.env.PATH,
      HOME: daemon.home,
      PULSE_HOME: daemon.pulseHome,
      PULSE_BIN: pulseBin,
      PULSE_E2E: "showcase",
      PULSE_E2E_OUT: join(out, theme),
      PULSE_E2E_THEME: theme,
      XDG_CONFIG_HOME: join(out, theme, "config"),
      XDG_CURRENT_DESKTOP: "GNOME",
      LD_LIBRARY_PATH: process.env.E2E_LD_LIBRARY_PATH ?? "",
      ELECTRON_ENABLE_LOGGING: "0",
      TZ: zone,
      LANG: "en_US.UTF-8",
    };
    const flags = (process.env.E2E_ELECTRON_FLAGS ?? "").split(/\s+/).filter(Boolean);
    // Its own process group, so that everything under xvfb-run can be stopped if it hangs.
    const child = spawn(xvfb, ["-a", "-s", "-screen 0 1440x900x24", electron, ...flags, join(dir, "dist-e2e", "harness.cjs")], { env, detached: true, stdio: ["ignore", "pipe", "pipe"] });
    if (child.pid) groups.add(child.pid);
    let log = "";
    child.stdout.on("data", (d: Buffer) => (log += d.toString()));
    child.stderr.on("data", (d: Buffer) => (log += d.toString()));
    const code = await new Promise<number | null>((resolveExit) => {
      const timer = setTimeout(() => child.pid && stopGroup(child.pid), 90_000);
      child.once("exit", (c) => {
        clearTimeout(timer);
        if (child.pid) groups.delete(child.pid);
        resolveExit(c);
      });
    });
    const shot = join(out, theme, "showcase-feed.png");
    if (code !== 0 || !existsSync(shot)) throw new Error(`the desktop showcase (${theme}) failed with ${code}:\n${log.slice(-4000)}`);
    copyFileSync(shot, join(OUT, `desktop-${theme}.png`));
    process.stdout.write(`wrote ${join(OUT, `desktop-${theme}.png`)}\n`);
  }
}
