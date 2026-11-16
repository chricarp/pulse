// Formatting for machine data. Everything here is pure; `now` is a parameter.

// Characters that change or hide how the text around them reads without showing themselves:
// bidi controls, zero-width and other default-ignorable characters, invisible fillers, line and
// paragraph separators, interlinear annotation, and the tag characters (U+E0000 to U+E007F,
// invisible on screen but read by models) with the rest of that block. Not U+FE00 to U+FE0F,
// which emoji need. The same list is in `pulse-core/src/prompt.rs` (`is_invisible`, also used
// by the CLI) and `apps/desktop/src/text.ts`; their tests run over the same cases.
// oxlint-disable-next-line no-misleading-character-class -- U+034F is matched on its own on purpose
const BIDI_AND_INVISIBLE = /[\u00AD\u034F\u061C\u115F\u1160\u17B4\u17B5\u180E\u200B-\u200F\u2028-\u202E\u2060-\u206F\u3164\uFEFF\uFFA0\uFFF0-\uFFFB\u{1BCA0}-\u{1BCA3}\u{1D173}-\u{1D17A}\u{E0000}-\u{E0FFF}]/gu;
// C0 and C1 controls except tab and newline.
// oxlint-disable-next-line no-control-regex -- matching control characters is the point
const CONTROL = /[\u0000-\u0008\u000B-\u001F\u007F-\u009F]/g;

function codepoint(ch: string): string {
  return `‹U+${(ch.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, "0")}›`;
}

/**
 * Untrusted text made visible: bidi overrides, zero-width and control characters are shown as
 * `‹U+202E›` instead of silently reordering or hiding what is around them. A file called
 * `invoice\u202Efdp.exe` (with a right-to-left override) must not read as `invoiceexe.pdf`.
 */
export function visible(text: string, opts: { singleLine?: boolean } = {}): string {
  let out = text.replace(BIDI_AND_INVISIBLE, codepoint).replace(CONTROL, codepoint);
  if (opts.singleLine) out = out.replace(/\r?\n/g, " ⏎ ").replace(/\t/g, " ");
  return out;
}

export function hasHiddenCharacters(text: string): boolean {
  BIDI_AND_INVISIBLE.lastIndex = 0;
  CONTROL.lastIndex = 0;
  const found = BIDI_AND_INVISIBLE.test(text) || CONTROL.test(text);
  BIDI_AND_INVISIBLE.lastIndex = 0;
  CONTROL.lastIndex = 0;
  return found;
}

const pad = (n: number, w = 2) => String(n).padStart(w, "0");

export function clockTime(iso: string, withMs = false): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const base = `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  return withMs ? `${base}.${pad(d.getMilliseconds(), 3)}` : base;
}

export function dateTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${clockTime(iso)}`;
}

/** "today 09:41:07", "yesterday 18:02:11" or a date. */
export function dayAndTime(iso: string, now: number): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  const t = d.getTime();
  if (t >= start.getTime()) return clockTime(iso);
  if (t >= start.getTime() - 86_400_000) return `yesterday ${clockTime(iso)}`;
  return dateTime(iso);
}

/** Compact span: 820ms, 4.2s, 3m 05s, 2h 10m, 3d 4h. */
export function span(ms: number): string {
  if (!Number.isFinite(ms)) return "–";
  const neg = ms < 0;
  const a = Math.abs(ms);
  let s: string;
  if (a < 1000) s = `${Math.round(a)}ms`;
  else if (a < 10_000) s = `${(a / 1000).toFixed(1)}s`;
  else if (a < 60_000) s = `${Math.floor(a / 1000)}s`;
  else if (a < 3_600_000) s = `${Math.floor(a / 60_000)}m ${pad(Math.floor((a % 60_000) / 1000))}s`;
  else if (a < 86_400_000) s = `${Math.floor(a / 3_600_000)}h ${pad(Math.floor((a % 3_600_000) / 60_000))}m`;
  else s = `${Math.floor(a / 86_400_000)}d ${Math.floor((a % 86_400_000) / 3_600_000)}h`;
  return neg ? `-${s}` : s;
}

/** "3s ago", "in 4m 10s". Coarse past a minute, so a list does not need to tick every second. */
export function relative(iso: string, now: number): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const d = t - now;
  const a = Math.abs(d);
  let s: string;
  if (a < 5_000) return d <= 0 ? "just now" : "now";
  if (a < 60_000) s = `${Math.floor(a / 1000)}s`;
  else if (a < 3_600_000) s = `${Math.floor(a / 60_000)}m`;
  else if (a < 86_400_000) s = `${Math.floor(a / 3_600_000)}h ${Math.floor((a % 3_600_000) / 60_000)}m`;
  else s = `${Math.floor(a / 86_400_000)}d`;
  return d < 0 ? `${s} ago` : `in ${s}`;
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}

const nf = new Intl.NumberFormat("en-US");
export function count(n: number): string {
  return nf.format(n);
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${count(n)} ${n === 1 ? one : many}`;
}

/**
 * Where to split a long string for middle truncation: keep the last path segment (or the last
 * `tail` characters) whole, so `…/Architettura/lecture-07.pdf` stays readable.
 */
export function splitForMiddle(text: string, tail = 18): [string, string] {
  if (text.length <= tail + 4) return [text, ""];
  const slash = text.lastIndexOf("/");
  const segment = slash >= 0 ? text.length - slash : 0;
  const keep = segment > 0 && segment <= 48 ? Math.max(segment, Math.min(tail, text.length - 4)) : tail;
  return [text.slice(0, text.length - keep), text.slice(text.length - keep)];
}

/** `pulse://fs/university` → `university`. */
export function sourceName(uri: string): string {
  const m = /^pulse:\/\/[^/]+\/(.+)$/.exec(uri);
  return m?.[1] ?? uri;
}

export function sourceKind(uri: string): string {
  const m = /^pulse:\/\/([^/]+)\//.exec(uri);
  return m?.[1] ?? "";
}

export function shortId(id: string): string {
  return id.length > 10 ? id.slice(-8) : id;
}

/** Every path under the home directory inside a message, as `~/…` (a source's error text). */
export function tildifyText(text: string, home: string | null | undefined): string {
  if (!home || home === "/") return text;
  const h = home.replace(/\/+$/, "");
  return text.split(`${h}/`).join("~/");
}

/** `/home/ada/University/x.pdf` as `~/University/x.pdf`, given the daemon's `user_home`. */
export function tildify(path: string, home: string | null | undefined): string {
  if (!home || home === "/") return path;
  const h = home.replace(/\/+$/, "");
  if (path === h) return "~";
  return path.startsWith(`${h}/`) ? `~${path.slice(h.length)}` : path;
}
