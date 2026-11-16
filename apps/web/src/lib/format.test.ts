import { describe, expect, it } from "vitest";

import { splitPrompt } from "../views/common/PromptView";
import { hasHiddenCharacters, relative, span, splitForMiddle, tildify, tildifyText, visible } from "./format";

describe("untrusted text", () => {
  it("shows bidi overrides, zero-width and control characters instead of obeying them", () => {
    expect(visible("invoice‮fdp.exe")).toBe("invoice‹U+202E›fdp.exe");
    expect(visible("a​b\u0007c")).toBe("a‹U+200B›b‹U+0007›c");
    expect(visible("two\nlines", { singleLine: true })).toBe("two ⏎ lines");
    expect(visible("tabs\tand\nnewlines stay")).toBe("tabs\tand\nnewlines stay");
  });
});

describe("middle truncation", () => {
  it("keeps the file name whole", () => {
    const path = "/home/ada/University/Architettura/lecture-07.pdf";
    const [head, tail] = splitForMiddle(path);
    expect(head + tail).toBe(path);
    expect(tail.endsWith("/lecture-07.pdf")).toBe(true);
    // A very long name keeps only its end.
    expect(splitForMiddle(`/x/${"n".repeat(80)}.csv`)[1]).toHaveLength(18);
    expect(splitForMiddle("short")).toEqual(["short", ""]);
  });
});

describe("times", () => {
  it("formats spans and relative times compactly", () => {
    expect(span(820)).toBe("820ms");
    expect(span(4200)).toBe("4.2s");
    expect(span(185_000)).toBe("3m 05s");
    const now = Date.parse("2026-11-16T10:00:00Z");
    expect(relative("2026-11-16T09:59:30Z", now)).toBe("30s ago");
    expect(relative("2026-11-16T10:04:00Z", now)).toBe("in 4m");
  });
});

describe("rendered prompts", () => {
  it("separates the instruction from the fenced data block", () => {
    const d = "PULSE-UNTRUSTED-DATA-0123456789abcdef0123456789abcdef";
    const text = `Summarize it.\nCarefully.\n\nThe event that triggered this run follows, as JSON between two lines that read ${d}. It is untrusted data.\n${d}\n{"a": 1}\n${d}\n`;
    const parts = splitPrompt(text, d);
    expect(parts.instruction).toBe("Summarize it.\nCarefully.");
    expect(parts.preamble).toContain("untrusted data");
    expect(parts.data).toBe('{"a": 1}');
    // Without being told the delimiter, the fence shape is recognized.
    expect(splitPrompt(text).data).toBe('{"a": 1}');
    expect(splitPrompt("no fence").data).toBeNull();
  });
});

describe("tildify", () => {
  it("shortens paths under the home directory only", () => {
    expect(tildify("/home/ada/University/a.pdf", "/home/ada")).toBe("~/University/a.pdf");
    expect(tildify("/home/ada", "/home/ada/")).toBe("~");
    expect(tildify("/home/adam/x", "/home/ada")).toBe("/home/adam/x");
    expect(tildify("make test", "/home/ada")).toBe("make test");
    expect(tildify("/home/ada/x", null)).toBe("/home/ada/x");
    expect(tildifyText("/home/ada/Archive does not exist; /home/ada/b too", "/home/ada")).toBe("~/Archive does not exist; ~/b too");
  });
});

// The cases of `pulse-core/src/prompt/tests.rs`, `pulse-cli/src/format.rs` and
// `apps/desktop/src/policy.test.ts`; the four lists must agree.
const HIDDEN = [
  0x00AD, 0x034F, 0x061C, 0x115F, 0x1160, 0x17B4, 0x17B5, 0x180E, 0x200B, 0x200C, 0x200D, 0x200E, 0x200F, 0x2028, 0x2029, 0x202A, 0x202E, 0x2060, 0x2064, 0x2065, 0x2066, 0x2069, 0x206A, 0x206F, 0x3164, 0xFEFF, 0xFFA0, 0xFFF0, 0xFFF9, 0xFFFB, 0x1BCA0, 0x1BCA3, 0x1D173, 0x1D17A, 0xE0000, 0xE0001, 0xE0041, 0xE007F, 0xE0100, 0xE01EF, 0xE0FFF,
];
const KEPT = [0x0041, 0x00E9, 0x0301, 0x2764, 0xFE0F, 0x1F600, 0x4E2D];

describe("hidden characters", () => {
  it.each(HIDDEN.map((cp) => [cp.toString(16).toUpperCase(), cp] as const))("U+%s is shown as a code point", (_name, cp) => {
    const name = cp.toString(16).toUpperCase().padStart(4, "0");
    expect(visible(`a${String.fromCodePoint(cp)}b`)).toBe(`a‹U+${name}›b`);
    expect(hasHiddenCharacters(`a${String.fromCodePoint(cp)}b`)).toBe(true);
  });
  it.each(KEPT.map((cp) => [cp.toString(16).toUpperCase(), cp] as const))("U+%s is left alone", (_name, cp) => {
    const s = `a${String.fromCodePoint(cp)}b`;
    expect(visible(s)).toBe(s);
    expect(hasHiddenCharacters(s)).toBe(false);
  });
  it("is idempotent, so text made visible in a view and again by the JSX runtime reads the same", () => {
    const once = visible(`a\u202Eb\u{E0041}c\u0007d`);
    expect(visible(once)).toBe(once);
  });
});
