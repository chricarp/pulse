// Turning untrusted strings (rule names from project files, event subjects, error output) into
// something safe to put in a menu, a notification or a status page.

// C0 and C1 controls, including newlines and tabs: everything becomes one line.
const CONTROL = /[\u0000-\u001f\u007f-\u009f]+/g;
// Invisible formatting: bidi controls, zero-width and other default-ignorable characters,
// invisible fillers, line and paragraph separators, interlinear annotation, and the tag
// characters (U+E0000 to U+E007F, invisible but read by models) with the rest of that block. A
// bidi override in a file name can make a notification read differently from what the file is
// called. Not U+FE00 to U+FE0F, which emoji need. The same list is in
// `pulse-core/src/prompt.rs` (`is_invisible`) and `apps/web/src/lib/format.ts`; their tests run
// over the same cases.
const INVISIBLE =
  /[\u00AD\u034F\u061C\u115F\u1160\u17B4\u17B5\u180E\u200B-\u200F\u2028-\u202E\u2060-\u206F\u3164\uFEFF\uFFA0\uFFF0-\uFFFB\u{1BCA0}-\u{1BCA3}\u{1D173}-\u{1D17A}\u{E0000}-\u{E0FFF}]/gu;

/** `input` without invisible formatting characters; lines and length are kept. */
export function withoutInvisible(input: string): string {
  return input.replace(INVISIBLE, "");
}

/** One line of plain text, at most `max` characters (code points), with an ellipsis if cut. */
export function plainText(input: unknown, max: number): string {
  const s = (typeof input === "string" ? input : String(input ?? ""))
    .replace(INVISIBLE, "")
    .replace(CONTROL, " ")
    .replace(/\s+/g, " ")
    .trim();
  const chars = Array.from(s);
  if (chars.length <= max) return s;
  return `${chars.slice(0, Math.max(0, max - 1)).join("").trimEnd()}…`;
}

/**
 * Notification bodies. Linux notification servers may interpret a subset of HTML in the body
 * (`body-markup`), so there `&`, `<` and `>` are escaped; macOS shows text as is.
 */
export function notificationText(input: unknown, max: number, platform: NodeJS.Platform): string {
  const s = plainText(input, max);
  if (platform !== "linux") return s;
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** Menu labels: `&` marks a mnemonic in Electron menus on Linux and Windows, so it is doubled. */
export function menuLabel(input: unknown, max = 48): string {
  return plainText(input, max).replace(/&/g, "&&");
}

/** Text for an HTML page: escaped, newlines kept. */
export function escapeHtml(input: string): string {
  return input
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
