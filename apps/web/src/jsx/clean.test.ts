import { describe, expect, it } from "vitest";

import { cleanProps } from "./clean";

const RLO = "‮";
const TAG_I = String.fromCodePoint(0xe0049);

describe("JSX text", () => {
  it("is made visible in DOM elements' children and text attributes", () => {
    const props = {
      title: `run ${RLO}x`,
      "aria-label": `a${TAG_I}`,
      className: `keep${RLO}`,
      children: [`argv ${RLO}fdp.exe`, 7, [`nested${TAG_I}`, null], { type: "span" }],
    };
    expect(cleanProps("code", props)).toEqual({
      title: "run ‹U+202E›x",
      "aria-label": "a‹U+E0049›",
      className: `keep${RLO}`,
      children: ["argv ‹U+202E›fdp.exe", 7, ["nested‹U+E0049›", null], { type: "span" }],
    });
  });

  it("leaves components, edited values and clean text alone", () => {
    const toComponent = { text: `a${RLO}`, children: `b${RLO}` };
    expect(cleanProps(() => null, toComponent)).toBe(toComponent);
    const input = { value: `typed${RLO}`, defaultValue: `x${RLO}` };
    expect(cleanProps("input", input)).toBe(input);
    const clean = { title: "fine", children: ["é ❤️", 1] };
    expect(cleanProps("div", clean)).toBe(clean);
  });
});
