import { describe, expect, it } from "vitest";

import { failureDetail } from "./daemon.ts";

describe("failureDetail", () => {
  it("reads the message of a --json failure from stdout", () => {
    const stdout = JSON.stringify({ error: { kind: "not_running", message: "Pulse is not running." } });
    expect(failureDetail({ stdout, stderr: "" })).toBe("Pulse is not running.");
  });

  it("falls back to text, stderr first", () => {
    expect(failureDetail({ stdout: "", stderr: "cannot bind the port" })).toBe("cannot bind the port");
    expect(failureDetail({ stdout: "plain words", stderr: "" })).toBe("plain words");
    expect(failureDetail({ stdout: '{"pid": 1}', stderr: "odd" })).toBe("odd");
  });
});
