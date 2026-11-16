// "Install command line tool": puts a `pulse` link in ~/.local/bin pointing at the binary the
// app ships (the same `pulse`). Never overwrites a `pulse` that is not such a link.

import { lstat, mkdir, readlink, symlink, unlink } from "node:fs/promises";
import { delimiter, join } from "node:path";

export type InstallResult =
  | { kind: "installed"; link: string; target: string; onPath: boolean }
  | { kind: "exists"; link: string }
  | { kind: "failed"; message: string };

export async function installCli(binary: string, homedir: string, pathEnv: string | undefined): Promise<InstallResult> {
  const dir = join(homedir, ".local", "bin");
  const link = join(dir, "pulse");
  try {
    await mkdir(dir, { recursive: true });
    const existing = await lstat(link).catch(() => null);
    if (existing !== null) {
      if (!existing.isSymbolicLink()) return { kind: "exists", link };
      if ((await readlink(link)) !== binary) await unlink(link);
    }
    if ((await lstat(link).catch(() => null)) === null) await symlink(binary, link);
  } catch (e) {
    return { kind: "failed", message: (e as Error).message };
  }
  const onPath = (pathEnv ?? "").split(delimiter).some((p) => p === dir || p === `${dir}/`);
  return { kind: "installed", link, target: binary, onPath };
}
