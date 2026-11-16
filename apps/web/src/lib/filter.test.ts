import { describe, expect, it } from "vitest";

import {
  EMPTY_FILTER,
  applyQuery,
  filterProblems,
  fromSearchParams,
  isLiveCapable,
  toEventFilter,
  toLiveFilter,
  toSearchParams,
  type FeedFilter,
} from "./filter";

const now = Date.parse("2026-11-16T10:00:00.000Z");

describe("feed filter", () => {
  it("maps to the daemon's EventFilter, resolving relative times against now", () => {
    const f: FeedFilter = {
      ...EMPTY_FILTER,
      types: ["fs.*"],
      sources: ["university"],
      path: " ~/University ",
      text: "lecture",
      since: "1h",
    };
    expect(toEventFilter(f, now)).toEqual({
      types: ["fs.*"],
      sources: ["university"],
      path: "~/University",
      text: "lecture",
      since: "2026-11-16T09:00:00.000Z",
    });
    expect(toEventFilter(EMPTY_FILTER, now)).toEqual({ types: [], sources: [] });
  });

  it("normalizes typed absolute times to RFC 3339", () => {
    const f = { ...EMPTY_FILTER, since: "2026-11-16T08:00:00Z", until: "2026-11-16T09:30:00+01:00" };
    expect(toEventFilter(f, now)).toMatchObject({
      since: "2026-11-16T08:00:00.000Z",
      until: "2026-11-16T08:30:00.000Z",
    });
  });

  it("drops time bounds for the live subscription and has no live tail with an upper bound", () => {
    const f = { ...EMPTY_FILTER, types: ["git.*"], since: "24h" };
    expect(toLiveFilter(f)).toEqual({ types: ["git.*"], sources: [] });
    expect(isLiveCapable(f)).toBe(true);
    expect(isLiveCapable({ ...f, until: "2026-11-16T09:00:00Z" })).toBe(false);
  });

  it("round-trips through the URL", () => {
    const f: FeedFilter = {
      types: ["fs.file.created", "git.*"],
      sources: ["a", "b"],
      subject: "/x/**",
      path: "/home/me",
      text: "two words",
      since: "7d",
      until: null,
      causedBy: "01JD0000000000000000000000",
    };
    expect(fromSearchParams(new URLSearchParams(toSearchParams(f).toString()))).toEqual(f);
    expect(fromSearchParams(new URLSearchParams(""))).toEqual(EMPTY_FILTER);
  });

  it("parses a typed query into fields, keeping the rest as text", () => {
    const f = applyQuery(
      { ...EMPTY_FILTER, types: ["fs.*"] },
      'type:git.* type:fs.* source:university path:"~/My Notes" since:1h failing http://x',
    );
    expect(f.types).toEqual(["fs.*", "git.*"]);
    expect(f.sources).toEqual(["university"]);
    expect(f.path).toBe("~/My Notes");
    expect(f.since).toBe("1h");
    expect(f.text).toBe("failing http://x");
    expect(applyQuery(EMPTY_FILTER, "type: lonely").text).toBe("type: lonely");
  });

  it("explains problems before the daemon has to", () => {
    expect(filterProblems({ ...EMPTY_FILTER, path: "relative/dir" })).toHaveLength(1);
    expect(filterProblems({ ...EMPTY_FILTER, since: "yesterdayish" })).toHaveLength(1);
    expect(filterProblems({ ...EMPTY_FILTER, path: "~/ok", since: "15m" })).toEqual([]);
  });
});
