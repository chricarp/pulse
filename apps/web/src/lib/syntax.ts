// Client-side checks for config value syntax, mirroring `pulse_contracts` so the editor can say
// what is wrong while typing. The daemon validates again on save and is the authority; when the
// two disagree, the daemon's error is the one shown.

const UNIT_MS: Record<string, [number, number]> = {
  d: [4, 86_400_000],
  h: [3, 3_600_000],
  m: [2, 60_000],
  s: [1, 1000],
  ms: [0, 1],
};

/** `"10s"`, `"1h30m"`, `"500ms"`, `"0"`. Units largest first, each once. */
export function parseDuration(input: string): number | null {
  const t = input.trim();
  if (!t) return null;
  if (t === "0") return 0;
  let total = 0;
  let lastRank = Number.POSITIVE_INFINITY;
  const re = /(\d+)([a-z]+)/gy;
  let consumed = 0;
  for (const m of t.matchAll(re)) {
    const unit = UNIT_MS[m[2] ?? ""];
    if (!unit) return null;
    const [rank, mult] = unit;
    if (rank >= lastRank) return null;
    lastRank = rank;
    total += Number(m[1]) * mult;
    consumed += m[0].length;
  }
  return consumed === t.length ? total : null;
}

export function formatDuration(ms: number): string {
  if (ms === 0) return "0s";
  let rest = ms;
  let out = "";
  for (const [unit, mult] of [
    ["d", 86_400_000],
    ["h", 3_600_000],
    ["m", 60_000],
    ["s", 1000],
    ["ms", 1],
  ] as const) {
    if (rest >= mult) {
      out += `${Math.floor(rest / mult)}${unit}`;
      rest %= mult;
    }
  }
  return out;
}

const RATE_UNITS: Record<string, number> = {
  s: 1000,
  sec: 1000,
  second: 1000,
  m: 60_000,
  min: 60_000,
  minute: 60_000,
  h: 3_600_000,
  hour: 3_600_000,
  d: 86_400_000,
  day: 86_400_000,
};

/** `"10/hour"`, `"5/10m"`. */
export function parseRate(input: string): { count: number; perMs: number } | null {
  const slash = input.indexOf("/");
  if (slash < 0) return null;
  const countText = input.slice(0, slash).trim();
  if (!/^\d+$/.test(countText)) return null;
  const count = Number(countText);
  const perText = input.slice(slash + 1).trim();
  const perMs = RATE_UNITS[perText] ?? parseDuration(perText);
  if (perMs === null || perMs === 0 || count === 0 || count > 10_000) return null;
  return { count, perMs };
}

export function nameProblem(name: string): string | null {
  if (!name) return "Required.";
  if (name.length > 64) return "At most 64 characters.";
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(name)) {
    return "Lowercase letters, digits, '-' or '_', starting with a letter or digit.";
  }
  return null;
}

export function typeGlobProblem(glob: string): string | null {
  if (!glob) return "Empty type.";
  if (glob.length > 200) return "Too long.";
  if (!/^[a-z0-9._\-*{},]+$/.test(glob)) {
    return `"${glob}": use lowercase letters, digits, '.', '_', '-', '*', and {a,b}.`;
  }
  let depth = 0;
  for (const ch of glob) {
    if (ch === "{") depth += 1;
    if (ch === "}") depth -= 1;
    if (depth < 0 || depth > 1) return `"${glob}": braces do not nest.`;
  }
  return depth === 0 ? null : `"${glob}": unclosed '{'.`;
}

/** Brace expansion, one level. */
function expand(glob: string): string[] {
  const open = glob.indexOf("{");
  if (open < 0) return [glob];
  const close = glob.indexOf("}", open);
  if (close < 0) return [glob];
  const head = glob.slice(0, open);
  const tail = glob.slice(close + 1);
  return glob
    .slice(open + 1, close)
    .split(",")
    .flatMap((alt) => expand(head + alt + tail));
}

function escapeRe(s: string): string {
  return s.replace(/[.+^${}()|[\]\\]/g, "\\$&");
}

/** Type globs: `*` crosses `.`, so `fs.*` matches `fs.file.created`. */
export function typeGlobMatches(glob: string, type: string): boolean {
  return expand(glob).some((alt) => {
    const re = new RegExp(`^${alt.split("*").map(escapeRe).join(".*")}$`);
    return re.test(type);
  });
}

/**
 * Whether the type glob `inner` only matches types that `outer` also matches, as far as a
 * catalog type list can tell. Used to explain scopes: "covers git.commit.created".
 */
export function typesCovered(glob: string, catalog: readonly string[]): string[] {
  return catalog.filter((t) => typeGlobMatches(glob, t));
}

function pathSegmentRe(seg: string): string {
  let out = "";
  for (const ch of seg) {
    if (ch === "*") out += "[^/]*";
    else if (ch === "?") out += "[^/]";
    else out += escapeRe(ch);
  }
  return out;
}

/** Path globs: `*` stays inside a segment, `**` is any depth (and `/a/**` matches `/a`). */
export function pathGlobMatches(glob: string, path: string, home: string): boolean {
  const g = glob === "~" ? home : glob.startsWith("~/") ? home + glob.slice(1) : glob;
  return expand(g).some((alt) => {
    const segs = alt.split("/");
    let re = "";
    for (let i = 0; i < segs.length; i++) {
      const s = segs[i] ?? "";
      if (s === "**") {
        re += i === segs.length - 1 ? "(?:/.*)?" : "(?:/[^/]+)*";
      } else {
        re += (i === 0 ? "" : "/") + pathSegmentRe(s);
      }
    }
    return new RegExp(`^${re}$`).test(path);
  });
}

const TEMPLATE_VARS = ["id", "type", "source", "subject", "time", "rule", "agent", "prompt"];

/** `{{ var }}` placeholders and what is wrong with them. */
export function templateProblems(template: string): { vars: string[]; problems: string[] } {
  const vars: string[] = [];
  const problems: string[] = [];
  let rest = template;
  let offset = 0;
  for (;;) {
    const start = rest.indexOf("{{");
    if (start < 0) break;
    const end = rest.indexOf("}}", start + 2);
    if (end < 0) {
      problems.push(`Unclosed "{{" at character ${offset + start + 1}.`);
      break;
    }
    const name = rest.slice(start + 2, end).trim();
    if (!name) problems.push(`Empty placeholder at character ${offset + start + 1}.`);
    else if (
      !TEMPLATE_VARS.includes(name) &&
      !/^(data\.)?[A-Za-z0-9_-]+(\.[A-Za-z0-9_-]+)*$/.test(name)
    ) {
      problems.push(`"{{${name}}}" is not a variable. Use ${TEMPLATE_VARS.join(", ")}, or a data path like path or data.repo.`);
    } else vars.push(name);
    offset += end + 2;
    rest = rest.slice(end + 2);
  }
  return { vars, problems };
}

/** `"<type glob>"` or `"<type glob>:<path glob>"`. */
export function parseScope(scope: string): { types: string; path: string | null; problem: string | null } {
  const colon = scope.indexOf(":");
  const types = colon >= 0 ? scope.slice(0, colon) : scope;
  const path = colon >= 0 ? scope.slice(colon + 1) : null;
  let problem = typeGlobProblem(types);
  if (!problem && path !== null && !(path.startsWith("/") || path.startsWith("~"))) {
    problem = "The path part must be absolute (start with / or ~).";
  }
  return { types, path, problem };
}
