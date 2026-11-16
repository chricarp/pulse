// Where things are: the Pulse home, the `pulse` binary, the shell's own data directory.

import { createHash } from "node:crypto";
import { delimiter, isAbsolute, join, resolve } from "node:path";

/** `$PULSE_HOME`, else `~/.pulse`; relative paths resolve against the working directory, like
 * the CLI does. */
export function pulseHome(env: NodeJS.ProcessEnv, homedir: string, cwd: string): string {
  const fromEnv = env.PULSE_HOME;
  if (fromEnv !== undefined && fromEnv !== "") return isAbsolute(fromEnv) ? fromEnv : resolve(cwd, fromEnv);
  return join(homedir, ".pulse");
}

/**
 * The shell's data directory. A development instance with its own `PULSE_HOME` gets its own
 * directory, so it has its own window state and its own single-instance lock and can run next to
 * the installed app.
 */
export function userDataFor(defaultUserData: string, home: string, homedir: string): string {
  if (home === join(homedir, ".pulse")) return defaultUserData;
  const id = createHash("sha256").update(home).digest("hex").slice(0, 12);
  return join(defaultUserData, "homes", id);
}

export interface BinaryContext {
  env: NodeJS.ProcessEnv;
  packaged: boolean;
  /** `process.resourcesPath` */
  resourcesPath: string;
  /** `apps/desktop`, in development. */
  appPath: string;
  exists: (path: string) => boolean;
}

/** The `pulse` binary to run: `PULSE_BIN`, the one bundled with the app, or a workspace build. */
export function bundledBinary(ctx: BinaryContext): string | null {
  const override = ctx.env.PULSE_BIN;
  if (override !== undefined && override !== "") return override;
  if (ctx.packaged) return join(ctx.resourcesPath, "bin", "pulse");
  for (const profile of ["release", "debug"]) {
    const p = join(ctx.appPath, "..", "..", "target", profile, "pulse");
    if (ctx.exists(p)) return p;
  }
  return null;
}

/**
 * An AppImage is mounted under a temporary directory only while the app runs. A daemon started
 * from inside it would lose its own executable when the app quits, so the binary is copied out
 * first, to a directory that stays.
 */
export function stableBinaryPath(binary: string, env: NodeJS.ProcessEnv, userData: string, version: string): string | null {
  const appDir = env.APPDIR;
  if (env.APPIMAGE === undefined || appDir === undefined || appDir === "") return null;
  if (!binary.startsWith(appDir.endsWith("/") ? appDir : `${appDir}/`)) return null;
  return join(userData, "bin", `pulse-${version.replace(/[^0-9A-Za-z.+-]/g, "_")}`);
}

const PATH_MARK = "__PULSE_PATH__";

/** The command that prints a login shell's PATH between markers (rc files may print too). */
export function loginShellCommand(shell: string): { file: string; args: string[] } {
  return { file: shell, args: ["-ilc", `printf '${PATH_MARK}%s${PATH_MARK}' "$PATH"`] };
}

/**
 * The PATH a daemon started from the app should get. Apps launched from the Dock or a desktop
 * entry inherit a minimal PATH (on macOS `/usr/bin:/bin:/usr/sbin:/sbin`), so agents installed
 * with Homebrew or into `~/.local/bin` would not be found. The login shell's entries come first;
 * entries only the app has are kept after them.
 */
export function mergedPath(current: string | undefined, shellOutput: string | null): string | undefined {
  if (shellOutput === null) return current;
  const start = shellOutput.indexOf(PATH_MARK);
  const end = shellOutput.indexOf(PATH_MARK, start + PATH_MARK.length);
  if (start === -1 || end === -1) return current;
  const fromShell = shellOutput.slice(start + PATH_MARK.length, end).split(delimiter);
  const all = [...fromShell, ...(current ?? "").split(delimiter)].filter((p) => p !== "" && isAbsolute(p));
  return [...new Set(all)].join(delimiter);
}
