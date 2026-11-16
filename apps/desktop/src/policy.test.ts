// URL, text, version, settings and path policies: small pure functions, tested as tables.

import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import { readDaemonInfo } from "./daemon.ts";
import { statusPage } from "./page.ts";
import { bundledBinary, loginShellCommand, mergedPath, pulseHome, stableBinaryPath, userDataFor } from "./paths.ts";
import { closeHidesToTray, fitBounds, parseSettings } from "./settings.ts";
import { menuLabel, notificationText, plainText } from "./text.ts";
import { externalUrl, isDaemonUrl, loginCodeFrom, loginUrl, redact, routePath } from "./urls.ts";
import { compareVersions, versionAdvice } from "./version.ts";

const origin = "http://127.0.0.1:7477";

describe("navigation", () => {
  it.each([
    ["http://127.0.0.1:7477/", true],
    ["http://127.0.0.1:7477/runs/01J?x=1#login=1.ab", true],
    ["http://localhost:7477/", false], // one origin only
    ["http://127.0.0.1:7478/", false],
    ["https://127.0.0.1:7477/", false],
    ["http://127.0.0.1:7477.evil.example/", false],
    ["http://evil.example/?u=http://127.0.0.1:7477/", false],
    ["file:///etc/passwd", false],
    ["data:text/html,hi", false],
    ["javascript:alert(1)", false],
    ["not a url", false],
  ])("isDaemonUrl(%s) = %s", (url, ok) => {
    expect(isDaemonUrl(url, origin)).toBe(ok);
  });

  it("nothing is the daemon's while there is no daemon", () => {
    expect(isDaemonUrl("http://127.0.0.1:7477/", null)).toBe(false);
  });

  it.each([
    ["https://github.com/pulse-dev/pulse", "https://github.com/pulse-dev/pulse"],
    ["http://example.com/a b", "http://example.com/a%20b"],
    ["file:///etc/passwd", null],
    ["javascript:alert(1)", null],
    ["vscode://file/etc/passwd", null],
    ["ms-msdt:/id", null],
    ["smb://host/share", null],
    ["https://user:pass@evil.example/", null],
    ["https://bank.example@evil.example/", null],
    [`https://example.com/${"x".repeat(3000)}`, null],
  ])("externalUrl(%s) = %s", (url, out) => {
    expect(externalUrl(url)).toBe(out);
  });

  it("routes and the login fragment", () => {
    expect(routePath({ kind: "run", id: "01J/../x" })).toBe("/runs/01J%2F..%2Fx");
    expect(routePath({ kind: "rule", name: "proj/lint" })).toBe("/rules/proj%2Flint");
    expect(loginUrl(origin, { kind: "approvals" }, "123.abc")).toBe(`${origin}/runs?show=approval#login=123.abc`);
    expect(loginCodeFrom(`${origin}/#login=1763.0f9a`, origin)).toBe("1763.0f9a");
    expect(loginCodeFrom("http://127.0.0.1:9999/#login=1763.0f9a", origin)).toBeNull();
    expect(loginCodeFrom(`${origin}/#login=<script>`, origin)).toBeNull();
    expect(redact(`${origin}/runs/1?q=2#login=123.abc`)).toBe(`${origin}/runs/1`);
  });
});

describe("text", () => {
  it.each([
    ["a\nb\tc", 20, "a b c"],
    ["  spaced   out  ", 20, "spaced out"],
    ["evil‮fdp.exe", 20, "evilfdp.exe"],
    ["zero​width", 20, "zerowidth"],
    ["abcdefghij", 5, "abcd…"],
    ["😀😀😀😀😀😀", 3, "😀😀…"],
    [null, 5, ""],
  ])("plainText(%j, %i)", (input, max, out) => {
    expect(plainText(input, max)).toBe(out);
  });

  it("escapes markup in Linux notification bodies only", () => {
    expect(notificationText("<b>x</b> & y", 50, "linux")).toBe("&lt;b&gt;x&lt;/b&gt; &amp; y");
    expect(notificationText("<b>x</b> & y", 50, "darwin")).toBe("<b>x</b> & y");
  });

  it("doubles & in menu labels", () => {
    expect(menuLabel("R&D")).toBe("R&&D");
  });

  it("the status page escapes and allows no script", () => {
    const page = decodeURIComponent(statusPage("<x>", ["</p><script>alert(1)</script>"]));
    expect(page).toContain("&lt;/p&gt;&lt;script&gt;");
    expect(page).not.toContain("<script>");
    expect(page).toContain("default-src 'none'");
  });
});

describe("versions", () => {
  it.each([
    ["0.1.0", "0.1.0", 0],
    ["0.1.0", "0.2.0", -1],
    ["1.10.0", "1.9.9", 1],
    ["pulse 0.1.0", "0.1.0", 0],
    ["0.2.0-beta.1", "0.2.0", 0],
    ["dev", "0.1.0", null],
  ])("compare(%s, %s) = %s", (a, b, out) => {
    expect(compareVersions(a, b)).toBe(out);
  });

  it("advice", () => {
    expect(versionAdvice("0.2.0", "0.1.0").kind).toBe("daemon_older");
    expect(versionAdvice("0.2.0", "0.3.0").kind).toBe("daemon_newer");
    expect(versionAdvice("0.2.0", "0.2.0").kind).toBe("same");
  });
});

