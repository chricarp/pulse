import { describe, expect, it } from "vitest";

import { parseLoginFragment } from "./login";

describe("login fragment", () => {
  it("takes the code and keeps the rest of the fragment", () => {
    expect(parseLoginFragment("#login=123.abc")).toEqual({ code: "123.abc", rest: "" });
    expect(parseLoginFragment("#login=123.abc&x=1")).toEqual({ code: "123.abc", rest: "#x=1" });
    expect(parseLoginFragment("#x=1")).toEqual({ code: null, rest: "#x=1" });
    expect(parseLoginFragment("")).toEqual({ code: null, rest: "" });
    expect(parseLoginFragment("#login=")).toEqual({ code: null, rest: "" });
  });
});
