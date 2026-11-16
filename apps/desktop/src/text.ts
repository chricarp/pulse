// Turning untrusted strings (rule names from project files, event subjects, error output) into
// something safe to put in a menu, a notification or a status page.

// C0 and C1 controls, including newlines and tabs: everything becomes one line.
const CONTROL = /[\u0000-\u001f\u007f-\u009f]+/g;
// Invisible formatting: soft hyphen, bidi marks and overrides, zero-width characters, word
// joiners, BOM, interlinear annotations. A bidi override in a file name can make a notification
// read differently from what the file is called.
const INVISIBLE = /[­؜᠎​-‏‪-‮⁠-⁤⁦-⁯﻿￹-￻]/g;

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