describe("settings", () => {
  it("falls back field by field", () => {
    expect(parseSettings({ notifications: false, closeToTray: "sometimes", window: { x: 1, y: 2, width: "wide" } })).toEqual({
      window: null,
      notifications: false,
      closeToTray: "auto",
    });
    expect(parseSettings("garbage").notifications).toBe(true);
  });

  const primary = { x: 0, y: 0, width: 1920, height: 1080 };
  it.each([
    ["saved place kept", { x: 100, y: 100, width: 900, height: 700 }, [primary], { x: 100, y: 100, width: 900, height: 700 }],
    ["on an unplugged monitor: centred", { x: 3000, y: 100, width: 900, height: 700 }, [primary], { x: 410, y: 160, width: 1100, height: 760 }],
    ["too big: shrunk", { x: 0, y: 0, width: 4000, height: 3000 }, [primary], { x: 0, y: 0, width: 1920, height: 1080 }],
    ["hanging off the edge: pulled in", { x: 1800, y: 900, width: 900, height: 700 }, [primary], { x: 1020, y: 380, width: 900, height: 700 }],
    ["nothing saved", null, [primary], { x: 410, y: 160, width: 1100, height: 760 }],
  ])("fitBounds: %s", (_n, saved, areas, out) => {
    expect(fitBounds(saved, areas, primary)).toEqual(out);
  });

  it.each([
    ["darwin", "auto", undefined, true],
    ["linux", "auto", "KDE", true],
    ["linux", "auto", "ubuntu:GNOME", true],
    ["linux", "auto", "GNOME", false],
    ["linux", "auto", undefined, false],
    ["linux", "always", "GNOME", true],
    ["linux", "never", "KDE", false],
  ] as const)("closeHidesToTray(%s, %s, %s) = %s", (platform, setting, desktop, out) => {
    expect(closeHidesToTray(platform, setting, desktop)).toBe(out);
  });
});

describe("paths", () => {
  it("the Pulse home", () => {
    expect(pulseHome({}, "/home/u", "/cwd")).toBe("/home/u/.pulse");
    expect(pulseHome({ PULSE_HOME: "dev-home" }, "/home/u", "/cwd")).toBe("/cwd/dev-home");
    expect(pulseHome({ PULSE_HOME: "/tmp/h" }, "/home/u", "/cwd")).toBe("/tmp/h");
  });

  it("a development home gets its own data directory", () => {
    expect(userDataFor("/cfg/Pulse", "/home/u/.pulse", "/home/u")).toBe("/cfg/Pulse");
    expect(userDataFor("/cfg/Pulse", "/tmp/h", "/home/u")).toMatch(/^\/cfg\/Pulse\/homes\/[0-9a-f]{12}$/);
  });

  it("which binary", () => {
    const ctx = { env: {}, packaged: true, resourcesPath: "/app/resources", appPath: "/repo/apps/desktop", exists: () => false };
    expect(bundledBinary(ctx)).toBe("/app/resources/bin/pulse");
    expect(bundledBinary({ ...ctx, env: { PULSE_BIN: "/x/pulse" } })).toBe("/x/pulse");
    expect(bundledBinary({ ...ctx, packaged: false, exists: (p: string) => p.includes("debug") })).toBe("/repo/target/debug/pulse");
    expect(bundledBinary({ ...ctx, packaged: false })).toBeNull();
  });

  it("copies the binary out of an AppImage, and only then", () => {
    const env = { APPIMAGE: "/home/u/Pulse.AppImage", APPDIR: "/tmp/.mount_PulseX" };
    expect(stableBinaryPath("/tmp/.mount_PulseX/resources/bin/pulse", env, "/cfg", "0.1.0")).toBe("/cfg/bin/pulse-0.1.0");
    expect(stableBinaryPath("/tmp/.mount_PulseXY/resources/bin/pulse", env, "/cfg", "0.1.0")).toBeNull();
    expect(stableBinaryPath("/opt/Pulse/resources/bin/pulse", {}, "/cfg", "0.1.0")).toBeNull();
  });

  it("the daemon gets the login shell's PATH first", () => {
    const { args } = loginShellCommand("/bin/zsh");
    expect(args[0]).toBe("-ilc");
    const out = "motd noise\n__PULSE_PATH__/opt/homebrew/bin:/usr/bin:relative__PULSE_PATH__";
    expect(mergedPath("/usr/bin:/bin", out)).toBe("/opt/homebrew/bin:/usr/bin:/bin");
    expect(mergedPath("/usr/bin", null)).toBe("/usr/bin");
    expect(mergedPath("/usr/bin", "no markers")).toBe("/usr/bin");
  });

  it("reads daemon.json, and treats anything odd as no daemon", async () => {
    const dir = await mkdtemp(join(tmpdir(), "pulse-desktop-"));
    expect(await readDaemonInfo(dir)).toBeNull();
    await writeFile(join(dir, "daemon.json"), '{"pid":1,"port":7477,"version":"0.1.0","started_at":"x"}');
    expect(await readDaemonInfo(dir)).toEqual({ pid: 1, port: 7477, version: "0.1.0", started_at: "x" });
    for (const bad of ['{"pid":1,"port":0,"version":"0.1.0","started_at":"x"}', '{"pid":1', "[]"]) {
      await writeFile(join(dir, "daemon.json"), bad);
      expect(await readDaemonInfo(dir)).toBeNull();
    }
  });
});
